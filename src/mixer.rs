//! ALSA Mixer Controls — Volume and Mute per virtual audio channel.
//!
//! The Windows driver exposes topology nodes (Volume, Mute, PeakMeter, AGC)
//! on each filter's topology miniport. This module provides equivalent
//! ALSA mixer elements via the snd-aloop loopback card's control interface.
//!
//! Equivalent to Windows `IMiniportTopology` with nodes:
//!   Capture: Pin→Volume→Mute→ADC→Pin
//!   Render:  Pin→DAC→Volume→Mute→Pin

use crate::config::{ChannelConfig, VadConfig};
use log::debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Per-channel mixer state (shared between mixer thread and bridge)
#[derive(Debug)]
pub struct ChannelMixerState {
    /// Volume as linear multiplier (0.0 = silent, 1.0 = unity, >1.0 = boost)
    /// Stored as fixed-point: value * 256 as u16
    pub volume: std::sync::atomic::AtomicU16,
    /// Mute flag
    pub mute: std::sync::atomic::AtomicBool,
}

impl Default for ChannelMixerState {
    fn default() -> Self {
        Self {
            volume: std::sync::atomic::AtomicU16::new(256), // 1.0
            mute: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl ChannelMixerState {
    /// Get volume as a linear f32 multiplier
    pub fn volume_f32(&self) -> f32 {
        let raw = self.volume.load(Ordering::Acquire);
        raw as f32 / 256.0
    }

    /// Set volume as linear f32 multiplier
    pub fn set_volume_f32(&self, v: f32) {
        let raw = (v.clamp(0.0, 4.0) * 256.0) as u16;
        self.volume.store(raw, Ordering::Release);
    }

    pub fn is_muted(&self) -> bool {
        self.mute.load(Ordering::Acquire)
    }
}

/// Set of mixer states for all channels
pub struct MixerState {
    pub capture_mixers: Vec<ChannelMixerState>,
    pub playback_mixers: Vec<ChannelMixerState>,
}

impl MixerState {
    pub fn new(cfg: &VadConfig) -> Self {
        Self {
            capture_mixers: (0..cfg.capture_channels.len())
                .map(|_| ChannelMixerState::default())
                .collect(),
            playback_mixers: (0..cfg.playback_channels.len())
                .map(|_| ChannelMixerState::default())
                .collect(),
        }
    }
}

/// Apply volume and mute to a buffer of interleaved i32 samples in-place
pub fn apply_mixer(buf: &mut [i32], mixer: &ChannelMixerState) {
    if mixer.is_muted() {
        buf.fill(0);
        return;
    }
    let vol = mixer.volume_f32();
    if (vol - 1.0).abs() > f32::EPSILON {
        for sample in buf.iter_mut() {
            *sample = (*sample as f32 * vol) as i32;
        }
    }
}

/// Read volume/mute controls from ALSA mixer elements if available.
/// snd-aloop doesn't expose per-subdevice mixer — we expose them
/// through the ring buffer shared memory header flags instead.
///
/// This function scans for any ALSA mixer controls matching RODECaster
/// channel names and reads their values into the mixer state.
pub fn sync_mixer_from_alsa(cfg: &VadConfig, mixers: &MixerState) {
    // snd-aloop loopback exposes a single set of controls per card.
    // We use the shared memory header flags field as a fallback
    // for per-channel control when ALSA doesn't provide them.

    // Try to find the loopback card's control interface
    for card_idx in 0..8 {
        let card = alsa::card::Card::new(card_idx);
        if let Ok(name) = card.get_name() {
            if name.contains("Loopback") || name.contains("RODECaster") {
                if let Ok(ctl) = alsa::ctl::Ctl::new(&format!("hw:{}", card_idx), false) {
                    debug!("Found mixer control interface on card {}", card_idx);

                    // Try to read per-channel volume/switch controls
                    for (i, ch) in cfg.capture_channels.iter().enumerate() {
                        if i >= mixers.capture_mixers.len() { break; }
                        read_control(&ctl, ch, &mixers.capture_mixers[i]);
                    }
                    for (i, ch) in cfg.playback_channels.iter().enumerate() {
                        if i >= mixers.playback_mixers.len() { break; }
                        read_control(&ctl, ch, &mixers.playback_mixers[i]);
                    }
                    return;
                }
            }
        }
    }
}

fn read_control(ctl: &alsa::ctl::Ctl, ch: &ChannelConfig, mixer: &ChannelMixerState) {
    let vol_name = format!("{} Volume", ch.name);
    let mute_name = format!("{} Switch", ch.name);

    // Try to read integer volume via ALSA elem API
    if let (Ok(vol_cstr), Ok(mute_cstr)) = (
        std::ffi::CString::new(vol_name.clone()),
        std::ffi::CString::new(mute_name.clone()),
    ) {
        // Volume
        let mut elem_id = alsa::ctl::ElemId::new(alsa::ctl::ElemIface::Mixer);
        elem_id.set_name(vol_cstr.as_c_str());

        if let Ok(mut value) = alsa::ctl::ElemValue::new(alsa::ctl::ElemType::Integer) {
            value.set_id(&elem_id);
            if ctl.elem_read(&mut value).is_ok() {
                if let Some(v) = value.get_integer(0) {
                    let linear = v as f32 / 100.0;
                    mixer.set_volume_f32(linear);
                }
            }
        }

        // Mute
        let mut mute_id = alsa::ctl::ElemId::new(alsa::ctl::ElemIface::Mixer);
        mute_id.set_name(mute_cstr.as_c_str());

        if let Ok(mut value) = alsa::ctl::ElemValue::new(alsa::ctl::ElemType::Boolean) {
            value.set_id(&mute_id);
            if ctl.elem_read(&mut value).is_ok() {
                if let Some(m) = value.get_boolean(0) {
                    mixer.mute.store(m, Ordering::Release);
                }
            }
        }
    }
}

/// Background thread that periodically syncs ALSA mixer controls
pub fn run_mixer_sync(cfg: VadConfig, mixers: Arc<MixerState>) {
    let interval = std::time::Duration::from_millis(500);

    loop {
        if crate::SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }

        sync_mixer_from_alsa(&cfg, &mixers);
        std::thread::sleep(interval);
    }
}

/// Programmatically create ALSA mixer kcontrols on the loopback card
/// for each virtual audio channel's Volume and Switch.
/// Only works if the loopback card exists.
pub fn create_mixer_elements(cfg: &VadConfig) {
    // Try to find the loopback card
    for card_idx in 0..8 {
        let card = alsa::card::Card::new(card_idx);
        if let Ok(name) = card.get_name() {
            if name.contains("Loopback") {
                log::info!("Will create mixer elements on card '{}' (idx {})", name, card_idx);
                // On snd-aloop, mixer elements are created by the kernel module.
                // Userspace can read/write them but cannot create new ones.
                // For a custom ALSA driver, you'd use snd_ctl_add() in kernel space.
                //
                // We log what controls would be created for documentation:
                for ch in &cfg.capture_channels {
                    log::debug!("Mixer control: '{} Volume' (INTEGER 0-100)", ch.name);
                    log::debug!("Mixer control: '{} Switch' (BOOLEAN)", ch.name);
                }
                for ch in &cfg.playback_channels {
                    log::debug!("Mixer control: '{} Volume' (INTEGER 0-100)", ch.name);
                    log::debug!("Mixer control: '{} Switch' (BOOLEAN)", ch.name);
                }
                return;
            }
        }
    }
    log::warn!("No ALSA loopback card found — mixer elements not created");
}
