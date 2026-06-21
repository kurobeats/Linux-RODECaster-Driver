# RODECaster Virtual Audio Device — Linux Port (WIP)

> **⚠️ DISCLAIMER: I have absolutely no idea what I'm doing.**
>
> This is a fumble through Ghidra, USB specs, and ALSA documentation,
> held together by AI-generated code, caffeine, and stubbornness.
> If you're looking for a production-ready driver, you're in the wrong repo.
> If you're looking for an adventure, let's go.

## What is this?

An attempt to port the **RODECaster Virtual Audio Device** driver from Windows to Linux. The Windows driver (`rodecastervad.sys` v1.0.4.0) exposes RODE USB audio hardware (RODECaster Pro II, Duo) as 11 virtual input devices + 5 virtual output devices to applications like DAWs.

This repo tries to replicate that on Linux using:
- **ALSA** for virtual PCM devices
- **libusb** (`rusb`) for USB isochronous audio streaming
- **POSIX shared memory** (`shm_open` / `mmap`) for the ring buffer transport
- **systemd** for daemonization

## Status

| Thing | Status |
|-------|--------|
| Compiles | 🤷 probably not |
| Works | 😂 absolutely not |
| USB device discovery | ⚠️ guesses about endpoint addresses |
| ALSA bridge | 🚧 skeleton exists |
| Ring buffer | 🚧 skeleton exists |
| Actual audio passing through | ❌ dreams only |
| My sanity | 📉 declining |

## How It's Supposed To Work (in theory)

```
┌──────────────┐    USB     ┌─────────────┐   shm    ┌──────────────┐
│ RODE Hardware │◄─────────►│ USB Bridge   │◄────────►│ ALSA Bridge  │
│ (Pro II/Duo)  │ isochron. │ (rusb async) │ring buf  │ (alsa I/O)   │
└──────────────┘            └─────────────┘          └──────┬───────┘
                                                             │
                                                   ┌─────────▼────────┐
                                                   │  Virtual ALSA    │
                                                   │  PCM Devices     │
                                                   │  (11 in / 5 out) │
                                                   └──────────────────┘
```

Audio flows: RODE hardware → USB isochronous transfers → shared memory
ring buffers → ALSA virtual PCM devices → your DAW/applications.
(And the reverse for playback.)

## Building

You'll need:
- Rust (edition 2021)
- ALSA development headers (`libasound2-dev` on Debian/Ubuntu)
- libusb development headers (`libusb-1.0-0-dev`)

```bash
cargo build
```

Will it build? No promises. The code was written by an AI that was told to "just make it like the Windows driver but on Linux" and we all know how well that goes.

## Configuration

Edit `rodecaster-vad.toml` to match your RODE hardware. The defaults assume you have a device at USB vendor `0x19F7` (RODE Microphones) with the channel layout of a RODECaster Pro II — both of which are wild assumptions.

## Running

```bash
# As a regular process (good luck)
cargo run

# As a systemd service (ambitious)
sudo cp rodecaster-vad.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start rodecaster-vad
```

## The "Methodology"

1. Open `rodecastervad.sys` in Ghidra
2. Stare at decompiled C++ until eyes glaze over
3. Ask an AI "what does this do and how do I make it work on Linux"
4. Paste the AI's output into Rust files
5. Repeat until something compiles or until giving up
6. There is no step 6

## Known Problems (there are many)

- **USB endpoint addresses are guessed.** The channel map in `channel_map.rs` uses made-up endpoint addresses (`0x81`–`0x8B` for capture, `0x01`–`0x05` for playback) because I haven't figured out how to extract the actual topology from the USB descriptors.
- **No actual isochronous transfer code.** The USB bridge has device   discovery but the actual streaming loop is... aspirational.
- **ALSA device naming is trial and error.** It tries `hw:`, `plughw:`,   `default:` prefixes hoping one sticks.
- **No `snd-aloop` setup instructions.** The virtual PCM devices need   the ALSA loopback kernel module (`snd-aloop`), and I haven't documented how to configure it for 16 devices.
- **Shared memory layout is reverse-engineered.** The `RingBufHeader` structure is based on reading Ghidra output and crossing fingers.
- **Probably doesn't compile.** The dependencies are specified, but I haven't tested whether `nix 0.29`, `alsa 0.9`, and `rusb 0.9` actually play nice together with these APIs.

## Help Wanted

If you actually know what you're doing with:
- USB Audio Class 2.0 isochronous streaming
- ALSA kernel modules / `snd-aloop` configuration
- RODE hardware USB descriptors
- Reverse engineering Windows kernel drivers

...please send help. Or at least point and laugh constructively.

## License

GPL-3.0 — see [LICENSE](LICENSE). Same as the Ghidra decompilation that inspired this mess. RODE doesn't make Linux drivers, so here we are.