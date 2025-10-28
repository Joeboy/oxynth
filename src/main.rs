#![no_std]
#![no_main]

mod audio_out;

use audio_out::audio_task;
use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Audio out example");
    let mut led = Output::new(p.PIN_25, Level::Low);
    led.set_high();

    spawner.spawn(
        audio_task(
            p.PIO0, p.DMA_CH0, p.DMA_CH1, p.DMA_CH2, p.PIN_18, p.PIN_19, p.PIN_20,
        )
        .unwrap(),
    );

    info!("Done");
}
