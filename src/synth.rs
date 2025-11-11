use core::ops::ControlFlow;
use micromath::F32Ext;

use defmt::debug;
use heapless::spsc::Queue;
use static_cell::StaticCell;

pub const MIDI_QUEUE_SIZE: usize = 32;
pub static MIDI_QUEUE: StaticCell<Queue<MidiEvent, MIDI_QUEUE_SIZE>> = StaticCell::new();

const SAMPLE_RATE: u32 = 48_000;

#[derive(Copy, Clone)]
pub struct MidiEvent {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

// Pack left and right 16-bit samples into a single u32, as that's what the I2S DMA expects
#[inline]
fn pack_lr_16(l: i16, r: i16) -> u32 {
    ((l as u32 as u16 as u32) << 16) | ((r as u16) as u32)
}

#[inline]
fn midi_note_to_freq(note: u8) -> f32 {
    // Standard MIDI note to frequency: A4 = 69 -> 440 Hz
    440.0 * 2f32.powf(((note as i32 - 69) as f32) / 12.0)
}

/// Minimal synth that owns a MIDI consumer and generates audio from it.
pub struct Synth {
    cons: heapless::spsc::Consumer<'static, MidiEvent, MIDI_QUEUE_SIZE>,
    voices: [Voice; N_VOICES],
    age_counter: u32,
}

impl Synth {
    pub fn new(cons: heapless::spsc::Consumer<'static, MidiEvent, MIDI_QUEUE_SIZE>) -> Self {
        Self {
            cons,
            voices: [Voice::new(); N_VOICES],
            age_counter: 0,
        }
    }
    pub fn process(&mut self, buf: &mut [u32]) -> ControlFlow<(), ()> {
        // Polyphonic synth rendering
        const MAX_AMPLITUDE: i16 = 12000; // headroom
        // ADSR parameters (seconds / level)
        const ATTACK_TIME_S: f32 = 0.005; // 10 ms
        const DECAY_TIME_S: f32 = 0.050; // 50 ms
        const SUSTAIN_LEVEL: f32 = 0.2; // sustain as fraction of peak
        const RELEASE_TIME_S: f32 = 0.500; // 100 ms

        // Drain MIDI events and update voice allocation
        while let Some(event) = self.cons.dequeue() {
            debug!(
                "SYNTH: MIDI event: status={}, data1={}, data2={}",
                event.status, event.data1, event.data2
            );
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
                            self.voices[idx].start_with_adsr(
                                note,
                                freq,
                                vel_amp,
                                self.age_counter,
                                ATTACK_TIME_S,
                                DECAY_TIME_S,
                                SUSTAIN_LEVEL,
                            );
                        } else {
                            // steal oldest voice (smallest age)
                            if let Some((idx, _)) = self
                                .voices
                                .iter()
                                .enumerate()
                                .min_by(|a, b| a.1.age.cmp(&b.1.age))
                            {
                                self.age_counter = self.age_counter.wrapping_add(1);
                                self.voices[idx].start_with_adsr(
                                    note,
                                    freq,
                                    vel_amp,
                                    self.age_counter,
                                    ATTACK_TIME_S,
                                    DECAY_TIME_S,
                                    SUSTAIN_LEVEL,
                                );
                            }
                        }
                    } else {
                        // velocity 0 -> note off
                        let note = event.data1;
                        for v in self.voices.iter_mut() {
                            if v.note == note && v.gate {
                                v.note_off(RELEASE_TIME_S);
                            }
                        }
                    }
                }
                0x80 => {
                    // Note Off
                    let note = event.data1;
                    for v in self.voices.iter_mut() {
                        if v.note == note && v.gate {
                            v.note_off(RELEASE_TIME_S);
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
                // envelope state machine
                match v.stage {
                    EnvStage::Idle => {
                        // nothing
                    }
                    EnvStage::Attack => {
                        v.env += v.attack_inc;
                        if v.env >= v.target_amp {
                            v.env = v.target_amp;
                            v.stage = EnvStage::Decay;
                        }
                    }
                    EnvStage::Decay => {
                        v.env -= v.decay_inc;
                        let sustain_level = v.sustain_level * v.target_amp;
                        if v.env <= sustain_level {
                            v.env = sustain_level;
                            v.stage = EnvStage::Sustain;
                        }
                    }
                    EnvStage::Sustain => {
                        // hold at sustain level while gate
                        // if gate turned off elsewhere, stage should have been set to Release
                    }
                    EnvStage::Release => {
                        v.env -= v.release_inc;
                        if v.env <= 0.0 {
                            v.env = 0.0;
                            v.stage = EnvStage::Idle;
                            v.gate = false;
                        }
                    }
                }

                // advance phase
                let phase_inc = if v.freq > 0.0 {
                    v.freq / (SAMPLE_RATE as f32)
                } else {
                    0.0
                };
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

#[derive(Copy, Clone, PartialEq, Eq)]
enum EnvStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Copy, Clone)]
struct Voice {
    note: u8,
    freq: f32,
    target_amp: f32,
    env: f32,
    gate: bool,
    phase: f32,
    age: u32,
    // ADSR fields
    stage: EnvStage,
    attack_inc: f32,
    decay_inc: f32,
    sustain_level: f32,
    release_inc: f32,
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
            stage: EnvStage::Idle,
            attack_inc: 0.0,
            decay_inc: 0.0,
            sustain_level: 1.0,
            release_inc: 0.0,
        }
    }

    fn start_with_adsr(
        &mut self,
        note: u8,
        freq: f32,
        vel_amp: f32,
        age: u32,
        attack_s: f32,
        decay_s: f32,
        sustain_level: f32,
    ) {
        self.note = note;
        self.freq = freq;
        self.target_amp = vel_amp;
        self.gate = true;
        self.age = age;
        self.sustain_level = sustain_level;

        // compute per-sample increments (simple linear ramps)
        let attack_samples = (attack_s * (SAMPLE_RATE as f32)).max(1.0);
        self.attack_inc = if attack_samples > 0.0 {
            self.target_amp / attack_samples
        } else {
            self.target_amp
        };

        let decay_samples = (decay_s * (SAMPLE_RATE as f32)).max(1.0);
        let sustain_target = self.sustain_level * self.target_amp;
        self.decay_inc = if decay_samples > 0.0 {
            (self.target_amp - sustain_target) / decay_samples
        } else {
            self.target_amp - sustain_target
        };

        // release_inc will be computed at note-off based on current env
        self.release_inc = 0.0;

        // start envelope
        self.stage = EnvStage::Attack;
        // keep current env to avoid hard clicks; if env is 0 start at tiny value
        if self.env <= 0.0 {
            self.env = 0.0;
        }
    }

    fn note_off(&mut self, release_s: f32) {
        self.gate = false;
        // compute release increment to bring env to 0 over release_s seconds
        let release_samples = (release_s * (SAMPLE_RATE as f32)).max(1.0);
        self.release_inc = if release_samples > 0.0 {
            self.env / release_samples
        } else {
            self.env
        };
        self.stage = EnvStage::Release;
    }

    fn active(&self) -> bool {
        self.stage != EnvStage::Idle || self.env > 1e-6
    }
}
