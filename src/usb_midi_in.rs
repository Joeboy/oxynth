use crate::synth::{MIDI_QUEUE_SIZE, MidiEvent as SynthMidiEvent};
use defmt::*;
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_usb::driver::host::DeviceEvent::Connected;
use embassy_usb::driver::host::UsbHostDriver;
use embassy_usb::handlers::midi::{MidiEvent as UsbMidiEvent, MidiHandler};
use embassy_usb::handlers::{HandlerEvent, UsbHostHandler};
use embassy_usb::host::UsbHostBusExt;
use heapless::spsc::Producer;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::host::InterruptHandler<USB>;
});

#[embassy_executor::task]
pub async fn usb_input_task(
    usb: Peri<'static, USB>,
    mut prod: Producer<'static, SynthMidiEvent, MIDI_QUEUE_SIZE>,
) -> ! {
    let mut usbhost = embassy_rp::usb::host::Driver::new(*usb, Irqs);

    info!("Detecting USB device...");
    // There seems to be an issue that like one time in ten the device isn't detected
    // Should investigate and fix that at some point.
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

                // Filter the MIDI events we care about, to avoid overflowing the queue
                // Could also maybe consider rate limiting for continuous controls
                let status_nybble = status & 0xF0;
                match status_nybble {
                    0xB0 | 0x90 | 0x80 => {
                        // CC | Note On | Note Off
                        let _ = prod.enqueue(SynthMidiEvent {
                            status,
                            data1,
                            data2,
                        });
                    }
                    _ => {
                        debug!("Ignored MIDI status={:#X}", status);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                defmt::warn!("MIDI wait error: {:?}", e);
            }
        }
    }
}
