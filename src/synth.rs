use core::ops::ControlFlow;
use micromath::F32Ext;

#[inline]
fn pack_lr_16(l: i16, r: i16) -> u32 {
    ((l as u32 as u16 as u32) << 16) | ((r as u16) as u32)
}

const TONE_HZ: u32 = 540;
const SAMPLE_RATE: u32 = 48_000;

pub fn audio_callback(buf: &mut [u32]) -> ControlFlow<(), ()> {
    const AMPLITUDE: i16 = 1024;
    const SAMPLES_PER_CYCLE: u32 = SAMPLE_RATE / TONE_HZ;
    static mut PHASE: u32 = 0;

    for w in buf.iter_mut() {
        // Calculate phase in [0, SAMPLES_PER_CYCLE)
        let phase = unsafe { PHASE };
        unsafe {
            PHASE = (PHASE + 1) % SAMPLES_PER_CYCLE;
        }

        // --- Example: Sine wave ---
        let angle = 2.0 * core::f32::consts::PI * (phase as f32) / (SAMPLES_PER_CYCLE as f32);
        let sample = (AMPLITUDE as f32 * angle.sin()) as i16;

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
