//! This example shows generating audio and sending it to a connected i2s DAC using the PIO
//! module of the RP235x.
//!
//! Connect the i2s DAC as follows:
//!   bclk : GPIO 18 (black)
//!   lrc  : GPIO 19 (white)
//!   din  : GPIO 20 (orange)
//! Then hold down the boot select button to trigger a rising triangle waveform.

#![no_std]
#![no_main]

use core::mem;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};
bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

const SAMPLE_RATE: u32 = 48_000;
const BIT_DEPTH: u32 = 16;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("pio i2s example");
    let mut led = Output::new(p.PIN_25, Level::Low);
    led.set_high();

    // Setup pio state machine for i2s output
    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO0, Irqs);

    let bit_clock_pin = p.PIN_18;
    let left_right_clock_pin = p.PIN_19;
    let data_pin = p.PIN_20;

    let program = PioI2sOutProgram::new(&mut common);
    let mut i2s = PioI2sOut::new(
        &mut common,
        sm0,
        p.DMA_CH0,
        data_pin,
        bit_clock_pin,
        left_right_clock_pin,
        SAMPLE_RATE,
        BIT_DEPTH,
        &program,
    );

    // let fade_input = Input::new(p.PIN_0, Pull::Up);

    // create two audio buffers (back and front) which will take turns being
    // filled with new audio data and being sent to the pio fifo using dma
    const BUFFER_SIZE: usize = 960;
    static DMA_BUFFER: StaticCell<[u32; BUFFER_SIZE * 2]> = StaticCell::new();
    let dma_buffer = DMA_BUFFER.init_with(|| [0u32; BUFFER_SIZE * 2]);
    let (mut back_buffer, mut front_buffer) = dma_buffer.split_at_mut(BUFFER_SIZE);

    // start pio state machine
    let mut phase: u32 = 0;
    const FREQ: u32 = 440;
    const PHASE_INC: u32 = (FREQ * 0x10000) / SAMPLE_RATE; // 16-bit phase accumulator

    info!("Starting audio loop");
    let mut led_status = false;
    loop {
        if led_status {
            led.set_high();
        } else {
            led.set_low();
        }
        led_status = !led_status;

        let dma_future = i2s.write(front_buffer);

        // fill back buffer with a 440Hz square wave
        for s in back_buffer.iter_mut() {
            phase = (phase + PHASE_INC) & 0xffff;
            let sample = if phase < 0x8000 {
                0x7fff // high
            } else {
                0x8001 // low
            };
            // duplicate mono sample into lower and upper half of dma word
            *s = (sample as u16 as u32) * 0x10001;
        }

        dma_future.await;
        mem::swap(&mut back_buffer, &mut front_buffer);
    }
}
