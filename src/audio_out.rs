use core::ops::ControlFlow;

use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use embassy_rp::Peri;
use embassy_rp::peripherals::PIN_18;
use embassy_rp::peripherals::PIN_19;
use embassy_rp::peripherals::PIN_20;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

const SAMPLE_RATE: u32 = 48_000;
const BIT_DEPTH: u32 = 16;
const TONE_HZ: u32 = 540;

// Each u32 is one stereo frame: [left: i16 | right: i16].
// If channels end up swapped in your build, just swap the halves below.
#[inline]
fn pack_lr_16(l: i16, r: i16) -> u32 {
    ((l as u32 as u16 as u32) << 16) | ((r as u16) as u32)
}

const FRAMES_PER_HALF: u32 = if SAMPLE_RATE / (TONE_HZ * 2) > 1 {
    SAMPLE_RATE / (TONE_HZ * 2)
} else {
    1
};
static mut FRAME_IN_HALF: u32 = 0;
static mut HIGH: bool = true;

fn audio_callback(buf: &mut [u32]) -> ControlFlow<(), ()> {
    let (hi, lo) = (1024, -1024);
    for w in buf.iter_mut() {
        let s = unsafe { if HIGH { hi } else { lo } };
        *w = pack_lr_16(s, s); // mono → stereo
        unsafe {
            FRAME_IN_HALF += 1;
            if FRAME_IN_HALF >= FRAMES_PER_HALF {
                FRAME_IN_HALF = 0;
                HIGH = !HIGH;
            }
        }
    }
    ControlFlow::Continue(())
}

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

    let mut buf_a = [0u32; 512];
    let mut buf_b = [0u32; 512];

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
