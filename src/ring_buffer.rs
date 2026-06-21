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

    /// Get the buffer fill percentage (0.0 = empty, 1.0 = full).
    /// Used for drift compensation monitoring.
    pub fn fill_ratio(&self) -> f32 {
        let avail = self.available_frames();
        if self.buffer_frames == 0 {
            return 0.0;
        }
        avail as f32 / self.buffer_frames as f32
    }
}

/// Double-buffered ring buffer pair for ping-pong audio transfer.
/// While the consumer reads from one buffer, the producer writes to the other.
/// Matches the Windows driver's two shared memory regions pattern.
pub struct DoubleRingBuffer {
    pub front: RingBuffer,
    pub back: RingBuffer,
    /// Front buffer is active for reading
    pub front_active: std::sync::atomic::AtomicBool,
}

unsafe impl Send for DoubleRingBuffer {}
unsafe impl Sync for DoubleRingBuffer {}

impl DoubleRingBuffer {
    pub fn create(name: &str, format: &AudioFormat, buffer_frames: u32) -> Result<Self> {
        let front = RingBuffer::create(&format!("{}_{}", name, 0), format, buffer_frames)?;
        let back  = RingBuffer::create(&format!("{}_{}", name, 1), format, buffer_frames)?;
        Ok(Self {
            front,
            back,
            front_active: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// Write to the back buffer (producer side)
    pub fn write_back(&self, data: &[u8]) -> Result<usize> {
        self.back.write(data)
    }

    /// Read from the front buffer (consumer side)
    pub fn read_front(&self, buf: &mut [u8]) -> Result<usize> {
        self.front.read(buf)
    }

    /// Atomically swap front and back buffers at a period boundary
    pub fn swap(&self) {
        self.front_active.fetch_xor(true, Ordering::Release);
        let was_front = self.front_active.load(Ordering::Acquire);
        // Copy the back buffer indices into the front for the next read
        if was_front {
            let w = self.back.hdr().write_index.load(Ordering::Acquire);
            self.front.hdr().write_index.store(w, Ordering::Release);
            self.front.hdr().read_index.store(0, Ordering::Release);
        }
    }

    pub fn is_running(&self) -> bool { self.front.is_running() }

    pub fn set_running(&self, r: bool) {
        self.front.set_running(r);
        self.back.set_running(r);
    }

    pub fn available_frames(&self) -> usize { self.front.available_frames() }

    pub fn frame_bytes(&self) -> usize { self.front.frame_bytes() }

    pub fn fill_ratio(&self) -> f32 { self.front.fill_ratio() }

    pub fn reset(&self) { self.front.reset(); self.back.reset(); }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_format::{AudioFormat, SampleFormat};
    use std::sync::atomic::Ordering;

    fn test_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            format: SampleFormat::S32LE,
        }
    }

    #[test]
    fn test_ring_buffer_basic_write_read() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_basic", &fmt, 1024)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        assert_eq!(rb.frame_bytes(), 8); // 2ch * 4 bytes
        assert_eq!(rb.available_frames(), 0);

        // Write 256 frames (2048 bytes) of test data
        let data: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
        let written = rb.write(&data).expect("Write failed");
        assert_eq!(written, 256);

        assert_eq!(rb.available_frames(), 256);

        // Read back
        let mut read_buf = vec![0u8; 2048];
        let read = rb.read(&mut read_buf).expect("Read failed");
        assert_eq!(read, 256);

        assert_eq!(&data[..2048], &read_buf[..2048]);
        assert_eq!(rb.available_frames(), 0);
    }

    #[test]
    fn test_ring_buffer_wraparound() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_wrap", &fmt, 256)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        // Fill buffer to 200 frames
        let data_a = vec![0xAAu8; 200 * 8];
        let written = rb.write(&data_a).expect("Write A failed");
        assert_eq!(written, 200);

        // Read some so we have room at end + beginning (wraparound territory)
        let mut tmp = vec![0u8; 100 * 8];
        let _ = rb.read(&mut tmp);

        // Write more to force wraparound
        let data_b = vec![0xBBu8; 150 * 8];
        let written = rb.write(&data_b).expect("Write B failed");
        assert_eq!(written, 150);

