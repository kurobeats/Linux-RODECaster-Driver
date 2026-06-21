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
mod ring_buffer;
mod shm;
mod signal_handler;
mod usb_bridge;

use anyhow::{Context, Result};
use log::{error, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Global shutdown flag — set by SIGTERM/SIGINT handler
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    info!("RODECaster Virtual Audio Device v{}", env!("CARGO_PKG_VERSION"));
    info!("Based on reverse engineering of rodecastervad.sys v1.0.4.0");

    // Load configuration
    let cfg = config::load_config().context("Failed to load configuration")?;
    info!("Configuration loaded: {:?}", cfg);

    // Set up signal handling (SIGTERM, SIGINT → graceful shutdown)
    signal_handler::setup_signal_handlers()?;

    // Verify ALSA is available
    alsa_bridge::verify_alsa_available().context("ALSA subsystem not available")?;

    // Create shared memory ring buffers for each channel
    let ring_buffers = Arc::new(
        ring_buffer::RingBufferSet::new(&cfg)
            .context("Failed to create shared memory ring buffers")?,
    );

    // --- Start ALSA Bridge ---
    // Opens virtual ALSA loopback devices and processes audio I/O
    let alsa_rb = Arc::clone(&ring_buffers);
    let alsa_cfg = cfg.clone();
    let alsa_handle = thread::Builder::new()
        .name("alsa-bridge".into())
        .spawn(move || {
            if let Err(e) = alsa_bridge::run_alsa_bridge(&alsa_cfg, &alsa_rb) {
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
    info!("Channels: {} capture, {} playback", cfg.capture_channels.len(), cfg.playback_channels.len());

    // Wait for shutdown signal
    while !SHUTDOWN.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(500));
    }

    info!("Shutting down...");
    SHUTDOWN.store(true, Ordering::SeqCst);

    // Wait for threads to finish
    let _ = alsa_handle.join();
    let _ = usb_handle.join();

    info!("RODECaster VAD stopped.");
    Ok(())
}
