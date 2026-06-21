//! Audio format definitions — mirrors the Windows KS data formats.
//!
//! Supported formats from reverse engineering:
//!   - KSDATAFORMAT_WAVEFORMATEX (PCM)
//!   - KSDATAFORMAT_SPECIFIER_DSOUND (DirectSound float)
//!   - Sample rates: 44100, 48000
//!   - Bit depths: 16, 24, 32-bit integer; 32-bit float
//!   - Channels: 1-8 (MONO through 7.1 SURROUND)

use anyhow::{bail, Result};

/// Supported audio sample formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// Signed 16-bit little-endian PCM
    S16LE,
    /// Signed 24-bit little-endian PCM (packed in 3 bytes)
    S24LE,
    /// Signed 32-bit little-endian PCM
    S32LE,
    /// 32-bit float little-endian
    FloatLE,
}

impl SampleFormat {
    /// Bytes per sample (per channel)
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            SampleFormat::S16LE => 2,
            SampleFormat::S24LE => 3,
            SampleFormat::S32LE => 4,
            SampleFormat::FloatLE => 4,
        }
    }

    /// Convert to ALSA format constant
    pub fn to_alsa_format(&self) -> alsa::pcm::Format {
        match self {
            SampleFormat::S16LE   => alsa::pcm::Format::S16LE,
            SampleFormat::S24LE   => alsa::pcm::Format::S243LE,
            SampleFormat::S32LE   => alsa::pcm::Format::S32LE,
            SampleFormat::FloatLE => alsa::pcm::Format::FloatLE,
        }
    }

    pub fn from_alsa_format(f: alsa::pcm::Format) -> Result<Self> {
        match f {
            alsa::pcm::Format::S16LE   => Ok(SampleFormat::S16LE),
            alsa::pcm::Format::S243LE  => Ok(SampleFormat::S24LE),
            alsa::pcm::Format::S32LE   => Ok(SampleFormat::S32LE),
            alsa::pcm::Format::FloatLE => Ok(SampleFormat::FloatLE),
            _ => bail!("Unsupported ALSA format: {:?}", f),
        }
    }
}

/// Audio stream configuration for a single PCM device
#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
}

impl AudioFormat {
    /// Default format matching the Windows driver's primary mode
    pub fn default_stereo() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            format: SampleFormat::S32LE,
        }
    }

    /// Frame size in bytes (= channels * bytes_per_sample)
    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * self.format.bytes_per_sample()
    }

    /// Convert to ALSA PCM hardware parameters
    pub fn apply_to_pcm(
        &self,
        pcm: &alsa::pcm::PCM,
        period_frames: usize,
        num_periods: usize,
    ) -> Result<()> {
        let hwp = alsa::pcm::HwParams::any(pcm)?;
        hwp.set_access(alsa::pcm::Access::RWInterleaved)?;
        hwp.set_format(self.format.to_alsa_format())?;
        hwp.set_rate(self.sample_rate, alsa::ValueOr::Nearest)?;
        hwp.set_channels(self.channels as u32)?;
        hwp.set_rate_resample(false)?;
        // Set period size and buffer size for low-latency event-driven mode
        hwp.set_period_size(period_frames as alsa::pcm::Frames, alsa::ValueOr::Nearest)?;
        hwp.set_buffer_size((period_frames * num_periods) as alsa::pcm::Frames)?;
        hwp.set_periods(num_periods as u32, alsa::ValueOr::Nearest)?;
        pcm.hw_params(&hwp)?;
        Ok(())
    }

    /// Convert to ALSA software parameters (for period size etc.)
    pub fn apply_sw_params(
        &self,
        pcm: &alsa::pcm::PCM,
        period_frames: usize,
        _num_periods: usize,
    ) -> Result<()> {
        let swp = pcm.sw_params_current()?;
        // Start the stream as soon as one period is available
        swp.set_start_threshold(period_frames as alsa::pcm::Frames)?;
        swp.set_avail_min(period_frames as alsa::pcm::Frames)?;
        pcm.sw_params(&swp)?;
        Ok(())
    }

    // --- Sample format conversion utilities ---

    /// Convert raw byte buffer to interleaved i32 samples (normalized internal format)
    pub fn bytes_to_i32(&self, bytes: &[u8]) -> Vec<i32> {
        let bps = self.format.bytes_per_sample();
        let num_samples = bytes.len() / bps;
        let mut samples = Vec::with_capacity(num_samples);

        match self.format {
            SampleFormat::S16LE => {
                for chunk in bytes.chunks_exact(2) {
                    let val = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
                    samples.push(val.saturating_mul(65536));
                }
            }
            SampleFormat::S24LE => {
                for chunk in bytes.chunks_exact(3) {
                    let val = ((chunk[2] as i8 as i32) << 24)
                        | ((chunk[1] as i32) << 16)
                        | ((chunk[0] as i32) << 8);
                    samples.push(val >> 8);
                }
            }
            SampleFormat::S32LE => {
                for chunk in bytes.chunks_exact(4) {
                    samples.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            SampleFormat::FloatLE => {
                for chunk in bytes.chunks_exact(4) {
                    let f = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    samples.push((f.clamp(-1.0, 1.0) * 2147483647.0) as i32);
                }
            }
        }
        samples
    }

    /// Convert interleaved i32 samples back to raw bytes in this format
    pub fn i32_to_bytes(&self, samples: &[i32]) -> Vec<u8> {
        match self.format {
            SampleFormat::S16LE => {
                let mut bytes = Vec::with_capacity(samples.len() * 2);
                for s in samples {
                    let v = (*s / 65536).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                bytes
            }
            SampleFormat::S24LE => {
                let mut bytes = Vec::with_capacity(samples.len() * 3);
                for s in samples {
                    let v = *s >> 8;
                    let le = v.to_le_bytes();
                    bytes.extend_from_slice(&le[..3]);
                }
                bytes
            }
            SampleFormat::S32LE => {
                let mut bytes = Vec::with_capacity(samples.len() * 4);
                for s in samples {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                bytes
            }
            SampleFormat::FloatLE => {
                let mut bytes = Vec::with_capacity(samples.len() * 4);
                for s in samples {
                    let f = *s as f32 / 2147483647.0;
                    bytes.extend_from_slice(&f.clamp(-1.0, 1.0).to_le_bytes());
                }
                bytes
            }
        }
    }
}
