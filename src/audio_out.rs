use crate::synth::audio_callback;
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::PIN_18;
use embassy_rp::peripherals::PIN_19;
use embassy_rp::peripherals::PIN_20;
use embassy_rp::peripherals::PIO0;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

const SAMPLE_RATE: u32 = 48_000;
const BIT_DEPTH: u32 = 16;
const BUFFER_SIZE: usize = 512;

#[embassy_executor::task]
pub async fn audio_task(
    pio0: Peri<'static, PIO0>,
    dma_ch0: Peri<'static, DMA_CH0>,
    dma_ch1: Peri<'static, DMA_CH1>,
    dma_ch2: Peri<'static, DMA_CH2>,
    pin18: Peri<'static, PIN_18>,
    pin19: Peri<'static, PIN_19>,
    pin20: Peri<'static, PIN_20>,
) {
    let Pio {
        mut common, sm0, ..
    } = Pio::new(pio0, Irqs);

    let bit_clock_pin = pin18;
    let left_right_clock_pin = pin19;
    let data_pin = pin20;

    let program = PioI2sOutProgram::new(&mut common);

    let mut buf_a = [0u32; BUFFER_SIZE];
    let mut buf_b = [0u32; BUFFER_SIZE];

    let mut i2s = PioI2sOut::new(
        &mut common,
        sm0,
        dma_ch2, // <- use any different DMA channel than the ones used below
        data_pin,
        bit_clock_pin,
        left_right_clock_pin,
        SAMPLE_RATE,
        BIT_DEPTH,
        &program,
    );

    i2s.stream_ping_pong(dma_ch0, dma_ch1, &mut buf_a, &mut buf_b, audio_callback)
        .await;
}
