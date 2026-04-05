/// Set up i2s / pio / dma for continuous audio output, using a PIO program to
/// switch between two buffers / dma channels
use core::ops::ControlFlow;
use core::sync::atomic::{Ordering, compiler_fence};

use embassy_rp::Peri;
use embassy_rp::dma::ChannelInstance;
use embassy_rp::pac;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, Instance, LoadedProgram, PioPin, ShiftConfig,
    ShiftDirection, StateMachine,
};
use fixed::traits::ToFixed;

// This local driver intentionally targets the current oxynth routing: PIO0 + SM0.
const PIO_NO: u8 = 0;
const SM_INDEX: usize = 0;

/// This struct represents an i2s output driver program
///
/// The sample bit-depth is set through scratch register `Y`.
/// `Y` has to be set to sample bit-depth - 2.
/// (14 = 16bit, 22 = 24bit, 30 = 32bit)
pub struct PioI2sOutProgram<'d, PIO: Instance> {
    prg: LoadedProgram<'d, PIO>,
}

impl<'d, PIO: Instance> PioI2sOutProgram<'d, PIO> {
    /// Load the program into the given pio
    pub fn new(common: &mut Common<'d, PIO>) -> Self {
        let prg = pio::pio_asm!(
            ".side_set 2",
            "    mov x, y           side 0b01",
            "left_data:",
            "    out pins, 1        side 0b00",
            "    jmp x-- left_data  side 0b01",
            "    out pins, 1        side 0b10",
            "    mov x, y           side 0b11",
            "right_data:",
            "    out pins, 1         side 0b10",
            "    jmp x-- right_data side 0b11",
            "    out pins, 1         side 0b00",
        );

        let prg = common.load_program(&prg.program);

        Self { prg }
    }
}

pub struct PioI2sOut<'d> {
    _sm: StateMachine<'d, PIO0, SM_INDEX>,
}

impl<'d> PioI2sOut<'d> {
    /// Configure a state machine to output I2S.
    pub fn new(
        common: &mut Common<'d, PIO0>,
        mut sm: StateMachine<'d, PIO0, SM_INDEX>,
        data_pin: Peri<'d, impl PioPin>,
        bit_clock_pin: Peri<'d, impl PioPin>,
        lr_clock_pin: Peri<'d, impl PioPin>,
        sample_rate: u32,
        bit_depth: u32,
        program: &PioI2sOutProgram<'d, PIO0>,
    ) -> Self {
        let data_pin = common.make_pio_pin(data_pin);
        let bit_clock_pin = common.make_pio_pin(bit_clock_pin);
        let left_right_clock_pin = common.make_pio_pin(lr_clock_pin);

        let cfg = {
            let mut cfg = Config::default();
            cfg.use_program(&program.prg, &[&bit_clock_pin, &left_right_clock_pin]);
            cfg.set_out_pins(&[&data_pin]);
            let clock_frequency = sample_rate * bit_depth * 2;
            cfg.clock_divider =
                (embassy_rp::clocks::clk_sys_freq() as f64 / clock_frequency as f64 / 2.)
                    .to_fixed();
            cfg.shift_out = ShiftConfig {
                threshold: 32,
                direction: ShiftDirection::Left,
                auto_fill: true,
            };
            // Join FIFOs to buy more time for refill work between DMA swaps.
            cfg.fifo_join = FifoJoin::TxOnly;
            cfg
        };
        sm.set_config(&cfg);
        sm.set_pin_dirs(
            Direction::Out,
            &[&data_pin, &left_right_clock_pin, &bit_clock_pin],
        );

        // The SM counts down to 0 and consumes one setup cycle, so Y = bit_depth - 2.
        unsafe { sm.set_y(bit_depth - 2) };

        sm.set_enable(true);

        Self { _sm: sm }
    }

    fn dreq() -> pac::dma::vals::TreqSel {
        pac::dma::vals::TreqSel::from(PIO_NO * 8 + SM_INDEX as u8)
    }

    fn tx_fifo_addr() -> u32 {
        pac::PIO0.txf(SM_INDEX).as_ptr() as u32
    }

    fn init_dma_channel(regs: pac::dma::Channel, chain_target: u8, buffer: &[u32]) {
        regs.read_addr().write_value(buffer.as_ptr() as u32);
        regs.write_addr().write_value(Self::tx_fifo_addr());

        regs.trans_count()
            .write(|w| w.set_count(buffer.len() as u32));

        // Use AL1_CTRL alias so we can configure without triggering the channel immediately.
        regs.al1_ctrl().write(|w| {
            w.set_treq_sel(Self::dreq());
            w.set_data_size(pac::dma::vals::DataSize::SizeWord);
            w.set_incr_read(true);
            w.set_incr_write(false);
            w.set_en(true);
            w.set_chain_to(chain_target);
        });
    }

    async fn wait_dma_idle<C: ChannelInstance>(_ch: &Peri<'_, C>) {
        while C::regs().ctrl_trig().read().busy() {
            embassy_futures::yield_now().await;
        }
    }

    /// Stream continuously using two DMA channels in ping-pong mode.
    ///
    /// Return `ControlFlow::Break(())` from `fill` to stop streaming.
    pub async fn stream_ping_pong<C1, C2, F>(
        &mut self,
        ch_ping: Peri<'d, C1>,
        ch_pong: Peri<'d, C2>,
        buf_a: &'d mut [u32],
        buf_b: &'d mut [u32],
        mut fill: F,
    ) where
        C1: ChannelInstance,
        C2: ChannelInstance,
        F: FnMut(&mut [u32]) -> ControlFlow<()>,
    {
        Self::init_dma_channel(C1::regs(), C2::number(), buf_a);
        Self::init_dma_channel(C2::regs(), C1::number(), buf_b);

        if let ControlFlow::Break(()) = fill(buf_a) {
            return;
        }

        compiler_fence(Ordering::SeqCst);
        // Trigger first channel. Following swaps are handled by DMA chaining.
        C1::regs().ctrl_trig().modify(|_| {});
        compiler_fence(Ordering::SeqCst);

        loop {
            if let ControlFlow::Break(()) = fill(buf_b) {
                break;
            }

            Self::wait_dma_idle(&ch_ping).await;
            C1::regs().read_addr().write_value(buf_a.as_ptr() as u32);

            if let ControlFlow::Break(()) = fill(buf_a) {
                break;
            }

            Self::wait_dma_idle(&ch_pong).await;
            C2::regs().read_addr().write_value(buf_b.as_ptr() as u32);
        }

        C1::regs().al1_ctrl().modify(|w| {
            w.set_en(false);
        });
        C2::regs().al1_ctrl().modify(|w| {
            w.set_en(false);
        });
    }
}
