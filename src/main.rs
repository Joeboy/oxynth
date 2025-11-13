#![no_std]
#![no_main]

mod audio_out;
mod synth;
mod usb_midi_in;

use audio_out::audio_task;
use heapless::spsc::Queue;
use static_cell::StaticCell;
use synth::MIDI_QUEUE;
use usb_midi_in::usb_input_task;

use defmt::*;
use embassy_executor::Executor;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::multicore::{Stack, spawn_core1};
use {defmt_rtt as _, panic_probe as _};

// NB if you start seeing mysterious crashes, it could be that core1's stack isn't big enough
// for 2x BUFFER_SIZE u32 buffers + synth state etc.
static mut CORE1_STACK: Stack<16384> = Stack::new();
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());
    info!("Starting USB MIDI synth POC");
    let mut led = Output::new(p.PIN_25, Level::Low);
    led.set_high();

    // MIDI queue producer and consumer
    let queue = MIDI_QUEUE.init(Queue::new());
    let (prod, cons) = queue.split();

    // Realtime audio processing goes on core 1
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                spawner.spawn(unwrap!(audio_task(
                    p.PIO0, p.DMA_CH0, p.DMA_CH1, p.DMA_CH2, p.PIN_18, p.PIN_19, p.PIN_20, cons
                )))
            });
        },
    );

    // Anything non-realtime (currently USB MIDI input) goes on core 0
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| spawner.spawn(unwrap!(usb_input_task(p.USB, prod))));
}
