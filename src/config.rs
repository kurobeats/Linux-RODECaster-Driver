//! Configuration for the RODECaster Virtual Audio Device.
//!
//! Maps the 11 capture + 5 playback channels from the Windows driver
//! to Linux ALSA PCM device specifications.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level daemon configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Number of audio frames per period (pull-mode equivalent)
    #[serde(default = "default_period_frames")]
    pub period_frames: u32,

    /// Number of periods in the ring buffer
    #[serde(default = "default_num_periods")]
    pub num_periods: u32,

    /// Default sample rate
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// RODE USB vendor ID
    #[serde(default = "default_vendor_id")]
    pub rode_vendor_id: u16,

    /// Capture (input) channel definitions
    pub capture_channels: Vec<ChannelConfig>,

    /// Playback (output) channel definitions
    pub playback_channels: Vec<ChannelConfig>,

    /// ALSA device name prefix for virtual devices
    #[serde(default = "default_alsa_prefix")]
    pub alsa_device_prefix: String,

    /// Shared memory directory
    #[serde(default = "default_shm_dir")]
    pub shm_dir: PathBuf,
}

/// Per-channel audio configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Human-readable name (e.g., "System In", "Music Out")
    pub name: String,

    /// ALSA PCM ID (unique per direction, e.g., "System", "Music")
    pub alsa_id: String,

    /// Number of audio channels (1=mono, 2=stereo)
    #[serde(default = "default_channels")]
    pub channels: u16,

    /// Direction: "capture" or "playback"
    pub direction: ChannelDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelDirection {
    Capture,
    Playback,
}

// Default values
fn default_period_frames() -> u32 { 256 }
fn default_num_periods() -> u32 { 4 }
fn default_sample_rate() -> u32 { 48000 }
fn default_vendor_id() -> u16 { 0x19F7 } // RODE Microphones
fn default_alsa_prefix() -> String { "RODECaster".to_string() }
fn default_shm_dir() -> PathBuf { PathBuf::from("/dev/shm") }
fn default_channels() -> u16 { 2 }

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            period_frames: default_period_frames(),
            num_periods: default_num_periods(),
            sample_rate: default_sample_rate(),
            rode_vendor_id: default_vendor_id(),
            capture_channels: build_capture_channels(),
            playback_channels: build_playback_channels(),
            alsa_device_prefix: default_alsa_prefix(),
            shm_dir: default_shm_dir(),
        }
    }
}

/// Build the 11 capture channel definitions (matching Windows INF topology — see Section 2.1)
pub fn build_capture_channels() -> Vec<ChannelConfig> {
    vec![
        // #  | INF Section | KS Name   | Friendly Name (from INF Strings)
        ChannelConfig { name: "System In".into(),     alsa_id: "System".into(),     channels: 2, direction: ChannelDirection::Capture },     // 1
        ChannelConfig { name: "Combo 1 In".into(),    alsa_id: "Combo1".into(),     channels: 2, direction: ChannelDirection::Capture },     // 2
        ChannelConfig { name: "Combo 2 In".into(),    alsa_id: "Combo2".into(),     channels: 2, direction: ChannelDirection::Capture },     // 3
        ChannelConfig { name: "Combo 3 In".into(),    alsa_id: "Combo3".into(),     channels: 2, direction: ChannelDirection::Capture },     // 4
        ChannelConfig { name: "Combo 4 In".into(),    alsa_id: "Combo4".into(),     channels: 2, direction: ChannelDirection::Capture },     // 5
        ChannelConfig { name: "Bluetooth In".into(),  alsa_id: "Bluetooth".into(),  channels: 2, direction: ChannelDirection::Capture },     // 6
        ChannelConfig { name: "Smart Pads In".into(), alsa_id: "SmartPads".into(),  channels: 2, direction: ChannelDirection::Capture },     // 7
        ChannelConfig { name: "USB1 Main In".into(),  alsa_id: "USB1Main".into(),   channels: 2, direction: ChannelDirection::Capture },     // 8
        ChannelConfig { name: "USB1 Chat In".into(),  alsa_id: "USB1Chat".into(),   channels: 2, direction: ChannelDirection::Capture },     // 9
        ChannelConfig { name: "USB2 In".into(),       alsa_id: "USB2".into(),       channels: 2, direction: ChannelDirection::Capture },     // 10
        ChannelConfig { name: "Headset In".into(),    alsa_id: "Headset".into(),    channels: 2, direction: ChannelDirection::Capture },     // 11
    ]
}

/// Build the 5 playback channel definitions (matching Windows INF topology — see Section 2.2)
pub fn build_playback_channels() -> Vec<ChannelConfig> {
    vec![
        // #  | INF Section         | KS Name   | Friendly Name (from INF Strings)
        ChannelConfig { name: "System Out".into(),    alsa_id: "System".into(),    channels: 2, direction: ChannelDirection::Playback },   // 1
        ChannelConfig { name: "Music Out".into(),     alsa_id: "Music".into(),     channels: 2, direction: ChannelDirection::Playback },   // 2
        ChannelConfig { name: "Game Out".into(),      alsa_id: "Game".into(),      channels: 2, direction: ChannelDirection::Playback },   // 3
        ChannelConfig { name: "Virtual A Out".into(), alsa_id: "VirtualA".into(),  channels: 2, direction: ChannelDirection::Playback },   // 4
        ChannelConfig { name: "Virtual B Out".into(), alsa_id: "VirtualB".into(),  channels: 2, direction: ChannelDirection::Playback },   // 5
    ]
}

/// Load configuration from file or built-in defaults
pub fn load_config() -> anyhow::Result<VadConfig> {
    use std::fs;

    let config_paths = [
        "/etc/rodecaster-vad/config.toml",
        "rodecaster-vad.toml",
    ];

    for path in &config_paths {
        if let Ok(contents) = fs::read_to_string(path) {
            log::info!("Loading configuration from {}", path);
            let mut cfg: VadConfig = toml::from_str(&contents)?;
            // Ensure channels are populated
            if cfg.capture_channels.is_empty() {
                cfg.capture_channels = build_capture_channels();
            }
            if cfg.playback_channels.is_empty() {
                cfg.playback_channels = build_playback_channels();
            }
            return Ok(cfg);
        }
    }

    log::info!("No config file found, using defaults");
    Ok(VadConfig::default())
}
