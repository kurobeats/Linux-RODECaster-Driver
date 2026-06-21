//! USB Audio Class 2.0 descriptor parser.
//!
//! Parses USB interface descriptors to discover the audio streaming
//! topology (endpoint addresses, sample rates, channel counts) at runtime.
//! This replaces the hardcoded endpoint guesses in channel_map.rs.
//!
//! Based on USB Device Class Definition for Audio Devices 2.0
//! (USB-IF, May 31, 2006 — Revision 2.0).

use anyhow::{bail, Context, Result};
use log::{debug, info, warn};
use rusb::{Device, DeviceDescriptor, DeviceHandle, UsbContext};

/// USB Audio Class 2.0 interface subclass codes
mod uac2 {
    pub const AUDIOCONTROL: u8 = 0x01;
    pub const AUDIOSTREAMING: u8 = 0x03;
}

/// USB Audio Class 2.0 descriptor types
mod desc_types {
    pub const CS_INTERFACE: u8 = 0x24;
    pub const CS_ENDPOINT: u8 = 0x25;
    pub const CLOCK_SOURCE: u8 = 0x0A;
    pub const INPUT_TERMINAL: u8 = 0x02;
    pub const OUTPUT_TERMINAL: u8 = 0x03;
    pub const FEATURE_UNIT: u8 = 0x06;
    pub const AS_GENERAL: u8 = 0x01;
    pub const FORMAT_TYPE: u8 = 0x02;
}

/// A parsed USB audio streaming endpoint
#[derive(Debug, Clone)]
pub struct AudioEndpoint {
    /// USB endpoint address (0x8X = IN, 0x0X = OUT)
    pub endpoint_address: u8,
    /// Maximum packet size (in bytes)
    pub max_packet_size: u16,
    /// Sample rate supported by this endpoint
    pub sample_rates: Vec<u32>,
    /// Audio channels per sample
    pub channels: u8,
    /// Bit resolution (16, 24, 32)
    pub bit_resolution: u8,
    /// Subslot size (bytes per audio subslot)
    pub subslot_size: u8,
}

/// A parsed USB audio streaming interface
#[derive(Debug, Clone)]
pub struct AudioStreamingInterface {
    /// Interface number
    pub interface_number: u8,
    /// Alternate setting that carries the active stream
    pub alt_setting: u8,
    /// Direction: "capture" (IN) or "playback" (OUT)
    pub direction: String,
    /// Is this a vendor-specific extension?
    pub is_vendor_specific: bool,
    /// Parsed audio endpoints on this interface
    pub endpoints: Vec<AudioEndpoint>,
}

/// Topology for one virtual audio channel discovered from USB descriptors
#[derive(Debug, Clone)]
pub struct DiscoveredChannel {
    /// Friendly name (derived from terminal descriptor strings if available)
    pub name: String,
    /// Direction: "capture" or "playback"
    pub direction: String,
    /// USB endpoint address for this channel
    pub endpoint_address: u8,
    /// Audio channels (1=mono, 2=stereo)
    pub audio_channels: u8,
    /// Supported sample rates
    pub sample_rates: Vec<u32>,
    /// Bit resolution
    pub bit_resolution: u8,
    /// Associated interface number
    pub interface_number: u8,
    /// Alternate setting for active streaming
    pub alt_setting: u8,
}

/// Parse all audio streaming interfaces from a USB device
pub fn parse_audio_topology<T: UsbContext>(
    device: &Device<T>,
    handle: &DeviceHandle<T>,
) -> Result<Vec<DiscoveredChannel>> {
    let desc = device.device_descriptor()
        .context("Failed to get device descriptor")?;
    let config = device.active_config_descriptor()
        .context("Failed to get active config descriptor")?;

    let mut channels = Vec::new();

    for interface in config.interfaces() {
        for desc in interface.descriptors() {
            // Only process audio streaming interfaces (class = 0x01 Audio, subclass = 0x03 Streaming)
            if desc.class_code() != 0x01 || desc.sub_class_code() != uac2::AUDIOSTREAMING {
                // Also check vendor-specific interfaces that might carry audio
                if desc.class_code() != 0xFF || desc.sub_class_code() != uac2::AUDIOSTREAMING {
                    continue;
                }
            }

            let is_vendor = desc.class_code() == 0xFF;
            debug!(
                "Found audio streaming interface {} alt {} (vendor={})",
                desc.interface_number(),
                desc.setting_number(),
                is_vendor
            );

            // Parse the class-specific audio streaming descriptors
            let alt_setting = desc.setting_number();
            let interface_num = desc.interface_number();

            // Determine direction from endpoint address
            for endpoint in desc.endpoint_descriptors() {
                let ep_addr = endpoint.address();
                let direction = if ep_addr & 0x80 != 0 {
                    "capture" // IN endpoint: device → host
                } else {
                    "playback" // OUT endpoint: host → device
                };

                // Try to extract extra descriptor bytes for class-specific data
                let extra = desc.extra();
                let (sample_rates, num_ch, bit_res, subslot) =
                    parse_format_descriptor(extra);

                let max_packet = endpoint.max_packet_size();

                let _ep = AudioEndpoint {
                    endpoint_address: ep_addr,
                    max_packet_size: max_packet,
                    sample_rates: sample_rates.clone(),
                    channels: num_ch,
                    bit_resolution: bit_res,
                    subslot_size: subslot,
                };

                let channel = DiscoveredChannel {
                    name: format!(
                        "{} {:02x}",
                        if direction == "capture" { "Capture" } else { "Playback" },
                        ep_addr
                    ),
                    direction: direction.to_string(),
                    endpoint_address: ep_addr,
                    audio_channels: num_ch,
                    sample_rates: sample_rates,
                    bit_resolution: bit_res,
                    interface_number: interface_num,
                    alt_setting,
                };

                info!(
                    "Discovered {} channel: EP 0x{:02x}, {}ch, {}bit, @{:?}Hz, {}B/pkt",
                    channel.direction,
                    channel.endpoint_address,
                    channel.audio_channels,
                    channel.bit_resolution,
                    channel.sample_rates,
                    max_packet
                );

                channels.push(channel);
            }
        }
    }

    if channels.is_empty() {
        warn!("No USB Audio Class streaming endpoints found. Falling back to hardcoded channel map.");
    }

    Ok(channels)
}

