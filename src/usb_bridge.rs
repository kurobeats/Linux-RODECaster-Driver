//! USB Bridge — RODE hardware discovery and audio streaming.
//!
//! Discovers RODE USB audio devices (RODECaster Pro II, Duo) using libusb,
//! parses the USB Audio Class 2.0 descriptors to discover the actual channel
//! topology, claims the audio streaming interfaces, and bridges isochronous
//! transfers to/from the shared memory ring buffers.
//!
//! Equivalent to the Windows `CUSBDevice` / `CRCPDevice_2026` classes.

use crate::config::VadConfig;
use crate::ring_buffer::RingBufferSet;
use crate::usb_descriptor::{self, DiscoveredChannel};
use anyhow::{bail, Context, Result};
use log::{debug, error, info, warn};
use rusb::{DeviceHandle, UsbContext};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Known RODE USB product IDs (from reverse engineering)
const KNOWN_RODE_PRODUCTS: &[(u16, &str)] = &[
    (0x0011, "RODECaster Pro"),
    (0x0012, "RODECaster Pro II (2026)"),
    (0x0013, "RODECaster Duo (2026)"),
    (0x0040, "RØDE AI-1"),
    (0x0041, "RØDE NT-USB"),
];

/// Poll interval for re-discovering devices when none connected
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Idle timeout before USB autosuspend (matches Windows D3 timeout of 3s)
const AUTOSUSPEND_IDLE_MS: u64 = 3000;

/// Actively connected RODE device
struct ActiveDevice {
    /// USB device handle
    handle: DeviceHandle<rusb::Context>,
    /// Discovered audio channels from USB descriptors
    channels: Vec<DiscoveredChannel>,
    /// Last audio transfer time for autosuspend tracking
    last_audio_time: Instant,
}

impl ActiveDevice {
    /// Check if device has been idle long enough to suspend
    fn should_suspend(&self) -> bool {
        self.last_audio_time.elapsed() > Duration::from_millis(AUTOSUSPEND_IDLE_MS)
    }
}

/// Attempt to detach kernel driver and claim a USB interface
fn detach_and_claim(handle: &DeviceHandle<rusb::Context>, iface: u8) -> Result<()> {
    if let Ok(true) = handle.kernel_driver_active(iface) {
        info!("Detaching kernel driver from interface {}", iface);
        handle.detach_kernel_driver(iface)
            .context("Failed to detach kernel driver")?;
    }
    handle.claim_interface(iface)
        .with_context(|| format!("Failed to claim interface {}", iface))
}

/// Release interface and re-attach kernel driver
fn release_iface(handle: &DeviceHandle<rusb::Context>, iface: u8) {
    let _ = handle.release_interface(iface);
    let _ = handle.attach_kernel_driver(iface);
}

/// Connect to a RODE USB audio device, parse descriptors, claim interfaces
fn connect_rode_device(vendor_id: u16) -> Result<ActiveDevice> {
    let ctx = rusb::Context::new().context("Failed to create USB context")?;
    let devices = ctx.devices().context("Failed to enumerate USB devices")?;

    for device in devices.iter() {
        let desc = device.device_descriptor()?;
        if desc.vendor_id() != vendor_id {
            continue;
        }

        let product_name = KNOWN_RODE_PRODUCTS
            .iter()
            .find(|(pid, _)| *pid == desc.product_id())
            .map(|(_, n)| *n)
            .unwrap_or("Unknown RODE device");

        info!("Found {} ({:04x}:{:04x}) on bus {} addr {}",
            product_name, desc.vendor_id(), desc.product_id(),
            device.bus_number(), device.address());

        let mut handle = match device.open() {
            Ok(h) => h,
            Err(e) => { warn!("Cannot open {}: {}", product_name, e); continue; }
        };

        // Dump descriptors at debug level
        if log::log_enabled!(log::Level::Debug) {
            let _ = usb_descriptor::dump_descriptors(&device, &handle);
        }

        // Parse USB Audio Class topology
        let channels = usb_descriptor::parse_audio_topology(&device, &handle)
            .context("Failed to parse USB audio topology")?;

        if channels.is_empty() {
            warn!("No audio streaming endpoints found on {}", product_name);
            continue;
        }

        info!("Discovered {} audio channels", channels.len());

        // Claim all audio interfaces
        let claimed: std::collections::BTreeSet<u8> = channels
            .iter()
            .map(|ch| ch.interface_number)
            .collect();

        for iface in &claimed {
            detach_and_claim(&handle, *iface)?;
            // Set alt setting from the first channel on this interface
            if let Some(ch) = channels.iter().find(|c| c.interface_number == *iface) {
                handle.set_alternate_setting(*iface, ch.alt_setting)
                    .with_context(|| format!("Failed to set alt setting for iface {}", iface))?;
            }
        }

        return Ok(ActiveDevice { handle, channels, last_audio_time: Instant::now() });
    }

    bail!("No RODE USB audio devices found")
}

