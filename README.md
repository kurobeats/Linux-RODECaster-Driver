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
| Compiles | ✅ Yes (Rust 2021, nix 0.29, alsa 0.9, rusb 0.9) |
| Works | 🚧 Framework complete, untested on real hardware |
| USB device discovery | ✅ Runtime USB Audio Class 2.0 descriptor parser |
| USB isochronous transfers | ✅ Full read/write loop with autosuspend |
| ALSA bridge | ✅ Format-aware I/O with volume/mute mixer |
| Ring buffer | ✅ Lock-free SPSC with ping-pong double-buffering |
| snd-aloop setup | ✅ Setup script for 16 subdevices |
| systemd integration | ✅ Type=notify with watchdog support |
| PipeWire/WirePlumber | ✅ Persistent named device config |
| Mixer controls (Volume/Mute) | ✅ Per-channel atomic state with ALSA ctl sync |
| Clock drift compensation | ✅ Fill-ratio monitoring with correction hints |
| Unit tests | ✅ 9 ring buffer tests passing |
| My sanity | 📈 Recovering |

## How It Works

```
┌──────────────┐    USB     ┌─────────────┐   shm    ┌──────────────┐
│ RODE Hardware │◄─────────►│ USB Bridge   │◄────────►│ ALSA Bridge  │
│ (Pro II/Duo)  │ isochron. │ (rusb I/O)   │ring buf  │ (alsa I/O)   │
└──────────────┘            └──────┬───────┘          └──────┬───────┘
                                   │                         │
                        ┌──────────▼──────────┐    ┌─────────▼────────┐
                        │ Autosuspend (3s idle)│    │ Mixer (vol/mute) │
                        │ Drift compensation   │    │ Per-channel state│
                        └─────────────────────┘    └──────────────────┘
                                                             │
                                                   ┌─────────▼────────┐
                                                   │  Virtual ALSA    │
                                                   │  PCM Devices     │
                                                   │  (snd-aloop:     │
                                                   │   11 in / 5 out) │
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
- pkg-config

```bash
# Install system dependencies
sudo apt install libasound2-dev libusb-1.0-0-dev pkg-config build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# Build
cargo build

# Run tests
cargo test
```

## Configuration

Edit `rodecaster-vad.toml` to match your setup. The defaults use
RODE vendor `0x19F7` with the RODECaster Pro II 11-in/5-out channel layout.

```toml
sample_rate = 48000
period_frames = 256
num_periods = 4
rode_vendor_id = 0x19F7
shm_dir = "/dev/shm"
```

Channel names match the Windows INF exactly: "System In", "Combo 1 In",
"Combo 2 In", "Combo 3 In", "Combo 4 In", "Bluetooth In",
"Smart Pads In", "USB1 Main In", "USB1 Chat In", "USB2 In",
"Headset In" (capture), and "System Out", "Music Out", "Game Out",
"Virtual A Out", "Virtual B Out" (playback).

## Running

```bash
# 1. Set up virtual ALSA loopback devices (needed once)
sudo ./scripts/setup_snd_aloop.sh

# 2. Run as a foreground process
RUST_LOG=info cargo run

# 3. Or install as a systemd service
sudo cp rodecaster-vad.service /etc/systemd/system/
sudo cp rodecaster-vad.toml /etc/rodecaster-vad/config.toml
sudo systemctl daemon-reload
sudo systemctl start rodecaster-vad
sudo journalctl -u rodecaster-vad -f
```

### PipeWire/WirePlumber Integration

For persistent device names in PipeWire:
```bash
mkdir -p ~/.config/wireplumber/main.lua.d/
cp config/wireplumber/50-rodecaster-vad.lua ~/.config/wireplumber/main.lua.d/
systemctl --user restart wireplumber
```

## The "Methodology"

1. Open `rodecastervad.sys` in Ghidra
2. Stare at decompiled C++ until eyes glaze over
3. Ask an AI "what does this do and how do I make it work on Linux"
4. Paste the AI's output into Rust files
5. Repeat until something compiles or until giving up
6. There is no step 6

## Known Problems

- **Untested on real RODE hardware.** The USB bridge is fully implemented
  but has only been verified in a VM/fallback mode. Real hardware will
  reveal whether the USB Audio Class 2.0 descriptor parser correctly
  discovers the endpoint topology.
- **snd-aloop subdevice mapping is approximate.** The loopback card
  exposes numbered subdevices (0-15). Mapping to our channel IDs
  ("System", "Combo1", etc.) relies on the PipeWire config.
- **No vendor-specific USB control transfers.** RODE hardware likely uses
  custom USB commands for routing, mixing, and gain. These require Ghidra
  analysis of `rodecastervad.sys` / `RODECasterVADService.exe`.
- **Mixer controls are read-only from userspace.** snd-aloop's kernel
  module creates its own controls. We read existing ones but cannot
  create new ones from userspace.

## Project Structure

```
src/
├── main.rs               # Entry point, systemd watchdog, 4 threads
├── config.rs              # 11 capture + 5 playback channel definitions
├── alsa_bridge.rs         # ALSA PCM I/O with volume/mute mixer
├── usb_bridge.rs          # USB discovery, isochronous transfer loop, autosuspend
├── usb_descriptor.rs      # USB Audio Class 2.0 topology parser
├── ring_buffer.rs         # Lock-free SPSC ring buffer + DoubleRingBuffer + 9 tests
├── shm.rs                 # POSIX shared memory (shm_open / mmap)
├── audio_format.rs        # S16/S24/S32/Float conversion + ALSA hw/sw params
├── mixer.rs               # Per-channel volume/mute state, ALSA ctl sync
├── drift.rs               # Clock drift monitoring & correction hints
├── channel_map.rs         # USB endpoint → virtual channel mapping
├── signal_handler.rs      # SIGTERM/SIGINT graceful shutdown
config/wireplumber/
└── 50-rodecaster-vad.lua  # PipeWire persistent device names
scripts/
└── setup_snd_aloop.sh     # snd-aloop loader (16 subdevices)
```

## TODO / Roadmap

### Needs Real Hardware Testing

- [ ] Test USB descriptor parser against actual RODECaster Pro II / Duo
- [ ] Verify isochronous endpoint addresses match discovered descriptors
- [ ] Tune period_frames / num_periods for real-world latency

### Needs Ghidra (Windows Driver RE)

- [ ] Extract vendor-specific USB control transfer commands
      (routing, mixing, gain control for each channel)
- [ ] Reverse `FUN_14000c840` — the main device bridge loop
      (2728-byte state machine in `RODECasterVADService.exe`)
- [ ] Find the IOCTL codes used between service and kernel driver
- [ ] Document the device initialization sequence
      (0x28-byte slot init at `0x1400448f8`)

### Wishlist

- [ ] Custom ALSA kernel module (`snd-rodecaster-vad`) to replace
      snd-aloop and expose proper per-channel mixer kcontrols
- [ ] JACK / PipeWire native backend (bypass ALSA for lower latency)
- [ ] udev rules for automatic snd-aloop loading on device plug
- [ ] LED meter / peak level support (need hardware protocol docs)
- [ ] GUI control panel (tray icon equivalent)

## License

GPL-3.0 — see [LICENSE](LICENSE). Same as the Ghidra decompilation that inspired this mess. RODE doesn't make Linux drivers, so here we are.