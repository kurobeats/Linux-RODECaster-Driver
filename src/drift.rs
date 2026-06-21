//! Drift compensation — monitors ring buffer fill levels to detect
//! clock drift between USB and ALSA subsystems.
//!
//! When the USB hardware runs at a slightly different sample rate than
//! the virtual ALSA devices, the ring buffers will slowly fill or drain.
//! This module provides metrics and diagnostics.
//!
//! Actual resampling correction (e.g., libsamplerate, Speex resampler) would
//! be implemented in the bridge loop based on these metrics.

use crate::ring_buffer::RingBuffer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Drift status for a single channel's ring buffer
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriftStatus {
    /// Buffer fill is within target range (±20% of half-full)
    Nominal { fill_pct: f32 },
    /// Buffer is draining (consumer faster than producer)
    Underflowing { fill_pct: f32, rate: f32 },
    /// Buffer is filling (producer faster than consumer)
    Overflowing { fill_pct: f32, rate: f32 },
    /// Buffer full — audio dropouts likely
    Full,
    /// Buffer empty — silence being played
    Empty,
}

impl DriftStatus {
    pub fn from_ring_buffer(rb: &RingBuffer, prev_fill: &mut f32, dt_secs: f32) -> Self {
        let fill = rb.fill_ratio();
        let rate = if dt_secs > 0.0 {
            (fill - *prev_fill) / dt_secs
        } else {
            0.0
        };
        *prev_fill = fill;

        let fill_pct = fill * 100.0;

        if fill < 0.01 {
            DriftStatus::Empty
        } else if fill > 0.99 {
            DriftStatus::Full
        } else if fill > 0.7 && rate > 0.02 {
            DriftStatus::Overflowing { fill_pct, rate }
        } else if fill < 0.2 && rate < -0.02 {
            DriftStatus::Underflowing { fill_pct, rate }
        } else {
            DriftStatus::Nominal { fill_pct }
        }
    }

    /// Returns true if corrective action may be needed
    pub fn needs_correction(&self) -> bool {
        matches!(self, DriftStatus::Underflowing { .. } | DriftStatus::Overflowing { .. })
    }

    /// Suggested resampling ratio adjustment.
    /// 1.0 = no change, >1.0 = speed up consumption, <1.0 = slow down.
    pub fn correction_ratio(&self) -> f32 {
        match self {
            DriftStatus::Underflowing { .. } => 0.9995, // slow down consumer slightly
            DriftStatus::Overflowing { .. } => 1.0005,   // speed up consumer slightly
            _ => 1.0,
        }
    }
}

/// Drift monitor for a full set of ring buffers
pub struct DriftMonitor {
    pub capture_fill_prev: Vec<f32>,
    pub playback_fill_prev: Vec<f32>,
    pub last_check: Instant,
}

impl DriftMonitor {
    pub fn new(num_capture: usize, num_playback: usize) -> Self {
        Self {
            capture_fill_prev: vec![0.0; num_capture],
            playback_fill_prev: vec![0.0; num_playback],
            last_check: Instant::now(),
        }
    }

    /// Check all capture buffers for drift
    pub fn check_capture(&mut self, buffers: &[RingBuffer]) -> Vec<DriftStatus> {
        let dt = self.last_check.elapsed().as_secs_f32();
        self.last_check = Instant::now();

        buffers.iter()
            .zip(self.capture_fill_prev.iter_mut())
            .map(|(rb, prev)| DriftStatus::from_ring_buffer(rb, prev, dt))
            .collect()
    }

    /// Check all playback buffers for drift
    pub fn check_playback(&mut self, buffers: &[RingBuffer]) -> Vec<DriftStatus> {
        let dt = self.last_check.elapsed().as_secs_f32();

        buffers.iter()
            .zip(self.playback_fill_prev.iter_mut())
            .map(|(rb, prev)| DriftStatus::from_ring_buffer(rb, prev, dt))
            .collect()
    }
}

/// Re-silence the last check timer (use when resuming from suspend)
pub fn reset_drift_timer() {
    // drift monitoring restarts after autosuspend
}