/// Parse the USB Audio 2.0 Type I Format Descriptor from extra bytes
/// Returns (sample_rates, channels, bit_resolution, subslot_size)
fn parse_format_descriptor(extra: &[u8]) -> (Vec<u32>, u8, u8, u8) {
    if extra.is_empty() {
        // Default to stereo 48kHz 32-bit if we can't parse
        return (vec![48000], 2, 32, 4);
    }

    let mut pos = 0;
    while pos + 3 <= extra.len() {
        let length = extra[pos] as usize;
        let desc_type = extra[pos + 1];
        let desc_subtype = extra[pos + 2];

        if length == 0 || pos + length > extra.len() {
            break;
        }

        // CS_INTERFACE + FORMAT_TYPE = Audio 2.0 format descriptor
        if desc_type == desc_types::CS_INTERFACE && desc_subtype == desc_types::FORMAT_TYPE {
            if length >= 6 {
                let format_code = extra[pos + 3];
                let channels = extra[pos + 4];
                let subslot_size = if length >= 7 { extra[pos + 5] } else { 4 };
                let bit_resolution = if length >= 8 { extra[pos + 6] } else { subslot_size * 8 };

                let mut sample_rates = Vec::new();
                let mut data_pos = pos + 7;

                // Sample rate list
                if data_pos < pos + length {
                    let num_rates = extra[data_pos] as usize;
                    data_pos += 1;

                    for _ in 0..num_rates {
                        if data_pos + 4 <= pos + length {
                            let rate = u32::from_le_bytes([
                                extra[data_pos],
                                extra[data_pos + 1],
                                extra[data_pos + 2],
                                extra[data_pos + 3],
                            ]);
                            sample_rates.push(rate);
                            data_pos += 4;
                        }
                    }

                    if sample_rates.is_empty() && data_pos + 3 <= pos + length {
                        // Single fixed rate
                        let rate = u32::from_le_bytes([
                            extra[data_pos],
                            extra[data_pos + 1],
                            extra[data_pos + 2],
                            extra[data_pos + 3],
                        ]);
                        sample_rates.push(rate);
                    }
                }

                if sample_rates.is_empty() {
                    sample_rates.push(48000); // fallback
                }

                return (sample_rates, channels, bit_resolution, subslot_size);
            }
        }

        pos += length;
    }

    // Default fallback
    (vec![48000], 2, 32, 4)
}

/// Print all USB descriptors for a device (debug helper)
pub fn dump_descriptors<T: UsbContext>(device: &Device<T>, handle: &DeviceHandle<T>) -> Result<()> {
    let desc = device.device_descriptor()?;
    info!("=== USB Device Descriptors ===");
    info!("  Vendor: 0x{:04x}, Product: 0x{:04x}", desc.vendor_id(), desc.product_id());
    info!("  Class: 0x{:02x}, SubClass: 0x{:02x}, Protocol: 0x{:02x}",
        desc.class_code(), desc.sub_class_code(), desc.protocol_code());

    let config = device.active_config_descriptor()?;
    info!("  Active configuration: {}", config.number());

    for interface in config.interfaces() {
        for desc in interface.descriptors() {
            info!("  Interface {} Alt {}: class=0x{:02x} sub=0x{:02x} proto=0x{:02x}",
                desc.interface_number(),
                desc.setting_number(),
                desc.class_code(),
                desc.sub_class_code(),
                desc.protocol_code()
            );

            for ep in desc.endpoint_descriptors() {
                info!("    Endpoint 0x{:02x}: max_pkt={}, interval={}, type={:?}",
                    ep.address(),
                    ep.max_packet_size(),
                    ep.interval(),
                    ep.transfer_type()
                );
            }
        }
    }

    Ok(())
}
