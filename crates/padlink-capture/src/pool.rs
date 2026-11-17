//! Capture buffers lent to the encoder without copying.
//!
//! Each shared-memory buffer is mapped once. A finished frame goes out as a
//! [`Lease`] on its buffer; dropping the lease puts the buffer back on the
//! free list. The compositor is only ever given buffers from the free list,
//! so it never writes into pixels someone is still reading.
//!
//! Adapted from OpenDisplay's `od-sender/src/wayland/shm_pool.rs` (GPL-3.0).

use std::fs::File;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use memmap2::Mmap;

/// Map `len` bytes of a shared-memory file for reading.
pub fn map(file: &File, len: usize) -> std::io::Result<Arc<Mmap>> {
    // SAFETY: the file is an anonymous memory file this process created and
    // sized; nothing truncates it. The compositor writes to it only while
    // the buffer is attached, and it is read only while leased, never both.
    #[allow(unsafe_code)]
    let mapping = unsafe { memmap2::MmapOptions::new().len(len).map(file)? };
    Ok(Arc::new(mapping))
}

#[derive(Default)]
struct State {
    free: Vec<usize>,
    /// Bumped when the buffers are reallocated; a lease from an older
    /// generation is not returned to the free list.
    generation: u64,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    returned: Condvar,
}

/// The free list. Indices refer to the capture loop's own buffer list.
#[derive(Clone, Default)]
pub struct Recycler(Arc<Shared>);

impl Recycler {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A buffer that is neither lent out nor attached.
    pub fn take(&self) -> Option<usize> {
        self.state().free.pop()
    }

    /// Wait up to `timeout` for a lease to come back. True if one did.
    pub fn wait(&self, timeout: Duration) -> bool {
        let state = self.state();
        let (state, _) = self
            .0
            .returned
            .wait_timeout_while(state, timeout, |s| s.free.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        !state.free.is_empty()
    }

    /// Return a buffer that was attached but never lent out.
    pub fn release(&self, index: usize) {
        self.state().free.push(index);
        self.0.returned.notify_one();
    }

    /// Forget every buffer: they are being reallocated at a new size.
    pub fn reset(&self) {
        let mut state = self.state();
        state.free.clear();
        state.generation += 1;
    }

    fn ticket(&self, index: usize) -> Ticket {
        Ticket {
            recycler: self.clone(),
            index,
            generation: self.state().generation,
        }
    }

    /// Lend out buffer `index`, whose first `len` bytes of `map` are pixels.
    pub fn lease(&self, index: usize, map: Arc<Mmap>, len: usize) -> Lease {
        assert!(len <= map.len());
        Lease {
            _ticket: self.ticket(index),
            map,
            len,
        }
    }

    /// Lend out GPU buffer `index`.
    pub fn lease_gpu(&self, index: usize, buffer: Arc<GpuBuffer>) -> GpuLease {
        GpuLease {
            _ticket: self.ticket(index),
            buffer,
        }
    }
}

/// Puts its buffer back on the free list when dropped.
struct Ticket {
    recycler: Recycler,
    index: usize,
    generation: u64,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.recycler.state();
        if state.generation == self.generation {
            state.free.push(self.index);
            drop(state);
            self.recycler.0.returned.notify_one();
        }
    }
}

/// Read access to one frame's pixels in shared memory. Dropping it frees
/// the buffer for the next frame, so hold it only as long as it is needed.
pub struct Lease {
    _ticket: Ticket,
    map: Arc<Mmap>,
    len: usize,
}

impl std::ops::Deref for Lease {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.map[..self.len]
    }
}

/// A single-plane GPU buffer (dmabuf) holding a 32-bit RGB picture.
#[derive(Debug)]
pub struct GpuBuffer {
    pub fd: std::os::fd::OwnedFd,
    /// The DRM format code, such as `XR24`.
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// Access to one frame that stayed on the GPU. Dropping it frees the buffer
/// for the next frame.
pub struct GpuLease {
    _ticket: Ticket,
    buffer: Arc<GpuBuffer>,
}

impl std::ops::Deref for GpuLease {
    type Target = GpuBuffer;

    fn deref(&self) -> &GpuBuffer {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn mapping(bytes: &[u8]) -> Arc<Mmap> {
        let mut file = tempfile();
        file.write_all(bytes).unwrap();
        map(&file, bytes.len()).unwrap()
    }

    fn tempfile() -> File {
        let path = std::env::temp_dir().join(format!(
            "padlink-pool-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let _ = std::fs::remove_file(&path);
        file
    }

    #[test]
    fn a_dropped_lease_frees_its_buffer() {
        let r = Recycler::default();
        let map = mapping(b"pixels!!");
        assert_eq!(r.take(), None);
        let lease = r.lease(1, map, 6);
        assert_eq!(&*lease, b"pixels");
        assert_eq!(r.take(), None);
        drop(lease);
        assert_eq!(r.take(), Some(1));
        assert_eq!(r.take(), None);
    }

    #[test]
    fn a_lease_from_before_a_reset_does_not_come_back() {
        let r = Recycler::default();
        let lease = r.lease(0, mapping(b"old size"), 8);
        r.reset();
        drop(lease);
        assert_eq!(r.take(), None);
    }

    #[test]
    fn a_released_buffer_is_free_again() {
        let r = Recycler::default();
        r.release(2);
        assert!(r.wait(Duration::ZERO));
        assert_eq!(r.take(), Some(2));
        assert!(!r.wait(Duration::from_millis(1)));
    }

    #[test]
    fn waiting_ends_when_another_thread_drops_its_lease() {
        let r = Recycler::default();
        let lease = r.lease(0, mapping(b"frame"), 5);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(lease);
        });
        assert!(r.wait(Duration::from_secs(5)));
        handle.join().unwrap();
    }
}
