# Oxynth

A Synthesizer written in Rust / [Embassy](https://embassy.dev/) for the
Raspberry Pi Pico 2 microcontroller.

## Overview

A minimal but capable "hardware" synth that can be easily bodged together from
cheap components (£8.66 per unit for the core components, see below). Once
you've wired it up and flashed it you can connect a USB MIDI keyboard and an amp
or headphones, and it's ready to play. The software in this repo gives you a
very basic synth, but if you want something custom you can modify
[synth.rs](./src/synth.rs) to taste.

### Caveats

- I'm fairly inexpert at electronics, Rust and audio synthesis, maybe this
  simple guide will be helpful for other inexperts. If you're looking for
  something advanced or expert, look elsewhere!
- Some of the code is LLM-assisted. In particular the default `synth.rs` was
  pretty much all spat out by an LLM.

## Hardware

Here's some instructions for building it on a solderless breadboard, like this:

![breadboard](./docs/images/breadboard.jpg)

You'll need:

- A Raspberry Pi Pico 2 with headers, currently £4.49
  [here](https://www.rapidonline.com/raspberry-pi-sc1632-pico-2-microcontroller-board-with-headers-11-5238)
- a PCM5102 I2S module. These are available on Amazon quite cheaply, eg. right
  now I see a pack of three of them for £8.02. Look for something like the
  purple board to the right of the above photo
- A micro USB OTG adapter. I can see these for £2.99 for two on Amazon.

You'll also need a bunch of other stuff like the breadboard, dupont wires,
soldering iron + solder, a midi USB keyboard, USB and jack cables, headphones or
an amp to plug it into... if we handwave all of that away, as well as delivery
costs and the fact we're bulk-buying, that gives us a cost of £8.66 per unit,
for the main components anyway.

### Connect the DAC board

Connect the Pico to the PCM5102 I2S module. This will give us a stereo audio
output via its onboard 3.5mm jack socket. The module comes with separate header
pins you'll have to solder on, or you could just solder wires directly to the
board.

Connect it up as follows:

```text
BCK -> GPIO18 (aka pin 24)
LCK -> GPIO19 (aka pin 25)
DIN -> GPIO20 (aka pin 26)
GND -> 0v
VIN -> 5v
```

### Connect the power

Because we're using the onboard USB port in host mode to connect a USB MIDI
keyboard, we can't use it for power in.

In the breadboard photo, 5v power is supplied via the grey and purple dupont
wires at the top. You can easily get 5v by cannibalizing a USB A cable. Then
wire up the pico to the power rails as follows:

```text
VBUS (pin 40) -> 5v
VSYS (pin 39) -> 5v
GND (pin 38) -> 0v
```

#### IMPORTANT NOTES

1. The wiring above is for USB host mode, where the Pico powers a USB device.
   With this wiring you _MUST_ _NOT_ connect the Pico's USB port directly to a
   USB _host_, eg. your computer, as that'll mean both hosts are trying to power
   each other, and hardware damage may ensue. Instead we're going to connect a
   USB _device_ via OTG, powered _from_ the Pico's USB socket.
2. I'm not actually sure connecting VBUS and VSYS is wholly safe / recommended.
   All I can say is it seems to work here with no issues, and the Pico is a
   pretty cheap device anyway. And
   [this guy](https://www.youtube.com/watch?v=yIXa-6DRW-Y) seems to think it's
   OK.

That's the audio output stuff connected, but we'll also need a way to actually
play it:

### Connect a USB MIDI keyboard

Pretty simple, plug an OTG adapter into the Pico's USB port, then plug a MIDI
USB keyboard into that. It works here with my Akai LPK25, hopefully it'll work
with yours too.

## Software

This repo contains Rust software that reads from the USB MIDI keyboard and
outputs notes via the PCM5102.

The default [synth code](./src/synth.rs) is pretty boring. It reads a buffer of
incoming midi messages and outputs notes to an audio buffer. It should be pretty
hackable if you want to make it do something more interesting.

At some point I'll provide a UF2 firmware file you can flash directly to the
device. But for now you'll need to build the Rust code yourself before flashing
the Pico.

So, you'll need
[Rust and Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html)
if you don't have them already.

and the `thumbv8m.main-none-eabihf` target:

```sh
rustup target add thumbv8m.main-none-eabihf
```

I recommend using a
[debug probe](https://www.raspberrypi.com/documentation/microcontrollers/debug-probe.html)
to flash the device. Certainly if you plan on doing any firmware modification.
Given which, you'll also want [probe-rs](https://probe.rs).

If you use a debug probe, in theory you should be able to just connect it up and
do `cargo run` from the repo's root folder. If you do that it'll build with
debug output, which will cause timing glitches. For "release mode", try
`DEFMT_LOG=off cargo run --release`.

I may well have forgotten things, in which case please file a github issue!

Side note: A bit inconveniently, embassy-rs doesn't support host mode usb or
gapless audio "out-of-the-box", so I had to
[patch](https://github.com/Joeboy/embassy/tree/usb-mid-host-plus-pio-dma-ping-pong)
it for those. Thanks to holly-hacker and dobrowolski-lukasz for doing the actual
work. Fortunately it turns out it's easy to update the cargo deps to
automatically use the patched branch, so I've done that now.

## The future / TODO

### Short-term

- Provide a UF2 flash file. So far I've been using a debug probe to flash the
  device, I guess people might want to flash it without building all the Rust
  stuff
- Move audio task onto dedicated CPU core for realtime safety
- Use MIDI controllers to modify synth sounds

### Longer term / maybe sometime / maybe never

- Audio input (so we can use it for effects as well as synthesis)
  - Remaining in the "cheap amazon components" vibe, this could be a PCM1808
    module.
  - But, it'd be nice if there was an I2S module that did both audio input +
    output, maybe based on something like the TLV310AIC3104. I find it a bit
    surprising nobody seems to make one. Maybe I should try to make one.
- I think, very theoretically, it could be possible to build static / no-std
  [LV2](https://lv2plug.in/) binaries for `thumbv8m.main-none-eabihf` and run
  them on the device. Maybe via [bFLT](https://github.com/uclinux-dev/elf2flt),
  which would be simpler to deal with than ELF on the device side. It'd be cool
  to be able to make hardware versions of all the thousands of open source LV2
  pluginsl Maybe connecting them together in the device software, a bit like the
  much beefier [MOD](https://mod.audio/) devices. But I'm not expecting this to
  be easy, or necessarily even doable at all.
