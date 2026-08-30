use crate::synth::{MIDI_QUEUE_SIZE, MidiEvent as SynthMidiEvent};
use defmt::*;
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_usb_driver::host::pipe;
use embassy_usb_driver::host::{PipeError, UsbHostAllocator, UsbPipe};
use embassy_usb_driver::{Direction, EndpointInfo, EndpointType};
use embassy_usb_host::descriptor::ConfigurationDescriptorChain;
use embassy_usb_host::handler::{BusRoute, EnumerationInfo, RegisterError};
use embassy_usb_host::{BusState, bus};
use heapless::spsc::Producer;
use {defmt_rtt as _, panic_probe as _};

const MAX_DESCRIPTOR_SIZE: usize = 512;
static USB_BUS_STATE: BusState = BusState::new();

#[repr(C)]
#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct MidiPacket {
    pub data: [u8; 4],
}

impl MidiPacket {
    fn from_bytes(bytes: [u8; 4]) -> Self {
        Self { data: bytes }
    }
}

#[derive(Debug, defmt::Format)]
pub enum MidiEvent {
    MidiPacket(MidiPacket),
}

pub struct MidiHandler<'d, A: UsbHostAllocator<'d>> {
    bulk_pipe: A::Pipe<pipe::Bulk, pipe::In>,
}

impl<'d, A: UsbHostAllocator<'d>> MidiHandler<'d, A> {
    fn try_register(
        bus: &A,
        enum_info: &EnumerationInfo,
        config_descriptor: &ConfigurationDescriptorChain<'_>,
    ) -> Result<Self, RegisterError> {
        let iface = config_descriptor
            .iter_interface()
            .find(|v| {
                v.interface_class == 0x01
                    && v.interface_subclass == 0x03
                    && v.interface_protocol == 0x00
            })
            .ok_or(RegisterError::NoSupportedInterface)?;

        let bulk_ep = iface
            .iter_endpoints()
            .find(|v| v.ep_type() == EndpointType::Bulk && v.ep_dir() == Direction::In)
            .ok_or(RegisterError::NoSupportedInterface)?;

        let bulk_endpoint: EndpointInfo = bulk_ep.into();
        let bulk_pipe = bus.alloc_pipe::<pipe::Bulk, pipe::In>(
            enum_info.device_address,
            &bulk_endpoint,
            enum_info.split(),
        )?;

        Ok(Self { bulk_pipe })
    }

    async fn wait_for_event(&mut self) -> Result<MidiEvent, PipeError> {
        let mut buffer = [0u8; 4];
        self.bulk_pipe.request_in(&mut buffer).await?;
        Ok(MidiEvent::MidiPacket(MidiPacket::from_bytes(buffer)))
    }
}

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::host::InterruptHandler<USB>;
});

#[embassy_executor::task]
pub async fn usb_input_task(
    usb: Peri<'static, USB>,
    mut prod: Producer<'static, SynthMidiEvent, MIDI_QUEUE_SIZE>,
) -> ! {
    let driver = embassy_rp::usb::host::Driver::new(usb, Irqs);
    let (mut controller, bus) = bus(driver, &USB_BUS_STATE);

    loop {
        info!("Detecting USB device...");
        let speed = controller.wait_for_connection().await;
        info!("Found device with speed = {:?}", speed);

        let mut config_buffer = [0u8; MAX_DESCRIPTOR_SIZE];
        let (enum_info, config_len) = match bus
            .enumerate(BusRoute::Direct(speed), &mut config_buffer)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                warn!("MIDI device enumeration failed: {:?}", error);
                continue;
            }
        };

        let configuration =
            match ConfigurationDescriptorChain::try_from_slice(&config_buffer[..config_len]) {
                Ok(configuration) => configuration,
                Err(error) => {
                    warn!("Invalid MIDI configuration descriptor: {:?}", error);
                    bus.free_address(enum_info.device_address);
                    continue;
                }
            };

        let mut midi_device = match MidiHandler::try_register(&bus, &enum_info, &configuration) {
            Ok(device) => device,
            Err(error) => {
                warn!("Couldn't register MIDI device: {:?}", error);
                bus.free_address(enum_info.device_address);
                continue;
            }
        };

        loop {
            match midi_device.wait_for_event().await {
                Ok(MidiEvent::MidiPacket(pkt)) => {
                    defmt::debug!("Received MIDI packet: {:?}", pkt);
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
                Err(error) => {
                    warn!("MIDI read error: {:?}", error);
                    break;
                }
            }
        }

        bus.free_address(enum_info.device_address);
    }
}
