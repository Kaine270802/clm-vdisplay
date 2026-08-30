use std::ptr::NonNull;
use tracing::info;
use x11rb::connection::Connection;
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// RAII wrapper around SysV Shared Memory segment attached to an X11 server via MIT-SHM
pub struct ShmSegment {
    pub shmid: i32,
    pub shmseg: shm::Seg,
    pub size: usize,
    pub ptr: NonNull<u8>,
    is_attached: bool,
}

// Safety: The SysV memory segment is mapped into the process virtual address space
// and can be safely sent across threads.
unsafe impl Send for ShmSegment {}
unsafe impl Sync for ShmSegment {}

impl ShmSegment {
    /// Allocate a SysV IPC shared memory segment and attach it to the X11 server
    pub fn create(conn: &RustConnection, size: usize) -> anyhow::Result<Self> {
        // 1. Allocate SysV Shared Memory Segment
        let shmid = unsafe {
            libc::shmget(
                libc::IPC_PRIVATE,
                size,
                libc::IPC_CREAT | 0o777,
            )
        };
        if shmid < 0 {
            return Err(anyhow::anyhow!(
                "Failed to allocate SysV SHM (size={}): {}",
                size,
                std::io::Error::last_os_error()
            ));
        }

        // 2. Attach memory to current process address space
        let addr = unsafe { libc::shmat(shmid, std::ptr::null(), 0) };
        if addr == (-1isize as *mut libc::c_void) {
            unsafe {
                libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
            }
            return Err(anyhow::anyhow!(
                "Failed to attach SysV SHM pointer (shmid={}): {}",
                shmid,
                std::io::Error::last_os_error()
            ));
        }

        // 3. Mark segment for destruction on last detach (prevents orphan SHM leaks on panic/exit)
        unsafe {
            libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
        }

        // 4. Attach segment to X11 server via MIT-SHM extension
        let shmseg = conn.generate_id()?;
        conn.shm_attach(shmseg, shmid as u32, false)?.check()?;

        info!(
            "MIT-SHM segment created: shmid={}, shmseg={}, size={} bytes",
            shmid, shmseg, size
        );

        Ok(Self {
            shmid,
            shmseg,
            size,
            ptr: NonNull::new(addr as *mut u8).expect("non-null pointer from shmat"),
            is_attached: true,
        })
    }

    /// Obtain immutable slice to shared framebuffer memory
    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.size) }
    }

    /// Obtain mutable slice to shared framebuffer memory
    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.size) }
    }

    /// Detach the shared memory segment from X server and local process
    pub fn detach(&mut self, conn: &RustConnection) {
        if self.is_attached {
            self.is_attached = false;
            let _ = conn.shm_detach(self.shmseg);
            unsafe {
                libc::shmdt(self.ptr.as_ptr() as *const libc::c_void);
            }
            info!("Detached MIT-SHM segment (shmid={})", self.shmid);
        }
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        if self.is_attached {
            unsafe {
                libc::shmdt(self.ptr.as_ptr() as *const libc::c_void);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_sysv_shm_alloc_and_detach() {
        let size = 1024 * 64;
        let shmid = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o777) };
        assert!(shmid >= 0);

        let addr = unsafe { libc::shmat(shmid, std::ptr::null(), 0) };
        assert_ne!(addr, -1isize as *mut libc::c_void);

        unsafe {
            libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
            libc::shmdt(addr);
        }
    }
}
