//! The native backend's heap (docs/java-compat/compiler.md 4.3): one range
//! of address space reserved at a fixed place, handed out in chunks to each
//! thread's allocation buffer.
//!
//! Every thread that runs compiled code has a [`Context`]: its buffer (the
//! next free byte and its end), the exception it is throwing, and a safe
//! point flag. Compiled code gets it as its first, hidden argument and
//! allocates by bumping the buffer; when the buffer runs out, [`refill`]
//! takes a new chunk from the heap with one atomic add. The heap knows every
//! thread's context, for the collector and for saving the heap after the
//! game's setup ran (a snapshot loads back at the same address, so the
//! pointers in it need no fixing).
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Where the heap is reserved when the system allows it (a range user
/// programs leave free on 64-bit systems).
const BASE: u64 = 0x2000_0000_0000;
/// Address space reserved: pages are only backed as they are touched.
pub const RESERVE: u64 = 64 << 30;
/// A thread's buffer is refilled this much at a time (or the object's size,
/// when larger).
pub const CHUNK: u64 = 256 << 10;

/// A thread's state for compiled code. Its layout is the code's: the
/// offsets below are baked into it.
#[repr(C)]
pub struct Context {
    /// The next free byte of this thread's buffer, and its end.
    pub bump: u64,
    pub limit: u64,
    /// The exception being thrown (0: none): set by `athrow`, tested after
    /// each call.
    pub exception: u64,
    /// Nonzero: stop at the next safe point (the collector, a snapshot).
    pub safepoint: u64,
    /// This thread's number in its heap.
    pub thread: u64,
    pub heap: *const Heap,
}

pub const BUMP: i32 = 0;
pub const LIMIT: i32 = 8;
pub const EXCEPTION: i32 = 16;

pub struct Heap {
    base: u64,
    end: u64,
    /// The first byte no buffer has.
    top: AtomicU64,
    /// Whether it got its fixed address.
    fixed: bool,
    /// The contexts of the threads running compiled code (their addresses).
    threads: Mutex<Vec<usize>>,
    next_thread: AtomicU64,
    _memory: Memory,
}

impl Heap {
    /// Reserves `size` bytes of address space: at [`BASE`] when the system
    /// gives it, else wherever it does.
    pub fn reserve(size: u64) -> Heap {
        let (memory, base, fixed) = Memory::reserve(size);
        Heap {
            base,
            end: base + size,
            top: AtomicU64::new(base),
            fixed,
            threads: Mutex::new(Vec::new()),
            next_thread: AtomicU64::new(0),
            _memory: memory,
        }
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    /// Whether the heap is at its fixed address (a saved heap can load back).
    pub fn fixed(&self) -> bool {
        self.fixed
    }

    /// Bytes handed out to buffers so far.
    pub fn used(&self) -> u64 {
        self.top.load(Ordering::Relaxed) - self.base
    }

    /// A chunk of at least `size` bytes: (start, end).
    fn take(&self, size: u64) -> Option<(u64, u64)> {
        let want = size.max(CHUNK).next_multiple_of(8);
        let start = self.top.fetch_add(want, Ordering::Relaxed);
        if start + want > self.end {
            // Leave it past the end: every later take fails too.
            return None;
        }
        Some((start, start + want))
    }

    /// A context for a new thread, registered until [`Heap::release`]. Its
    /// buffer starts empty: the first allocation refills it.
    pub fn context(&self) -> Box<Context> {
        let ctx = Box::new(Context {
            bump: 0,
            limit: 0,
            exception: 0,
            safepoint: 0,
            thread: self.next_thread.fetch_add(1, Ordering::Relaxed),
            heap: self,
        });
        self.threads
            .lock()
            .unwrap()
            .push(&*ctx as *const Context as usize);
        ctx
    }

    /// Forgets a thread's context (it no longer runs compiled code).
    pub fn release(&self, ctx: &Context) {
        let at = ctx as *const Context as usize;
        self.threads.lock().unwrap().retain(|&c| c != at);
    }

    /// How many threads have a context.
    pub fn threads(&self) -> usize {
        self.threads.lock().unwrap().len()
    }
}

/// Allocates `size` bytes (a multiple of 8) for `ctx`'s thread when its
/// buffer is out: a new buffer from the heap, the object at its start.
/// Returns the object's address, 0 when the heap is full. Compiled code
/// calls it on its slow path.
pub extern "C" fn refill(ctx: *mut Context, size: u64) -> u64 {
    // SAFETY: compiled code passes its own thread's context, whose heap
    // outlives the code.
    let ctx = unsafe { &mut *ctx };
    let heap = unsafe { &*ctx.heap };
    match heap.take(size) {
        Some((start, end)) => {
            ctx.bump = start + size;
            ctx.limit = end;
            start
        }
        None => 0,
    }
}

/// Reserved address space, released on drop.
struct Memory {
    #[cfg(unix)]
    at: *mut libc::c_void,
    #[cfg(unix)]
    size: usize,
    #[cfg(not(unix))]
    _bytes: Vec<u8>,
}

// SAFETY: the mapping is plain memory; who writes where is the heap's
// business (buffers never overlap).
unsafe impl Send for Memory {}
unsafe impl Sync for Memory {}

impl Memory {
    #[cfg(unix)]
    fn reserve(size: u64) -> (Memory, u64, bool) {
        let len = size as usize;
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        // SAFETY: an anonymous mapping; at BASE only where nothing is
        // mapped (MAP_FIXED_NOREPLACE on Linux; elsewhere a hint we check).
        #[cfg(target_os = "linux")]
        let fixed_flags = flags | libc::MAP_FIXED_NOREPLACE;
        #[cfg(not(target_os = "linux"))]
        let fixed_flags = flags;
        let at = unsafe { libc::mmap(BASE as *mut _, len, prot, fixed_flags, -1, 0) };
        if at != libc::MAP_FAILED && at as u64 == BASE {
            leave_out_of_core_dumps(at, len);
            return (Memory { at, size: len }, BASE, true);
        }
        if at != libc::MAP_FAILED {
            // SAFETY: the mapping just made.
            unsafe { libc::munmap(at, len) };
        }
        // SAFETY: as above, anywhere.
        let at = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, -1, 0) };
        assert!(at != libc::MAP_FAILED, "no address space for the heap");
        leave_out_of_core_dumps(at, len);
        (Memory { at, size: len }, at as u64, false)
    }

    #[cfg(not(unix))]
    fn reserve(size: u64) -> (Memory, u64, bool) {
        let bytes = vec![0u8; size as usize];
        let base = bytes.as_ptr() as u64;
        (Memory { _bytes: bytes }, base, false)
    }
}

