//! RODECaster Virtual Audio Device — Linux Audio Bridge Daemon
//!
//! This daemon bridges RODE USB audio hardware (RODECaster Pro II, Duo)
//! to virtual ALSA PCM devices, providing the same 11-input / 5-output
//! channel topology as the Windows RODECaster VAD driver.
//!
//! Architecture:
//! ┌──────────────┐    USB     ┌─────────────┐   shm    ┌──────────────┐
//! │ RODE Hardware │◄─────────►│ USB Bridge   │◄────────►│ ALSA Bridge  │
//! │ (Pro II/Duo)  │ isochronous│ (rusb async) │ring buf  │ (alsa I/O)   │
//! └──────────────┘            └─────────────┘          └──────┬───────┘
//!                                                              │
//!                                                    ┌─────────▼────────┐
//!                                                    │  Virtual ALSA    │
//!                                                    │  PCM Devices     │
//!                                                    │  (snd-aloop or   │
//!                                                    │   custom plugin) │
//!                                                    └──────────────────┘
//!
//! Reconstructed from reverse engineering of rodecastervad.sys (v1.0.4.0)
//! and RODECasterVADService.exe — see RODECaster_Linux_Porting_Guide.md.

mod alsa_bridge;
mod audio_format;
mod channel_map;
mod config;
mod drift;
mod mixer;
mod ring_buffer;
mod shm;
mod signal_handler;
mod usb_bridge;
mod usb_descriptor;

use anyhow::{Context, Result};
use log::{error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Global shutdown flag — set by SIGTERM/SIGINT handler
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Send systemd watchdog notification (sd_notify WATCHDOG=1)
fn systemd_notify_watchdog() {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::net::UnixDatagram;
        if let Ok(socket) = UnixDatagram::unbound() {
            let path = std::env::var("NOTIFY_SOCKET").unwrap_or_default();
            if !path.is_empty() {
                let _ = socket.send_to(b"WATCHDOG=1", &path);
            }
        }
    }
}

/// Send systemd ready notification (sd_notify READY=1)
fn systemd_notify_ready() {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::net::UnixDatagram;
        if let Ok(socket) = UnixDatagram::unbound() {
            let path = std::env::var("NOTIFY_SOCKET").unwrap_or_default();
            if !path.is_empty() {
                let _ = socket.send_to(b"READY=1", &path);
            }
        }
    }
}

/// Send systemd status message
fn systemd_notify_status(msg: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::net::UnixDatagram;
        if let Ok(socket) = UnixDatagram::unbound() {
            let path = std::env::var("NOTIFY_SOCKET").unwrap_or_default();
            if !path.is_empty() {
                let status = format!("STATUS={}", msg);
                let _ = socket.send_to(status.as_bytes(), &path);
            }
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    info!("RODECaster Virtual Audio Device v{}", env!("CARGO_PKG_VERSION"));
    info!("Based on reverse engineering of rodecastervad.sys v1.0.4.0");

    // Load configuration
    let cfg = config::load_config().context("Failed to load configuration")?;
    info!("Configuration loaded: {} capture + {} playback channels",
        cfg.capture_channels.len(), cfg.playback_channels.len());

    // Set up signal handling (SIGTERM, SIGINT → graceful shutdown)
    signal_handler::setup_signal_handlers()?;

    // Verify ALSA is available
    alsa_bridge::verify_alsa_available().context("ALSA subsystem not available")?;

    // Create shared memory ring buffers for each channel
    let ring_buffers = Arc::new(
        ring_buffer::RingBufferSet::new(&cfg)
            .context("Failed to create shared memory ring buffers")?,
    );

    // Start the mixer control creator (creates ALSA kcontrols if possible)
    mixer::create_mixer_elements(&cfg);

    // --- Start Mixer Sync ---
    // Periodically reads ALSA mixer controls for volume/mute per channel
    let mixer_state = Arc::new(mixer::MixerState::new(&cfg));
    let mixer_cfg = cfg.clone();
    let mixer_mix = Arc::clone(&mixer_state);
    let _mixer_handle = thread::Builder::new()
        .name("mixer-sync".into())
        .spawn(move || {
            mixer::run_mixer_sync(mixer_cfg, mixer_mix);
        })?;

    // --- Start ALSA Bridge ---
    // Opens virtual ALSA loopback devices and processes audio I/O
    let alsa_rb = Arc::clone(&ring_buffers);
    let alsa_cfg = cfg.clone();
    let alsa_mixer = Arc::clone(&mixer_state);
    let alsa_handle = thread::Builder::new()
        .name("alsa-bridge".into())
        .spawn(move || {
            if let Err(e) = alsa_bridge::run_alsa_bridge(&alsa_cfg, &alsa_rb, &alsa_mixer) {
                error!("ALSA bridge fatal error: {:#}", e);
                SHUTDOWN.store(true, Ordering::SeqCst);
            }
        })?;

    // --- Start USB Bridge ---
    // Discovers RODE hardware and bridges audio to ring buffers
    let usb_rb = Arc::clone(&ring_buffers);
    let usb_cfg = cfg.clone();
    let usb_handle = thread::Builder::new()
        .name("usb-bridge".into())
        .spawn(move || {
            if let Err(e) = usb_bridge::run_usb_bridge(&usb_cfg, &usb_rb) {
                error!("USB bridge fatal error: {:#}", e);
                SHUTDOWN.store(true, Ordering::SeqCst);
            }
        })?;

    info!("RODECaster VAD running. Waiting for shutdown signal...");
    systemd_notify_ready();
    systemd_notify_status("Running — bridging RODE hardware to virtual ALSA devices");

    // Watchdog interval is typically half the systemd WatchdogSec value
    let watchdog_interval = Duration::from_secs(5); // systemd WatchdogSec=10, so notify every 5s
    let mut last_watchdog = Instant::now();

    // Wait for shutdown signal
    while !SHUTDOWN.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(500));

        // Pet the systemd watchdog
        if last_watchdog.elapsed() >= watchdog_interval {
            systemd_notify_watchdog();
            last_watchdog = Instant::now();
        }
    }

    info!("Shutting down...");
    systemd_notify_status("Stopping...");
    SHUTDOWN.store(true, Ordering::SeqCst);

    // Wait for threads to finish (with timeout)
    let _ = alsa_handle.join();
    let _ = usb_handle.join();

    info!("RODECaster VAD stopped.");
    Ok(())
}
