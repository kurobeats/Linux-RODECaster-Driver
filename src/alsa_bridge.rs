//! ALSA Bridge — Virtual PCM device I/O layer.
//!
//! This module manages the ALSA side of the bridge:
//! - Opens virtual PCM devices (from snd-aloop or custom plugin)
//! - Reads audio from capture ring buffers → writes to ALSA capture PCMs
//! - Reads audio from ALSA playback PCMs → writes to playback ring buffers
//!
//! Equivalent to the Windows `CKsAudCapFilter` / `CKsAudRenFilter` classes.

use crate::audio_format::AudioFormat;
use crate::channel_map::ChannelMap;
use crate::config::VadConfig;
use crate::ring_buffer::{RingBuffer, RingBufferSet};
use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Verify that ALSA is available on the system
pub fn verify_alsa_available() -> Result<()> {
    // Try to open the default PCM to verify ALSA is functional
    match alsa::pcm::PCM::new("default", alsa::Direction::Playback, false) {
        Ok(_) => {
            info!("ALSA subsystem verified — default PCM available");
            Ok(())
        }
        Err(e) => {
            // Even if default fails, ALSA may still be present
            warn!("Default ALSA PCM not available: {}. Trying hw:0...", e);
            alsa::pcm::PCM::new("hw:0,0", alsa::Direction::Playback, false)
                .map(|_| ())
                .context("ALSA subsystem not functional — no PCM devices found")
        }
    }
}