        // Now read everything available
        let mut all = vec![0u8; 1024 * 8];
        let total_read = rb.read(&mut all).expect("Read all failed");

        // We should have read 100 (remaining A) + 150 (B) = 250 frames
        assert_eq!(total_read, 250);

        // Verify A tail (should be 0xAA)
        assert_eq!(all[0], 0xAA);
        assert_eq!(all[99 * 8], 0xAA);

        // Verify B head (should be 0xBB)
        assert_eq!(all[100 * 8], 0xBB);
    }

    #[test]
    fn test_ring_buffer_empty_read() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_empty", &fmt, 512)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        assert_eq!(rb.available_frames(), 0);

        let mut buf = vec![0u8; 1024];
        let read = rb.read(&mut buf).expect("Read empty failed");
        assert_eq!(read, 0);
    }

    #[test]
    fn test_ring_buffer_full_write() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_full", &fmt, 256)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        // Fill completely (leave 1 frame gap for full/empty distinction)
        let data = vec![0xCCu8; 256 * 8]; // 256 frames
        let written = rb.write(&data).expect("Write full failed");
        assert_eq!(written, 255); // 255 fits (1 frame gap preserved)

        let available = rb.available_frames();
        assert_eq!(available, 255);
    }

    #[test]
    fn test_ring_buffer_header_validation() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_header", &fmt, 256)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        let hdr = unsafe { &*rb.header };
        assert_eq!(hdr.magic.load(Ordering::Acquire), RINGBUF_MAGIC);
        assert_eq!(hdr.version.load(Ordering::Acquire), 1);
        assert_eq!(hdr.frame_bytes.load(Ordering::Acquire), 8);
        assert_eq!(hdr.channels.load(Ordering::Acquire), 2);
        assert_eq!(hdr.sample_rate.load(Ordering::Acquire), 48000);
        assert_eq!(hdr.buffer_frames.load(Ordering::Acquire), 256);
    }

    #[test]
    fn test_ring_buffer_reset() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_reset", &fmt, 256)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        let data = vec![0xDDu8; 100 * 8];
        let _ = rb.write(&data);
        assert!(rb.available_frames() > 0);

        rb.reset();
        assert_eq!(rb.available_frames(), 0);
    }

    #[test]
    fn test_ring_buffer_stop_flag() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_stop", &fmt, 256)
            .expect("Failed to create ring buffer");

        assert!(!rb.is_running());
        rb.set_running(true);
        assert!(rb.is_running());
        rb.set_running(false);
        assert!(!rb.is_running());
    }

    #[test]
    fn test_ring_buffer_large_transfer() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_large", &fmt, 8192)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        // Write 4096 frames in 4 chunks
        for _ in 0..4 {
            let chunk = vec![0xEFu8; 1024 * 8];
            let written = rb.write(&chunk).expect("Write chunk failed");
            assert_eq!(written, 1024);
        }
        assert_eq!(rb.available_frames(), 4096);

        // Read back 4096 frames
        let mut buf = vec![0u8; 4096 * 8];
        let read = rb.read(&mut buf).expect("Read large failed");
        assert_eq!(read, 4096);

        // All should be 0xEF
        for byte in &buf[..read * 8] {
            assert_eq!(*byte, 0xEF);
        }
        assert_eq!(rb.available_frames(), 0);
    }

    #[test]
    fn test_ring_buffer_partial_read() {
        let fmt = test_format();
        let rb = RingBuffer::create("/test_rb_partial", &fmt, 512)
            .expect("Failed to create ring buffer");

        rb.set_running(true);

        let data = vec![0x42u8; 300 * 8];
        let _ = rb.write(&data);
        assert_eq!(rb.available_frames(), 300);

        // Read only 100 frames
        let mut buf = vec![0u8; 100 * 8];
        let read = rb.read(&mut buf).expect("Partial read failed");
        assert_eq!(read, 100);
        assert_eq!(rb.available_frames(), 200);

        // Read remaining 200
        let mut buf2 = vec![0u8; 200 * 8];
        let read2 = rb.read(&mut buf2).expect("Second read failed");
        assert_eq!(read2, 200);
        assert_eq!(rb.available_frames(), 0);
    }
}
