use crate::synth::{MIDI_QUEUE_SIZE, MidiEvent as SynthMidiEvent};
use defmt::*;
use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_usb::driver::host::DeviceEvent::Connected;
use embassy_usb::driver::host::{HostError, UsbChannel, UsbHostDriver, channel};
use embassy_usb::driver::{Direction, EndpointInfo, EndpointType};
use embassy_usb_host::control::ControlChannelExt;
use embassy_usb_host::descriptor::InterfaceDescriptor;
use embassy_usb_host::handler::{EnumerationInfo, HandlerEvent, RegisterError, UsbHostHandler};
use heapless::spsc::Producer;
use {defmt_rtt as _, panic_probe as _};

const MAX_DESCRIPTOR_SIZE: usize = 512;

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

pub struct MidiHandler<H: UsbHostDriver> {
    bulk_channel: H::Channel<channel::Bulk, channel::In>,
    _control_channel: H::Channel<channel::Control, channel::InOut>,
}

impl<H: UsbHostDriver> UsbHostHandler for MidiHandler<H> {
    type PollEvent = MidiEvent;
    type Driver = H;

    fn static_spec() -> embassy_usb_host::handler::StaticHandlerSpec {
        embassy_usb_host::handler::StaticHandlerSpec { device_filter: None }
    }

    async fn try_register(bus: &H, enum_info: &EnumerationInfo) -> Result<Self, RegisterError> {
        let mut control_channel = bus.alloc_channel::<channel::Control, channel::InOut>(
            enum_info.device_address,
            &EndpointInfo {
                addr: 0.into(),
                ep_type: EndpointType::Control,
                max_packet_size: (enum_info.device_desc.max_packet_size0 as u16)
                    .min(enum_info.speed.max_packet_size()),
                interval_ms: 0,
            },
            enum_info.ls_over_fs,
        )?;

        let mut cfg_desc_buf = [0u8; MAX_DESCRIPTOR_SIZE];
        let configuration = enum_info
            .active_config_or_set_default(&mut control_channel, &mut cfg_desc_buf)
            .await?;

        let iface = configuration
            .iter_interface()
            .find(|v| {
                matches!(
                    v,
                    InterfaceDescriptor {
                        interface_class: 0x01,
                        interface_subclass: 0x03,
                        interface_protocol: 0x00,
                        ..
                    }
                )
            })
            .ok_or(RegisterError::NoSupportedInterface)?;

        let bulk_ep = iface
            .iter_endpoints()
            .find(|v| v.ep_type() == EndpointType::Bulk && v.ep_dir() == Direction::In)
            .ok_or(RegisterError::NoSupportedInterface)?;

        control_channel
            .set_configuration(configuration.configuration_value)
            .await?;

        let bulk_channel = bus.alloc_channel::<channel::Bulk, channel::In>(
            enum_info.device_address,
            &bulk_ep.into(),
            enum_info.ls_over_fs,
        )?;

        Ok(Self {
            bulk_channel,
            _control_channel: control_channel,
        })
    }

    async fn wait_for_event(&mut self) -> Result<HandlerEvent<Self::PollEvent>, HostError> {
        let mut buffer = [0u8; 4];
        self.bulk_channel.request_in(&mut buffer).await?;
        Ok(HandlerEvent::HandlerEvent(MidiEvent::MidiPacket(
            MidiPacket::from_bytes(buffer),
        )))
    }
}

async fn enumerate_root_bare<H: UsbHostDriver>(
    bus: &H,
    speed: embassy_usb::driver::Speed,
    new_device_address: u8,
) -> Result<EnumerationInfo, HostError> {
    let mut channel = bus.alloc_channel::<channel::Control, channel::InOut>(
        0,
        &EndpointInfo {
            addr: 0.into(),
            ep_type: EndpointType::Control,
            max_packet_size: speed.max_packet_size(),
            interval_ms: 0,
        },
        false,
    )?;

    channel.enumerate_device(speed, new_device_address, false).await
}

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::host::InterruptHandler<USB>;
});

#[embassy_executor::task]
pub async fn usb_input_task(
    usb: Peri<'static, USB>,
    mut prod: Producer<'static, SynthMidiEvent, MIDI_QUEUE_SIZE>,
) -> ! {
    let usbhost = embassy_rp::usb::host::Driver::new(usb, Irqs);

    info!("Detecting USB device...");
    // There seems to be an issue that like one time in ten the device isn't detected
    // Should investigate and fix that at some point.
    let speed = loop {
        match usbhost.wait_for_device_event().await {
            Connected(speed) => break speed,
            _ => {}
        }
    };

    info!("Found device with speed = {:?}", speed);

    let enum_info = enumerate_root_bare(&usbhost, speed, 1).await.unwrap();
    let mut midi_device = MidiHandler::try_register(&usbhost, &enum_info)
        .await
        .expect("Couldn't register MIDI device");

    loop {
        let result = midi_device.wait_for_event().await;
        debug!("{:?}", result);

        match result {
            Ok(HandlerEvent::HandlerEvent(MidiEvent::MidiPacket(pkt))) => {
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
            Ok(_) => {}
            Err(e) => {
                defmt::warn!("MIDI wait error: {:?}", e);
            }
        }
    }
}
