#![no_std]
#![no_main]

mod audio_out;
mod synth;

use audio_out::audio_task;
use heapless::spsc::Queue;
use synth::{MIDI_QUEUE, MidiEvent as SynthMidiEvent};

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::USB;
use embassy_usb::driver::host::DeviceEvent::Connected;
use embassy_usb::driver::host::UsbHostDriver;
use embassy_usb::handlers::midi::{MidiEvent as UsbMidiEvent, MidiHandler};
use embassy_usb::handlers::{HandlerEvent, UsbHostHandler};
use embassy_usb::host::UsbHostBusExt;
use defmt::*;
use {defmt_rtt as _, panic_probe as _};



bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::host::InterruptHandler<USB>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Starting USB MIDI synth POC");
    let mut led = Output::new(p.PIN_25, Level::Low);
    led.set_high();

    // MIDI queue producer and consumer
    let queue = MIDI_QUEUE.init(Queue::new());
    let (mut prod, cons) = queue.split();

    spawner.spawn(
        audio_task(
            p.PIO0, p.DMA_CH0, p.DMA_CH1, p.DMA_CH2, p.PIN_18, p.PIN_19, p.PIN_20, cons,
        )
        .unwrap(),
    );

    let mut usbhost = embassy_rp::usb::host::Driver::new(*p.USB, Irqs);

    info!("Detecting USB device...");
    let speed = loop {
        match usbhost.wait_for_device_event().await {
            Connected(speed) => break speed,
            _ => {}
        }
    };

    println!("Found device with speed = {:?}", speed);

    let enum_info = usbhost.enumerate_root_bare(speed, 1).await.unwrap();
    let mut midi_device = MidiHandler::try_register(&usbhost, &enum_info)
        .await
        .expect("Couldn't register MIDI device");

    loop {
        let result = midi_device.wait_for_event().await;
        debug!("{:?}", result);

        match result {
            Ok(HandlerEvent::HandlerEvent(UsbMidiEvent::MidiPacket(pkt))) => {
                let bytes: [u8; 4] = pkt.data;
                let status = bytes[1];
                let data1 = bytes[2];
                let data2 = bytes[3];

                // Enqueue via the producer we created above in main.
                let _ = prod.enqueue(SynthMidiEvent {
                    status,
                    data1,
                    data2,
                });
            }
            Ok(_) => {}
            Err(e) => {
                defmt::warn!("MIDI wait error: {:?}", e);
            }
        }
    }
}