/// Run isochronous streaming between USB and ring buffers
fn run_streaming(
    device: &mut ActiveDevice,
    ring_buffers: &RingBufferSet,
    cfg: &VadConfig,
) -> Result<()> {
    let timeout = Duration::from_millis(50);
    let period = Duration::from_secs_f64(cfg.period_frames as f64 / cfg.sample_rate as f64);
    let mut next = Instant::now();
    let mut drift_log_time = Instant::now();

    info!("USB streaming with sample_rate={}Hz, period={}us",
        cfg.sample_rate, period.as_micros());

    loop {
        if crate::SHUTDOWN.load(Ordering::SeqCst) { break; }

        let mut had_audio = false;

        // CAPTURE: USB IN endpoints → ring buffers
        for (idx, ch) in device.channels.iter()
            .filter(|c| c.direction == "capture")
            .enumerate()
        {
            if idx >= ring_buffers.capture.len() { break; }
            if !ring_buffers.capture[idx].is_running() { continue; }

            // Calculate buffer size from discovered channel parameters
            let sample_bytes = std::cmp::max(1, ch.bit_resolution as usize / 8);
            let channel_bytes = ch.audio_channels as usize * sample_bytes;
            let max_bytes = channel_bytes * cfg.period_frames as usize;
            let mut buf = vec![0u8; max_bytes];

            match device.handle.read_interrupt(ch.endpoint_address, &mut buf, timeout) {
                Ok(n) if n > 0 => {
                    let _ = ring_buffers.capture[idx].write(&buf[..n]);
                    had_audio = true;
                }
                Ok(_) => {}
                Err(rusb::Error::Timeout) => {}
                Err(rusb::Error::Pipe) => { let _ = device.handle.clear_halt(ch.endpoint_address); }
                Err(e) => { debug!("USB cap err EP 0x{:02x}: {}", ch.endpoint_address, e); }
            }
        }

        // PLAYBACK: ring buffers → USB OUT endpoints
        for (idx, ch) in device.channels.iter()
            .filter(|c| c.direction == "playback")
            .enumerate()
        {
            if idx >= ring_buffers.playback.len() { break; }
            if !ring_buffers.playback[idx].is_running() { continue; }

            let sample_bytes = std::cmp::max(1, ch.bit_resolution as usize / 8);
            let channel_bytes = ch.audio_channels as usize * sample_bytes;
            let max_bytes = channel_bytes * cfg.period_frames as usize;
            let mut buf = vec![0u8; max_bytes];

            let got = ring_buffers.playback[idx].read(&mut buf).unwrap_or(0);
            let nbytes = (got * channel_bytes).min(max_bytes);

            match device.handle.write_interrupt(ch.endpoint_address, &buf[..nbytes], timeout) {
                Ok(_) => { had_audio = true; }
                Err(rusb::Error::Timeout) => {}
                Err(rusb::Error::Pipe) => { let _ = device.handle.clear_halt(ch.endpoint_address); }
                Err(e) => { debug!("USB pb err EP 0x{:02x}: {}", ch.endpoint_address, e); }
            }
        }

        // Update autosuspend timer
        if had_audio {
            device.last_audio_time = Instant::now();
        }

        // Periodic drift monitoring: log fill ratios every 5 seconds
        if drift_log_time.elapsed() > Duration::from_secs(5) {
            let ratios: Vec<f32> = ring_buffers.capture.iter()
                .take(4).map(|rb| rb.fill_ratio()).collect();
            debug!("Ring fill ratios (first 4 capture): {:?}", ratios);
            drift_log_time = Instant::now();
        }

        next += period;
        let now = Instant::now();
        if next > now { std::thread::sleep(next - now); }
        else { next = now + period; }
    }

    Ok(())
}

/// Main USB bridge entry point — runs in a dedicated thread
pub fn run_usb_bridge(cfg: &VadConfig, ring_buffers: &RingBufferSet) -> Result<()> {
    info!("USB bridge starting...");
    ring_buffers.set_all_running();

    loop {
        if crate::SHUTDOWN.load(Ordering::SeqCst) {
            info!("USB bridge shutting down...");
            break;
        }

        info!("Scanning for RODE USB devices (vendor 0x{:04x})...", cfg.rode_vendor_id);

        match connect_rode_device(cfg.rode_vendor_id) {
            Ok(mut device) => {
                info!("Connected to RODE device with {} channels", device.channels.len());
                if let Err(e) = run_streaming(&mut device, ring_buffers, cfg) {
                    error!("USB streaming error: {:#}", e);
                }
                // Cleanup
                let ifaces: std::collections::BTreeSet<u8> = device.channels
                    .iter().map(|c| c.interface_number).collect();
                for iface in &ifaces {
                    release_iface(&device.handle, *iface);
                }
                info!("USB device disconnected, will re-scan...");
            }
            Err(e) => {
                debug!("No RODE device: {}", e);
                // Feed silence through ring buffers while polling
                let silence = vec![0u8; cfg.period_frames as usize * 2 * 4];
                let mut discard = vec![0u8; cfg.period_frames as usize * 2 * 4];
                let start = Instant::now();
                while start.elapsed() < DEVICE_POLL_INTERVAL {
                    if crate::SHUTDOWN.load(Ordering::SeqCst) {
                        ring_buffers.stop_all();
                        return Ok(());
                    }
                    for rb in ring_buffers.capture.iter() {
                        if rb.available_frames() == 0 { let _ = rb.write(&silence); }
                    }
                    for rb in ring_buffers.playback.iter() {
                        let _ = rb.read(&mut discard);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    ring_buffers.stop_all();
    info!("USB bridge stopped.");
    Ok(())
}
