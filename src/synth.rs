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

const SAMPLE_RATE: u32 = 48_000;

#[inline]
fn midi_note_to_freq(note: u8) -> f32 {
    // Standard MIDI note to frequency: A4 = 69 -> 440 Hz
    440.0 * 2f32.powf(((note as i32 - 69) as f32) / 12.0)
}

/// Minimal synth that owns a MIDI consumer and generates audio from it.
pub struct Synth {
    cons: heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>,
    voices: [Voice; N_VOICES],
    age_counter: u32,
}

impl Synth {
    pub fn new(cons: heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>) -> Self {
        Self {
            cons,
            voices: [Voice::new(); N_VOICES],
            age_counter: 0,
        }
    }
    pub fn process(&mut self, buf: &mut [u32]) -> ControlFlow<(), ()> {
        // Polyphonic synth rendering
        const MAX_AMPLITUDE: i16 = 12000; // headroom
        const ATTACK_COEFF: f32 = 0.2; // how quickly env approaches target on note-on
        const RELEASE_COEFF: f32 = 0.02; // how quickly env decays on note-off

        // Drain MIDI events and update voice allocation
        while let Some(event) = self.cons.dequeue() {
            info!("SYNTH: MIDI event: status={}, data1={}, data2={}", event.status, event.data1, event.data2);
            let status_nybble = event.status & 0xF0;
            match status_nybble {
                0x90 => {
                    // Note On (velocity 0 treated as Note Off)
                    if event.data2 > 0 {
                        let note = event.data1;
                        let vel_amp = (event.data2 as f32) / 127.0;
                        let freq = midi_note_to_freq(note);
                        // find free voice
                        if let Some(idx) = self.voices.iter().position(|v| !v.active()) {
                            self.age_counter = self.age_counter.wrapping_add(1);
                            self.voices[idx].start(note, freq, vel_amp, self.age_counter);
                        } else {
                            // steal oldest voice (smallest age)
                            if let Some((idx, _)) = self.voices.iter().enumerate().min_by(|a, b| a.1.age.cmp(&b.1.age)) {
                                self.age_counter = self.age_counter.wrapping_add(1);
                                self.voices[idx].start(note, freq, vel_amp, self.age_counter);
                            }
                        }
                    } else {
                        // velocity 0 -> note off
                        let note = event.data1;
                        for v in self.voices.iter_mut() {
                            if v.note == note && v.gate {
                                v.gate = false;
                            }
                        }
                    }
                }
                0x80 => {
                    // Note Off
                    let note = event.data1;
                    for v in self.voices.iter_mut() {
                        if v.note == note && v.gate {
                            v.gate = false;
                        }
                    }
                }
                _ => {}
            }
        }

        // Render audio: sum voices
        for w in buf.iter_mut() {
            let mut mix: f32 = 0.0;
            for v in self.voices.iter_mut() {
                // envelope smoothing toward target
                if v.gate {
                    v.env += (v.target_amp - v.env) * ATTACK_COEFF;
                } else {
                    v.env += (0.0 - v.env) * RELEASE_COEFF;
                    if v.env < 1e-4 {
                        v.env = 0.0; // consider inactive
                    }
                }

                // advance phase
                let phase_inc = if v.freq > 0.0 { v.freq / (SAMPLE_RATE as f32) } else { 0.0 };
                v.phase += phase_inc;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }

                if v.env > 0.0 {
                    let angle = 2.0 * core::f32::consts::PI * v.phase;
                    mix += angle.sin() * v.env;
                }
            }

            // normalize mix by number of voices to avoid clipping
            let mix_norm = mix / (N_VOICES as f32);
            let sample = (MAX_AMPLITUDE as f32 * mix_norm) as i16;
            *w = pack_lr_16(sample, sample);
        }

        ControlFlow::Continue(())
    }
}

// Number of voices
const N_VOICES: usize = 5;

#[derive(Copy, Clone)]
struct Voice {
    note: u8,
    freq: f32,
    target_amp: f32,
    env: f32,
    gate: bool,
    phase: f32,
    age: u32,
}

impl Voice {
    const fn new() -> Self {
        Self {
            note: 0,
            freq: 0.0,
            target_amp: 0.0,
            env: 0.0,
            gate: false,
            phase: 0.0,
            age: 0,
        }
    }

    fn start(&mut self, note: u8, freq: f32, vel_amp: f32, age: u32) {
        self.note = note;
        self.freq = freq;
        self.target_amp = vel_amp;
        self.gate = true;
        self.age = age;
        // keep current env to avoid clicks
    }

    fn active(&self) -> bool {
        self.gate || self.env > 1e-6
    }
}


