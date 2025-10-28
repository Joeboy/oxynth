use core::ops::ControlFlow;
use micromath::F32Ext;

use heapless::spsc::Queue;
use static_cell::StaticCell;
use defmt::info;

// Define a static SPSC queue for MIDI events (backing storage). The queue
// is initialized in `main.rs` at startup and split there; the consumer is
// moved into the audio task to avoid `static mut` usage.
pub static MIDI_QUEUE: StaticCell<Queue<MidiEvent, 32>> = StaticCell::new();

pub const QUEUE_SIZE: usize = 32;

// Define your MIDI event type
#[derive(Copy, Clone)]
pub struct MidiEvent {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}
#[inline]
fn pack_lr_16(l: i16, r: i16) -> u32 {
    ((l as u32 as u16 as u32) << 16) | ((r as u16) as u32)
}

const TONE_HZ: u32 = 540;
const SAMPLE_RATE: u32 = 48_000;

#[inline]
fn midi_note_to_freq(note: u8) -> f32 {
    // Standard MIDI note to frequency: A4 = 69 -> 440 Hz
    440.0 * 2f32.powf(((note as i32 - 69) as f32) / 12.0)
}

// Simple synth state shared inside the Synth struct.
struct SynthState {
    freq_hz: f32,
    amp: f32,
}

impl Default for SynthState {
    fn default() -> Self {
        Self {
            freq_hz: TONE_HZ as f32,
            amp: 0.0,
        }
    }
}

/// Minimal synth that owns a MIDI consumer and generates audio from it.
pub struct Synth {
    cons: heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>,
    state: SynthState,
    phase_acc: f32,
}

impl Synth {
    pub fn new(cons: heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>) -> Self {
        Self {
            cons,
            state: SynthState::default(),
            phase_acc: 0.0,
        }
    }

    pub fn process(&mut self, buf: &mut [u32]) -> ControlFlow<(), ()> {
        // Inline the audio_callback logic here, but operating on `self`.
        const MAX_AMPLITUDE: i16 = 1024;

        // Drain MIDI events and update synth state
        while let Some(event) = self.cons.dequeue() {
            info!("SYNTH: MIDI event: status={}, data1={}, data2={}", event.status, event.data1, event.data2);
            let status_nybble = event.status & 0xF0;
            match status_nybble {
                0x90 => {
                    // Note On (velocity 0 treated as Note Off)
                    if event.data2 > 0 {
                        let note = event.data1;
                        let freq = midi_note_to_freq(note);
                        self.state.freq_hz = freq;
                        self.state.amp = (event.data2 as f32) / 127.0;
                    } else {
                        self.state.amp = 0.0;
                    }
                }
                0x80 => {
                    // Note Off
                    self.state.amp = 0.0;
                }
                _ => {}
            }
        }

        // Render audio
        for w in buf.iter_mut() {
            let freq = self.state.freq_hz;
            let amp = self.state.amp;
            let phase_inc = if freq > 0.0 { freq / (SAMPLE_RATE as f32) } else { 0.0 };
            self.phase_acc += phase_inc;
            if self.phase_acc >= 1.0 {
                self.phase_acc -= 1.0;
            }

            let angle = 2.0 * core::f32::consts::PI * self.phase_acc;
            let sample = (MAX_AMPLITUDE as f32 * amp * angle.sin()) as i16;
            *w = pack_lr_16(sample, sample);
        }

        ControlFlow::Continue(())
    }
}


