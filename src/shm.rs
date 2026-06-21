//! POSIX shared memory abstraction.
//!
//! Replaces Windows `CreateFileMappingA` + `Global\RODECaster_VAD{...}`
//! with Linux `shm_open()` + `mmap()`.

use anyhow::{Context, Result};
use std::ffi::CString;

/// A POSIX shared memory region
pub struct ShmRegion {
    /// Mapped memory pointer
    ptr: *mut std::ffi::c_void,
    /// Size of the region in bytes
    size: usize,
    /// File descriptor for the shm object
    fd: std::os::fd::RawFd,
    /// Name (for cleanup)
    name: String,
}

/// SAFETY: ShmRegion owns a memory mapping and fd.
/// Send is safe because the memory is process-unique.
unsafe impl Send for ShmRegion {}
unsafe impl Sync for ShmRegion {}

impl ShmRegion {
    /// Create a new shared memory region with the given name and size
    pub fn create(name: &str, size: usize) -> Result<Self> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::{mmap, shm_open, ProtFlags, MapFlags, Mode};
        use nix::sys::stat::Mode as StatMode;
        use nix::unistd::ftruncate;

        let cname = CString::new(name)
            .with_context(|| format!("Invalid shm name: {}", name))?;

        let fd = shm_open(
            cname.as_c_str(),
            OFlag::O_CREAT | OFlag::O_RDWR | OFlag::O_EXCL,
            StatMode::S_IRUSR | StatMode::S_IWUSR,
        )
        .or_else(|_| {
            // If exists, open it
            shm_open(cname.as_c_str(), OFlag::O_RDWR, StatMode::S_IRUSR | StatMode::S_IWUSR)
        })
        .context("Failed to create/open shared memory")?;

        // Set size
        ftruncate(fd, size as i64)
            .context("Failed to set shared memory size")?;

        // Map into memory
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                0,
            )
            .context("Failed to mmap shared memory")?
        };

        Ok(Self {
            ptr: ptr.as_ptr(),
            size,
            fd,
            name: name.to_string(),
        })
    }

    /// Open an existing shared memory region
    pub fn open(name: &str) -> Result<Self> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::{mmap, shm_open, ProtFlags, MapFlags};
        use nix::sys::stat::Mode;
        use nix::sys::stat::stat;

        let cname = CString::new(name)
            .with_context(|| format!("Invalid shm name: {}", name))?;

        let fd = shm_open(cname.as_c_str(), OFlag::O_RDWR, Mode::S_IRUSR | Mode::S_IWUSR)
            .context("Failed to open shared memory")?;

        // Get size from stat
        let stat = stat(cname.as_c_str())
            .context("Failed to stat shared memory")?;
        let size = stat.st_size as usize;

        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                0,
            )
            .context("Failed to mmap shared memory")?
        };

        Ok(Self {
            ptr: ptr.as_ptr(),
            size,
            fd,
            name: name.to_string(),
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
        use std::ffi::CString;

        // Unmap
        unsafe {
            let _ = munmap(std::ptr::NonNull::new_unchecked(self.ptr), self.size);
        }

        // Close fd
        unsafe {
            let _ = nix::unistd::close(self.fd);
        }

        // Remove the shared memory object
        if let Ok(cname) = CString::new(self.name.clone()) {
            let _ = shm_unlink(cname.as_c_str());
        }
    }
}