/// Open an ALSA PCM device by card name and device index
fn open_pcm_device(
    device_name: &str,
    direction: alsa::Direction,
) -> Result<(alsa::pcm::PCM, String)> {
    // Try multiple naming conventions to find the virtual device
    let candidates = vec![
        format!("hw:{}", device_name),
        format!("hw:CARD={},DEV=0", device_name),
        format!("plughw:{}", device_name),
        format!("default:{}", device_name),
    ];

    // Also try with "RODECaster_" prefix for consistency
    let prefixed_candidates: Vec<String> = candidates
        .iter()
        .flat_map(|c| vec![c.clone(), format!("hw:RODECaster_{}", c)])
        .collect();

    for name in candidates.iter().chain(prefixed_candidates.iter()) {
        match alsa::pcm::PCM::new(name.as_str(), direction, false) {
            Ok(pcm) => {
                debug!("Opened ALSA PCM: {}", name);
                return Ok((pcm, name.clone()));
            }
            Err(_) => continue,
        }
    }

    // As a fallback, try to open by card index
    // This works when the virtual device is loaded as a kernel module
    for card_idx in 0..8 {
        let name = format!("hw:{},0", card_idx);
        if let Ok(pcm) = alsa::pcm::PCM::new(&name, direction, false) {
            // Check if this card has the right name
            if let Ok(info) = alsa::card::Card::new(card_idx).and_then(|c| {
                let name = c.get_name()?;
                Ok(name)
            }) {
                if info.contains("RODECaster") || info.contains("Loopback") {
                    info!("Using ALSA PCM: {} (card {})", name, card_idx);
                    return Ok((pcm, name));
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "No ALSA PCM device found for {}. \
         Ensure snd-aloop or the RODECaster kernel module is loaded.",
        device_name
    ))
}

/// Main ALSA bridge loop — runs in a dedicated thread
pub fn run_alsa_bridge(cfg: &VadConfig, ring_buffers: &RingBufferSet) -> Result<()> {
    info!("ALSA bridge starting...");

    let format = AudioFormat {
        sample_rate: cfg.sample_rate,
        channels: 2, // Default stereo
        format: crate::audio_format::SampleFormat::S32LE,
    };

    let period_frames = cfg.period_frames as usize;
    let period_bytes = period_frames * format.frame_bytes();

    // Allocate reusable buffers
    let mut cap_buf: Vec<u8> = vec![0u8; period_bytes];
    let mut pb_buf: Vec<u8> = vec![0u8; period_bytes];

    let interval = Duration::from_secs_f64(period_frames as f64 / cfg.sample_rate as f64);

    info!(
        "ALSA bridge configured: {} Hz, {} channels, {} frames/period, {} ms interval",
        cfg.sample_rate,
        format.channels,
        period_frames,
        interval.as_millis()
    );

    // Open all capture PCM devices
    let mut capture_pcms: Vec<(alsa::pcm::PCM, usize)> = Vec::new();
    for (i, ch) in cfg.capture_channels.iter().enumerate() {
        if i < ring_buffers.capture.len() {
            match open_pcm_device(&ch.alsa_id, alsa::Direction::Capture) {
                Ok((pcm, name)) => {
                    match format.apply_to_pcm(&pcm) {
                        Ok(()) => {
                            format.apply_sw_params(&pcm, period_frames, cfg.num_periods as usize)
                                .context("Failed to set SW params")?;
                            pcm.start()?;
                            info!("Capture PCM ready: {} ({})", ch.name, name);
                            capture_pcms.push((pcm, i));
                        }
                        Err(e) => {
                            warn!("Failed to configure capture PCM {}: {}. Skipping.", ch.name, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("{}", e);
                }
            }
        }
    }

    // Open all playback PCM devices
    let mut playback_pcms: Vec<(alsa::pcm::PCM, usize)> = Vec::new();
    for (i, ch) in cfg.playback_channels.iter().enumerate() {
        if i < ring_buffers.playback.len() {
            match open_pcm_device(&ch.alsa_id, alsa::Direction::Playback) {
                Ok((pcm, name)) => {
                    match format.apply_to_pcm(&pcm) {
                        Ok(()) => {
                            format.apply_sw_params(&pcm, period_frames, cfg.num_periods as usize)
                                .context("Failed to set SW params")?;
                            pcm.start()?;
                            info!("Playback PCM ready: {} ({})", ch.name, name);
                            playback_pcms.push((pcm, i));
                        }
                        Err(e) => {
                            warn!("Failed to configure playback PCM {}: {}. Skipping.", ch.name, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("{}", e);
                }
            }
        }
    }

    ring_buffers.set_all_running();
    info!(
        "ALSA bridge active: {} capture + {} playback devices",
        capture_pcms.len(),
        playback_pcms.len()
    );

    // Main I/O loop
    let mut next_tick = Instant::now();
    loop {
        // Check shutdown
        if crate::SHUTDOWN.load(Ordering::SeqCst) {
            info!("ALSA bridge shutting down...");
            break;
        }

        // --- CAPTURE direction: USB hardware → ring buffer → ALSA capture PCM ---
        // Read from ring buffer (audio from USB), write to ALSA capture PCM
        for (pcm, buf_idx) in &capture_pcms {
            if !ring_buffers.capture[*buf_idx].is_running() {
                continue;
            }

            let n_frames = ring_buffers.capture[*buf_idx].read(&mut cap_buf).unwrap_or(0);
            if n_frames == 0 {
                // No data available yet, write silence
                cap_buf.fill(0);
                let _ = pcm.io_i32().map(|io| {
                    io.writei(&vec![0i32; period_frames * format.channels as usize])
                        .ok()
                });
            } else {
                let bytes_read = n_frames * format.frame_bytes();
                // Convert interleaved i32 samples for ALSA
                let samples: Vec<i32> = cap_buf[..bytes_read]
                    .chunks_exact(4)
                    .map(|chunk| i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();

                if let Ok(io) = pcm.io_i32() {
                    let written = io.writei(&samples).unwrap_or(0);
                    if written < n_frames {
                        debug!("Capture PCM write underrun: {}/{} frames", written, n_frames);
                    }
                }
            }
        }

        // --- PLAYBACK direction: ALSA playback PCM → ring buffer → USB hardware ---
        for (pcm, buf_idx) in &playback_pcms {
            if !ring_buffers.playback[*buf_idx].is_running() {
                continue;
            }

            // Read from ALSA playback PCM
            let mut samples = vec![0i32; period_frames * format.channels as usize];
            if let Ok(io) = pcm.io_i32() {
                let read = io.readi(&mut samples).unwrap_or(0);
                if read > 0 {
                    // Convert i32 samples back to interleaved bytes
                    let mut audio_data: Vec<u8> = Vec::with_capacity(read * 4);
                    for &sample in &samples[..read * format.channels as usize] {
                        audio_data.extend_from_slice(&sample.to_le_bytes());
                    }
                    let _ = ring_buffers.playback[*buf_idx].write(&audio_data);
                }
            }
        }

        // Rate-limit to the period interval
        next_tick += interval;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            // We're falling behind, reset tick
            next_tick = now + interval;
        }
    }

    // Cleanup
    ring_buffers.stop_all();
    for (pcm, _) in &capture_pcms {
        let _ = pcm.drain();
    }
    for (pcm, _) in &playback_pcms {
        let _ = pcm.drain();
    }

    info!("ALSA bridge stopped.");
    Ok(())
}