/// A crash's core dump without the heap: the system's crash handler would
/// read through the whole reservation (tens of GB) to write it.
#[cfg(unix)]
fn leave_out_of_core_dumps(at: *mut libc::c_void, len: usize) {
    #[cfg(target_os = "linux")]
    // SAFETY: advice on the mapping just made.
    unsafe {
        libc::madvise(at, len, libc::MADV_DONTDUMP);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (at, len);
}

#[cfg(unix)]
impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: the mapping `reserve` made; nothing uses it any more.
        unsafe { libc::munmap(self.at, self.size) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_fill_their_buffers_from_one_heap_without_overlap() {
        let heap = Heap::reserve(1 << 30);
        let objects: Vec<Vec<(u64, u64)>> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        let mut ctx = heap.context();
                        let mut mine = Vec::new();
                        for k in 0..20_000u64 {
                            let size = 16 + (k % 7) * 8;
                            let next = ctx.bump + size;
                            let at = if ctx.bump != 0 && next <= ctx.limit {
                                let at = ctx.bump;
                                ctx.bump = next;
                                at
                            } else {
                                refill(&mut *ctx, size)
                            };
                            assert_ne!(at, 0);
                            mine.push((at, at + size));
                        }
                        heap.release(&ctx);
                        mine
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        let mut all: Vec<(u64, u64)> = objects.into_iter().flatten().collect();
        all.sort();
        for pair in all.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "objects overlap: {pair:?}");
        }
        assert!(
            all.iter()
                .all(|&(a, b)| a >= heap.base() && b <= heap.base() + (1 << 30))
        );
        assert_eq!(heap.threads(), 0);
    }

    #[test]
    fn a_full_heap_says_so() {
        let heap = Heap::reserve(4 * CHUNK);
        let mut ctx = heap.context();
        let mut got = 0;
        while refill(&mut *ctx, CHUNK) != 0 {
            got += 1;
        }
        assert_eq!(got, 4);
        assert_eq!(refill(&mut *ctx, 8), 0);
    }
}
