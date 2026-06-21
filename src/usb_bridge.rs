//! USB Bridge — RODE hardware discovery and audio streaming.
//!
//! Discovers RODE USB audio devices (RODECaster Pro II, Duo) using libusb,
//! claims the audio streaming interfaces, and bridges isochronous transfers
//! to/from the shared memory ring buffers.
//!
//! Equivalent to the Windows `CUSBDevice` / `CRCPDevice_2026` classes.

use crate::config::VadConfig;
use crate::ring_buffer::RingBufferSet;
use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use rusb::{DeviceHandle, GlobalContext, UsbContext};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Known RODE USB product IDs (from reverse engineering)
const KNOWN_RODE_PRODUCTS: &[(u16, &str)] = &[
    (0x0011, "RODECaster Pro"),
    (0x0012, "RODECaster Pro II (2026)"),
    (0x0013, "RODECaster Duo (2026)"),
    (0x0040, "RØDE AI-1"),
    (0x0041, "RØDE NT-USB"),
];

/// A discovered RODE USB audio device
#[derive(Debug)]
struct RodeUsbDevice {
    /// Product name derived from USB descriptor
    name: String,
    /// USB device handle
    _handle: DeviceHandle<GlobalContext>,
    /// USB bus number
    _bus: u8,
    /// USB device address
    _address: u8,
    /// Vendor ID
    _vendor_id: u16,
    /// Product ID
    pub product_id: u16,
}

/// Discover RODE USB audio devices on the system
fn discover_rode_devices(vendor_id: u16) -> Result<Vec<RodeUsbDevice>> {
    let context = rusb::Context::new()
        .context("Failed to create USB context")?;

    let devices = context.devices()
        .context("Failed to enumerate USB devices")?;

    let mut rode_devices = Vec::new();

    for device in devices.iter() {
        let desc = device.device_descriptor()
            .context("Failed to get device descriptor")?;

        if desc.vendor_id() != vendor_id {
            continue;
        }

        let product_name = KNOWN_RODE_PRODUCTS
            .iter()
            .find(|(pid, _)| *pid == desc.product_id())
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("Unknown RODE Device ({:04x}:{:04x})",
                desc.vendor_id(), desc.product_id()));

        info!(
            "Found RODE audio device: {} ({:04x}:{:04x}) on bus {} addr {}",
            product_name,
            desc.vendor_id(),
            desc.product_id(),
            device.bus_number(),
            device.address()
        );

        // Try to open the device
        match device.open() {
            Ok(handle) => {
                rode_devices.push(RodeUsbDevice {
                    name: product_name,
                    _handle: handle,
                    _bus: device.bus_number(),
                    _address: device.address(),
                    _vendor_id: desc.vendor_id(),
                    product_id: desc.product_id(),
                });
            }
            Err(e) => {
                warn!("Failed to open RODE device {}: {}", product_name, e);
            }
        }
    }

    Ok(rode_devices)
}

/// Audio transfer direction for USB isochronous streams
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Device → Host (capture/microphone)
    In,
    /// Host → Device (playback/speaker)
    Out,
}

/// Main USB bridge loop — runs in a dedicated thread
pub fn run_usb_bridge(cfg: &VadConfig, ring_buffers: &RingBufferSet) -> Result<()> {
    info!("USB bridge starting...");

    info!("Scanning for RODE USB devices (vendor 0x{:04x})...", cfg.rode_vendor_id);

    let _devices = discover_rode_devices(cfg.rode_vendor_id)
        .context("No RODE USB devices found")?;

    // The audio streaming requires:
    // 1. Claiming the audio streaming interface
    // 2. Setting the alternate interface with the right sample rate
    // 3. Submitting isochronous transfers
    //
    // For now, we implement the framework. Full isochronous streaming
    // requires the rusb async API and is platform-specific.

    info!("USB bridge monitoring loop started");
    ring_buffers.set_all_running();

    // Template buffer for audio data (period_frames * 2ch * 4bytes)
    let frame_bytes = 2 * 4; // stereo S32LE = 8 bytes
    let period_bytes = cfg.period_frames as usize * frame_bytes;
    let mut audio_buf: Vec<u8> = vec![0u8; period_bytes];

    // Main loop — re-enumerate devices periodically
    let poll_interval = Duration::from_millis(2000);
    loop {
        if crate::SHUTDOWN.load(Ordering::SeqCst) {
            info!("USB bridge shutting down...");
            break;
        }

        // TODO: Full isochronous transfer loop:
        //
        // For each capture channel:
        //   - Submit isochronous IN transfer buffer
        //   - On completion, copy data to ring_buffers.capture[ch_idx].write()
        //
        // For each playback channel:
        //   - ring_buffers.playback[ch_idx].read() → audio_buf
        //   - Submit isochronous OUT transfer with audio_buf
        //
        // This requires rusb's async API or a threaded poll loop with
        // libusb_handle_events_timeout_completed()

        // Simulate silence pass-through for testing (remove when USB is connected)
        for (i, rb) in ring_buffers.capture.iter().enumerate() {
            if !rb.is_running() {
                continue;
            }
            // Minimal: write silence to indicate we're alive
            if rb.available_frames() == 0 {
                audio_buf.fill(0);
                let _ = rb.write(&audio_buf[..]);
            }
        }

        for (i, rb) in ring_buffers.playback.iter().enumerate() {
            if !rb.is_running() {
                continue;
            }
            // Read and discard (audio consumed by USB output)
            let mut discard = vec![0u8; period_bytes];
            let _ = rb.read(&mut discard);
        }

        std::thread::sleep(poll_interval);
    }

    ring_buffers.stop_all();
    info!("USB bridge stopped.");
    Ok(())
}
