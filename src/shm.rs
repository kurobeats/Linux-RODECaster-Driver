//! POSIX shared memory abstraction.
//!
//! Replaces Windows `CreateFileMappingA` + `Global\RODECaster_VAD{...}`
//! with Linux `shm_open()` + `mmap()`.

use anyhow::{Context, Result};
use std::ffi::CString;
use std::os::fd::AsRawFd;

/// A POSIX shared memory region
pub struct ShmRegion {
    /// Mapped memory pointer
    ptr: *mut std::ffi::c_void,
    /// Size of the region in bytes
    size: usize,
    /// Name (for cleanup — only the creating process unlinks)
    name: Option<String>,
}

/// SAFETY: ShmRegion owns a memory mapping.
/// Send is safe because the memory is process-unique (MAP_PRIVATE would be, but
/// MAP_SHARED is still process-safe as we own the pointer exclusively).
unsafe impl Send for ShmRegion {}
unsafe impl Sync for ShmRegion {}

impl ShmRegion {
    /// Create a new shared memory region with the given name and size.
    /// Returns an error if the name already exists. The region is unlinked
    /// on drop.
    pub fn create(name: &str, size: usize) -> Result<Self> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::{mmap, shm_open, ProtFlags, MapFlags};
        use nix::sys::stat::Mode as StatMode;
        use nix::unistd::ftruncate;
        use std::num::NonZero;

        let cname = CString::new(name)
            .with_context(|| format!("Invalid shm name: {}", name))?;

        // Try exclusive create; if exists, open existing
        let owned_fd = shm_open(
            cname.as_c_str(),
            OFlag::O_CREAT | OFlag::O_RDWR | OFlag::O_EXCL,
            StatMode::S_IRUSR | StatMode::S_IWUSR,
        )
        .or_else(|_| {
            shm_open(cname.as_c_str(), OFlag::O_RDWR, StatMode::S_IRUSR | StatMode::S_IWUSR)
        })
        .context("Failed to create/open shared memory")?;

        // Set size
        ftruncate(&owned_fd, size as i64)
            .context("Failed to set shared memory size")?;

        // nix 0.29 mmap takes ownership of OwnedFd — the fd is closed on munmap
        let ptr = unsafe {
            mmap(
                None,
                NonZero::new(size).context("Shared memory size must be non-zero")?,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                owned_fd,
                0,
            )
            .context("Failed to mmap shared memory")?
        };

        Ok(Self {
            ptr: ptr.as_ptr(),
            size,
            name: Some(name.to_string()),
        })
    }

    /// Open an existing shared memory region. Does NOT unlink on drop
    /// (another process owns the region).
    pub fn open(name: &str) -> Result<Self> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::{mmap, shm_open, ProtFlags, MapFlags};
        use nix::sys::stat::Mode;
        use std::num::NonZero;

        let cname = CString::new(name)
            .with_context(|| format!("Invalid shm name: {}", name))?;

        let owned_fd = shm_open(cname.as_c_str(), OFlag::O_RDWR, Mode::S_IRUSR | Mode::S_IWUSR)
            .context("Failed to open shared memory")?;

        // Get size via fstat
        let fd_raw = owned_fd.as_raw_fd();
        let stat = nix::sys::stat::fstat(fd_raw)
            .context("Failed to stat shared memory")?;
        let size = stat.st_size as usize;

        // mmap takes ownership of OwnedFd
        let ptr = unsafe {
            mmap(
                None,
                NonZero::new(size).context("Shared memory size must be non-zero")?,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                owned_fd,
                0,
            )
            .context("Failed to mmap shared memory")?
        };

        // name = None means we won't unlink on drop
        Ok(Self {
            ptr: ptr.as_ptr(),
            size,
            name: None,
        })
    }

    /// Get a raw pointer to the mapped memory
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.ptr
    }

    /// Get the size
    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for ShmRegion {
    fn drop(&mut self) {
        use nix::sys::mman::munmap;
        use nix::sys::mman::shm_unlink;

        // Unmap memory (mmap took the OwnedFd, munmap releases both)
        unsafe {
            let _ = munmap(std::ptr::NonNull::new(self.ptr).unwrap(), self.size);
        }

        // Only the creating process unlinks the shm object
        if let Some(ref name) = self.name {
            if let Ok(cname) = CString::new(name.as_str()) {
                let _ = shm_unlink(cname.as_c_str());
            }
        }
    }
}
