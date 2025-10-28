use core::ops::ControlFlow;
use micromath::F32Ext;

use heapless::spsc::Queue;
use static_cell::StaticCell;
use defmt::info;

// Define a static SPSC queue for MIDI events
pub static MIDI_QUEUE: StaticCell<Queue<MidiEvent, 32>> = StaticCell::new();
// static mut MIDI_CONS: Option<heapless::spsc::Consumer<MidiEvent, 32>> = None;
// pub static mut MIDI_PROD: Option<heapless::spsc::Producer<MidiEvent, 32>> = None;

// pub fn init_midi_queue() {
//     let queue = MIDI_QUEUE.init(Queue::new());
//     let (prod, cons) = queue.split();
//     unsafe {
//         MIDI_CONS = Some(cons);
//         MIDI_PROD = Some(prod);
//     }
// }

const QUEUE_SIZE: usize = 32;

// Store producer/consumer in plain statics (Option) and set them at init.
// We access them with `unsafe` where required. This avoids relying on
// methods that may not exist on the `StaticCell` type in this crate
// version (e.g. `as_ref()` / `as_mut()`).
pub static mut MIDI_PRODUCER: Option<heapless::spsc::Producer<'static, MidiEvent, QUEUE_SIZE>> = None;
static mut MIDI_CONSUMER: Option<heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>> = None;

pub fn init_midi_queue() {
    let (p, c) = MIDI_QUEUE.init(Queue::new()).split();
    unsafe {
        MIDI_PRODUCER = Some(p);
        MIDI_CONSUMER = Some(c);
    }
}

// Define your MIDI event type
#[derive(Copy, Clone)]
pub struct MidiEvent {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

// Simple synth state shared with the audio callback. Kept minimal and
// accessed inside `unsafe` in the real-time callback.
struct SynthState {
    freq_hz: f32,
    amp: f32, // 0.0..1.0
}

impl Default for SynthState {
    fn default() -> Self {
        Self {
            freq_hz: TONE_HZ as f32,
            amp: 0.0,
        }
    }
}

static mut SYNTH_STATE: SynthState = SynthState {
    freq_hz: TONE_HZ as f32,
    amp: 0.0,
};

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

pub fn audio_callback(buf: &mut [u32]) -> ControlFlow<(), ()> {
    // Real-time synth parameters
    const MAX_AMPLITUDE: i16 = 1024;
    static mut PHASE_ACC: f32 = 0.0; // phase [0.0, 1.0)

    // Pull any pending MIDI events from the consumer and update synth state.
    unsafe {
        let cons_ptr = &raw mut MIDI_CONSUMER;
        let cons_opt: &mut Option<heapless::spsc::Consumer<'static, MidiEvent, QUEUE_SIZE>> = &mut *cons_ptr;
        if let Some(cons) = cons_opt.as_mut() {
            while let Some(event) = cons.dequeue() {
                info!("SYNTH: MIDI event: status={}, data1={}, data2={}", event.status, event.data1, event.data2);
                let status_nybble = event.status & 0xF0;
                match status_nybble {
                    0x90 => {
                        // Note On (velocity 0 treated as Note Off)
                        if event.data2 > 0 {
                            let note = event.data1;
                            let freq = midi_note_to_freq(note);
                            SYNTH_STATE.freq_hz = freq;
                            SYNTH_STATE.amp = (event.data2 as f32) / 127.0;
                        } else {
                            // velocity 0 -> note off
                            SYNTH_STATE.amp = 0.0;
                        }
                    }
                    0x80 => {
                        // Note Off
                        SYNTH_STATE.amp = 0.0;
                    }
                    _ => {
                        // ignore other messages for now
                    }
                }
            }
        }
    }

    for w in buf.iter_mut() {
        // Update phase accumulator based on current frequency
        let (freq, amp) = unsafe { (SYNTH_STATE.freq_hz, SYNTH_STATE.amp) };
        let phase_inc = if freq > 0.0 { freq / (SAMPLE_RATE as f32) } else { 0.0 };
        unsafe {
            PHASE_ACC += phase_inc;
            if PHASE_ACC >= 1.0 {
                PHASE_ACC -= 1.0;
            }
        }

        // --- Sine wave ---
        let angle = 2.0 * core::f32::consts::PI * unsafe { PHASE_ACC };
        let sample = (MAX_AMPLITUDE as f32 * amp * angle.sin()) as i16;

        // --- Example: Triangle wave ---
        // let sample = ((2 * AMPLITUDE as i32 * phase as i32) / SAMPLES_PER_CYCLE as i32
        //     - AMPLITUDE as i32) as i16;

        // --- Example: Square wave ---
        // let sample = if phase < SAMPLES_PER_CYCLE / 2 {
        //     AMPLITUDE
        // } else {
        //     -AMPLITUDE
        // };

        *w = pack_lr_16(sample, sample); // mono → stereo
    }
    ControlFlow::Continue(())
}
