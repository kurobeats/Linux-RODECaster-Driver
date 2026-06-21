//! Lock-free ring buffer for audio data using POSIX shared memory.
//!
//! Mirrors the Windows `CSharedMemoryGlobalWriter` pattern.
//! Uses a multi-producer single-consumer bounded queue per channel.
//!
//! Layout in shared memory:
//! ```
//! [RingBufHeader (64 bytes)] [audio data ring... (buffer_frames * frame_bytes)]
//! ```

use crate::audio_format::AudioFormat;
use crate::config::VadConfig;
use crate::shm::ShmRegion;
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Magic identifier for validation
pub const RINGBUF_MAGIC: u32 = 0x524F4445; // "RODE"

/// Header at the start of each shared memory region
#[repr(C)]
#[derive(Debug)]
pub struct RingBufHeader {
    /// Magic number for validation (0x524F4445 = "RODE")
    pub magic: AtomicU32,
    /// Protocol version
    pub version: AtomicU32,
    /// Frame size in bytes (= channels * bytes_per_sample)
    pub frame_bytes: AtomicU32,
    /// Number of audio channels per frame
    pub channels: AtomicU32,
    /// Current sample rate in Hz
    pub sample_rate: AtomicU32,
    /// Producer write position (frames from start)
    pub write_index: AtomicU64,
    /// Consumer read position (frames from start)
    pub read_index: AtomicU64,
    /// Total buffer capacity in frames
    pub buffer_frames: AtomicU32,
    /// State flags (0=stopped, 1=running, 2=paused)
    pub flags: AtomicU32,
    /// Padding to 64 bytes (cache-line aligned)
    pub _padding: [u8; 12],
}

// Compile-time size check
const _: () = {
    if std::mem::size_of::<RingBufHeader>() != 64 {
        panic!("RingBufHeader must be exactly 64 bytes");
    }
};

impl RingBufHeader {
    pub fn new(format: &AudioFormat, buffer_frames: u32) -> Self {
        Self {
            magic: AtomicU32::new(RINGBUF_MAGIC),
            version: AtomicU32::new(1),
            frame_bytes: AtomicU32::new(format.frame_bytes() as u32),
            channels: AtomicU32::new(format.channels as u32),
            sample_rate: AtomicU32::new(format.sample_rate),
            write_index: AtomicU64::new(0),
            read_index: AtomicU64::new(0),
            buffer_frames: AtomicU32::new(buffer_frames),
            flags: AtomicU32::new(0), // stopped
            _padding: [0u8; 12],
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.magic.load(Ordering::Acquire) != RINGBUF_MAGIC {
            anyhow::bail!("Invalid ring buffer magic number");
        }
        Ok(())
    }
}

/// A single audio ring buffer backed by shared memory
pub struct RingBuffer {
    /// The POSIX shared memory region
    #[allow(dead_code)]
    shm: ShmRegion,
    /// Pointer to the header (in mapped memory)
    header: *mut RingBufHeader,
    /// Pointer to the data ring (after the header)
    data_ptr: *mut u8,
    /// Total data size in bytes (buffer_frames * frame_bytes)
    data_size: usize,
    /// Frame size in bytes
    frame_bytes: usize,
    /// Buffer capacity in frames
    buffer_frames: u32,
}

// RingBuffer is safe to send between threads (backed by shm)
unsafe impl Send for RingBuffer {}
unsafe impl Sync for RingBuffer {}

impl RingBuffer {
    /// Create a new ring buffer in shared memory
    pub fn create(name: &str, format: &AudioFormat, buffer_frames: u32) -> Result<Self> {
        let frame_bytes = format.frame_bytes();
        let header_size = std::mem::size_of::<RingBufHeader>();
        let data_size = buffer_frames as usize * frame_bytes;
        let total_size = header_size + data_size;

        let shm = ShmRegion::create(name, total_size)
            .context("Failed to create shared memory region")?;

        let ptr = shm.as_ptr() as *mut u8;

        // Initialize header
        let header = ptr as *mut RingBufHeader;
        unsafe {
            std::ptr::write(header, RingBufHeader::new(format, buffer_frames));
        }

        Ok(Self {
            shm,
            header,
            data_ptr: unsafe { ptr.add(header_size) },
            data_size,
            frame_bytes,
            buffer_frames,
        })
    }

    /// Open an existing ring buffer from shared memory
    pub fn open(name: &str) -> Result<Self> {
        let shm = ShmRegion::open(name)
            .context("Failed to open shared memory region")?;

        let ptr = shm.as_ptr() as *mut u8;
        let header = ptr as *mut RingBufHeader;

        // Validate
        unsafe {
            (*header).validate()?;
        }

        let frame_bytes = unsafe { (*header).frame_bytes.load(Ordering::Acquire) as usize };
        let buffer_frames = unsafe { (*header).buffer_frames.load(Ordering::Acquire) };
        let data_size = buffer_frames as usize * frame_bytes;
        let header_size = std::mem::size_of::<RingBufHeader>();

        Ok(Self {
            shm,
            header,
            data_ptr: unsafe { ptr.add(header_size) },
            data_size,
            frame_bytes,
            buffer_frames,
        })
    }

    /// Reference the header
    fn hdr(&self) -> &RingBufHeader {
        unsafe { &*self.header }
    }

    /// Check if buffer is in running state
    pub fn is_running(&self) -> bool {
        self.hdr().flags.load(Ordering::Acquire) == 1
    }

    /// Set running state
    pub fn set_running(&self, running: bool) {
        self.hdr().flags.store(if running { 1 } else { 0 }, Ordering::Release);
    }

