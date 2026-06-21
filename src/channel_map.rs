//! Channel mapping — maps RODE USB audio channels to virtual ALSA channels.
//!
//! The Windows driver maps 11 USB capture channels + 5 USB playback channels
//! to individual KS filter pins. This module provides the same mapping
//! for the Linux implementation.

/// Channel mapping: USB endpoint address → virtual ALSA channel index
#[derive(Debug, Clone)]
pub struct ChannelMap {
    /// Capture mappings: (USB IN endpoint addr, ALSA capture channel index, friendly name)
    pub capture_map: Vec<(u8, usize, String)>,
    /// Playback mappings: (USB OUT endpoint addr, ALSA playback channel index, friendly name)
    pub playback_map: Vec<(u8, usize, String)>,
}

impl Default for ChannelMap {
    fn default() -> Self {
        Self {
            capture_map: vec![
                // These endpoint addresses are guesses — actual addresses
                // depend on the USB descriptor topology of the RODE hardware
                (0x81, 0,  "System In".into()),
                (0x82, 1,  "Combo1 In".into()),
                (0x83, 2,  "Combo2 In".into()),
                (0x84, 3,  "Combo3 In".into()),
                (0x85, 4,  "Combo4 In".into()),
                (0x86, 5,  "Bluetooth In".into()),
                (0x87, 6,  "SmartPads In".into()),
                (0x88, 7,  "USB1 Main In".into()),
                (0x89, 8,  "USB1 Chat In".into()),
                (0x8A, 9,  "USB2 In".into()),
                (0x8B, 10, "Headset In".into()),
            ],
            playback_map: vec![
                (0x01, 0, "System Out".into()),
                (0x02, 1, "Music Out".into()),
                (0x03, 2, "Game Out".into()),
                (0x04, 3, "Virtual A Out".into()),
                (0x05, 4, "Virtual B Out".into()),
            ],
        }
    }
}
