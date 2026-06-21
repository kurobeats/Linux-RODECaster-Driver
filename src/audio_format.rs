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
            SampleFormat::FloatLE => alsa::pcm::Format::FLOATLE,
        }
    }

    pub fn from_alsa_format(f: alsa::pcm::Format) -> Result<Self> {
        match f {
            alsa::pcm::Format::S16LE   => Ok(SampleFormat::S16LE),
            alsa::pcm::Format::S243LE  => Ok(SampleFormat::S24LE),
            alsa::pcm::Format::S32LE   => Ok(SampleFormat::S32LE),
            alsa::pcm::Format::FLOATLE => Ok(SampleFormat::FloatLE),
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
    pub fn apply_to_pcm(&self, pcm: &alsa::pcm::PCM) -> Result<()> {
        let hwp = alsa::pcm::HwParams::any(pcm)?;
        hwp.set_access(alsa::pcm::Access::RWInterleaved)?;
        hwp.set_format(self.format.to_alsa_format())?;
        hwp.set_rate(self.sample_rate, alsa::ValueOr::Nearest)?;
        hwp.set_channels(self.channels)?;
        hwp.set_rate_resample(false)?;
        pcm.hw_params(&hwp)?;
        Ok(())
    }

    /// Convert to ALSA software parameters (for period size etc.)
    pub fn apply_sw_params(
        &self,
        pcm: &alsa::pcm::PCM,
        period_frames: usize,
        num_periods: usize,
    ) -> Result<()> {
        let swp = alsa::pcm::SwParams::current(pcm)?;
        swp.set_start_threshold(period_frames as alsa::pcm::Frames)?;
        swp.set_avail_min(period_frames as alsa::pcm::Frames)?;
        pcm.sw_params(&swp)?;
        Ok(())
    }
}