    /// Write audio frames to the ring buffer (producer side)
    /// Returns number of frames actually written
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let frame_count = data.len() / self.frame_bytes;
        if frame_count == 0 {
            return Ok(0);
        }

        let write_idx = self.hdr().write_index.load(Ordering::Acquire) as usize;
        let read_idx = self.hdr().read_index.load(Ordering::Acquire) as usize;
        let buf_frames = self.buffer_frames as usize;

        // Calculate available space
        let used = if write_idx >= read_idx {
            write_idx - read_idx
        } else {
            (buf_frames - read_idx) + write_idx
        };
        let free = buf_frames.saturating_sub(used + 1); // +1 to avoid full==empty ambiguity

        let frames_to_write = frame_count.min(free);
        if frames_to_write == 0 {
            return Ok(0);
        }

        let bytes_to_write = frames_to_write * self.frame_bytes;
        let write_offset = (write_idx % buf_frames) * self.frame_bytes;

        unsafe {
            let dst = self.data_ptr.add(write_offset);
            if write_offset + bytes_to_write <= self.data_size {
                // Single contiguous write
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst, bytes_to_write);
            } else {
                // Wrap-around write
                let first_chunk = self.data_size - write_offset;
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst, first_chunk);
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(first_chunk),
                    self.data_ptr,
                    bytes_to_write - first_chunk,
                );
            }
        }

        self.hdr()
            .write_index
            .store((write_idx + frames_to_write) as u64, Ordering::Release);

        Ok(frames_to_write)
    }

    /// Read audio frames from the ring buffer (consumer side)
    /// Returns number of frames actually read
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let max_frames = buf.len() / self.frame_bytes;
        if max_frames == 0 {
            return Ok(0);
        }

        let write_idx = self.hdr().write_index.load(Ordering::Acquire) as usize;
        let read_idx = self.hdr().read_index.load(Ordering::Acquire) as usize;
        let buf_frames = self.buffer_frames as usize;

        let available = if write_idx >= read_idx {
            write_idx - read_idx
        } else {
            (buf_frames - read_idx) + write_idx
        };

        let frames_to_read = max_frames.min(available);
        if frames_to_read == 0 {
            return Ok(0);
        }

        let bytes_to_read = frames_to_read * self.frame_bytes;
        let read_offset = (read_idx % buf_frames) * self.frame_bytes;

        unsafe {
            let src = self.data_ptr.add(read_offset);
            if read_offset + bytes_to_read <= self.data_size {
                std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), bytes_to_read);
            } else {
                let first_chunk = self.data_size - read_offset;
                std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), first_chunk);
                std::ptr::copy_nonoverlapping(
                    self.data_ptr,
                    buf.as_mut_ptr().add(first_chunk),
                    bytes_to_read - first_chunk,
                );
            }
        }

        self.hdr()
            .read_index
            .store((read_idx + frames_to_read) as u64, Ordering::Release);

        Ok(frames_to_read)
    }

    /// Get available frame count for reading
    pub fn available_frames(&self) -> usize {
        let write_idx = self.hdr().write_index.load(Ordering::Acquire) as usize;
        let read_idx = self.hdr().read_index.load(Ordering::Acquire) as usize;
        let buf_frames = self.buffer_frames as usize;

        if write_idx >= read_idx {
            write_idx - read_idx
        } else {
            (buf_frames - read_idx) + write_idx
        }
    }

    /// Get the frame size in bytes
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// Reset the buffer (clear read/write indices)
    pub fn reset(&self) {
        self.hdr().write_index.store(0, Ordering::Release);
        self.hdr().read_index.store(0, Ordering::Release);
    }
}

/// Set of ring buffers — one per virtual audio channel
pub struct RingBufferSet {
    /// Capture ring buffers (USB hardware → virtual ALSA input)
    pub capture: Vec<RingBuffer>,
    /// Playback ring buffers (virtual ALSA output → USB hardware)
    pub playback: Vec<RingBuffer>,
}

impl RingBufferSet {
    /// Create all ring buffers in shared memory
    pub fn new(cfg: &VadConfig) -> Result<Self> {
        let format = crate::audio_format::AudioFormat {
            sample_rate: cfg.sample_rate,
            channels: 2,
            format: crate::audio_format::SampleFormat::S32LE,
        };

        let total_frames = cfg.period_frames * cfg.num_periods;

        let mut capture = Vec::new();
        for ch in &cfg.capture_channels {
            let shm_name = format!("rodecaster_vad_cap_{}", ch.alsa_id);
            log::debug!("Creating capture ring buffer: {}", shm_name);
            let rb = RingBuffer::create(&shm_name, &format, total_frames)
                .with_context(|| format!("Failed to create capture ring buffer for {}", ch.name))?;
            capture.push(rb);
        }

        let mut playback = Vec::new();
        for ch in &cfg.playback_channels {
            let shm_name = format!("rodecaster_vad_pb_{}", ch.alsa_id);
            log::debug!("Creating playback ring buffer: {}", shm_name);
            let rb = RingBuffer::create(&shm_name, &format, total_frames)
                .with_context(|| format!("Failed to create playback ring buffer for {}", ch.name))?;
            playback.push(rb);
        }

        Ok(Self { capture, playback })
    }

    /// Set all buffers to running
    pub fn set_all_running(&self) {
        for rb in &self.capture {
            rb.set_running(true);
        }
        for rb in &self.playback {
            rb.set_running(true);
        }
    }

    /// Stop all buffers
    pub fn stop_all(&self) {
        for rb in &self.capture {
            rb.set_running(false);
        }
        for rb in &self.playback {
            rb.set_running(false);
        }
    }
}
