use core::ops::ControlFlow;
use micromath::F32Ext;

use defmt::debug;
use heapless::spsc::Queue;
use static_cell::StaticCell;

pub const MIDI_QUEUE_SIZE: usize = 32;
pub static MIDI_QUEUE: StaticCell<Queue<MidiEvent, MIDI_QUEUE_SIZE>> = StaticCell::new();

const SAMPLE_RATE: u32 = 48_000;

const N_VOICES: usize = 10;

#[derive(Copy, Clone)]
pub struct MidiEvent {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Waveform {
    Sine,
    Square,
    Sawtooth,
    Triangle,
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
    // Waveform (controllable via MIDI CC 21)
    waveform: Waveform,
    // ADSR parameters (controllable via MIDI CC 22-25)
    attack_time_s: f32,
    decay_time_s: f32,
    sustain_level: f32,
    release_time_s: f32,
    // Filter parameters (controllable via MIDI CC 26-27)
    filter_cutoff: f32,    // 0.0 to 1.0 (fraction of sample rate)
    filter_resonance: f32, // 0.0 to 4.0
}

impl Synth {
    pub fn new(cons: heapless::spsc::Consumer<'static, MidiEvent, MIDI_QUEUE_SIZE>) -> Self {
        Self {
            cons,
            voices: [Voice::new(); N_VOICES],
            age_counter: 0,
            // Default waveform (controllable via MIDI CC 21)
            waveform: Waveform::Sine,
            // Default ADSR values (controllable via MIDI CC 22-25)
            attack_time_s: 0.005,  // 5 ms (CC 22)
            decay_time_s: 0.050,   // 50 ms (CC 23)
            sustain_level: 0.2,    // 20% (CC 24)
            release_time_s: 0.500, // 500 ms (CC 25)
            // Default filter values (controllable via MIDI CC 26-27)
            filter_cutoff: 0.5,    // 50% of sample rate (CC 26)
            filter_resonance: 0.5, // Low resonance (CC 27)
        }
    }
    pub fn process(&mut self, buf: &mut [u32]) -> ControlFlow<(), ()> {
        // Polyphonic synth rendering
        const MAX_AMPLITUDE: i16 = 12000; // headroom

        // Drain MIDI events and update voice allocation
        while let Some(event) = self.cons.dequeue() {
            debug!(
                "SYNTH: MIDI event: status={}, data1={}, data2={}",
                event.status, event.data1, event.data2
            );
            let status_nybble = event.status & 0xF0;
            match status_nybble {
                0xB0 => {
                    // Control Change
                    let cc_num = event.data1;
                    let cc_val = event.data2;
                    match cc_num {
                        21 => {
                            // Waveform: divide 0-127 into 4 regions
                            self.waveform = match cc_val {
                                0..=31 => Waveform::Sine,
                                32..=63 => Waveform::Square,
                                64..=95 => Waveform::Sawtooth,
                                96..=127 => Waveform::Triangle,
                                _ => Waveform::Sine, // fallback
                            };
                            let waveform_name = match self.waveform {
                                Waveform::Sine => "Sine",
                                Waveform::Square => "Square",
                                Waveform::Sawtooth => "Sawtooth",
                                Waveform::Triangle => "Triangle",
                            };
                            debug!("Waveform set to {}", waveform_name);
                        }
                        22 => {
                            // Attack time: map 0-127 to 0.001-2.0 seconds
                            self.attack_time_s = 0.001 + (cc_val as f32 / 127.0) * 1.999;
                            debug!("Attack time set to {} s", self.attack_time_s);
                        }
                        23 => {
                            // Decay time: map 0-127 to 0.001-2.0 seconds
                            self.decay_time_s = 0.001 + (cc_val as f32 / 127.0) * 1.999;
                            debug!("Decay time set to {} s", self.decay_time_s);
                        }
                        24 => {
                            // Sustain level: map 0-127 to 0.0-1.0
                            self.sustain_level = cc_val as f32 / 127.0;
                            debug!("Sustain level set to {}", self.sustain_level);
                        }
                        25 => {
                            // Release time: map 0-127 to 0.001-3.0 seconds
                            self.release_time_s = 0.001 + (cc_val as f32 / 127.0) * 2.999;
                            debug!("Release time set to {} s", self.release_time_s);
                        }
                        26 => {
                            // Filter cutoff: map 0-127 to 0.0-1.0 (fraction of sample rate)
                            self.filter_cutoff = cc_val as f32 / 127.0;
                            debug!("Filter cutoff set to {}", self.filter_cutoff);
                        }
                        27 => {
                            // Filter resonance: map 0-127 to 0.0-4.0
                            self.filter_resonance = (cc_val as f32 / 127.0) * 4.0;
                            debug!("Filter resonance set to {}", self.filter_resonance);
                        }
                        _ => {}
                    }
                }
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
                                self.attack_time_s,
                                self.decay_time_s,
                                self.sustain_level,
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
                                    self.attack_time_s,
                                    self.decay_time_s,
                                    self.sustain_level,
                                );
                            }
                        }
                    } else {
                        // velocity 0 -> note off
                        let note = event.data1;
                        for v in self.voices.iter_mut() {
                            if v.note == note && v.gate {
                                v.note_off(self.release_time_s);
                            }
                        }
                    }
                }
                0x80 => {
                    // Note Off
                    let note = event.data1;
                    for v in self.voices.iter_mut() {
                        if v.note == note && v.gate {
                            v.note_off(self.release_time_s);
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
                    let sample = match self.waveform {
                        Waveform::Sine => {
                            let angle = 2.0 * core::f32::consts::PI * v.phase;
                            angle.sin()
                        }
                        Waveform::Square => {
                            if v.phase < 0.5 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        Waveform::Sawtooth => 2.0 * v.phase - 1.0,
                        Waveform::Triangle => {
                            if v.phase < 0.5 {
                                4.0 * v.phase - 1.0
                            } else {
                                3.0 - 4.0 * v.phase
                            }
                        }
                    };

                    // Apply resonant low-pass filter (simple 2-pole)
                    // Calculate filter coefficients based on cutoff and resonance
                    let cutoff_freq = self.filter_cutoff * 0.5; // Max at Nyquist
                    let resonance = self.filter_resonance;

                    // Chamberlain state-variable filter
                    let f = (cutoff_freq * core::f32::consts::PI).min(1.5); // Frequency coefficient
                    let q = 1.0 - resonance * 0.24; // Damping (lower = more resonance)
                    let q_clamped = q.max(0.05); // Prevent complete instability

                    let lowpass = v.filter_buf1 + f * v.filter_buf0;
                    let highpass = sample - lowpass - q_clamped * v.filter_buf0;
                    let bandpass = f * highpass + v.filter_buf0;

                    v.filter_buf0 = bandpass;
                    v.filter_buf1 = lowpass;

                    let filtered = lowpass;
                    mix += filtered * v.env;
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
    // Filter state (simple 2-pole resonant low-pass)
    filter_buf0: f32,
    filter_buf1: f32,
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
            filter_buf0: 0.0,
            filter_buf1: 0.0,
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
