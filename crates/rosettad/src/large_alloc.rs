//! The daemon's global allocator on macOS: the system allocator, except that
//! a block larger than [`DIRECT_ABOVE`] is a private mapping of its own,
//! unmapped when it is freed.
//!
//! The system allocator keeps the most recently freed large blocks — 64 of
//! them, of any size — for reuse, still dirty, so they stay in the process's
//! physical footprint after `free`. A cold build frees gigabytes of them:
//! every capture step frames a batch of up to 32 MiB into one buffer and
//! compresses it into a second, and both are freed once the ledger has them.
//! Unmapped, freed memory leaves the footprint at once: on the frozen R1
//! corpus about 220 MB of the cold build's peak, at unchanged canonical
//! readiness. Blocks this large are few, so a system call and fresh zero
//! pages each cost little beside the copy that fills them; smaller blocks
//! stay with the system allocator, whose regions are reused in place.
//! Other targets keep their system allocator unchanged; glibc's mmap
//! threshold rises with the blocks it frees (to 32 MiB), so whether Linux
//! retains these buffers is not measured.
//! (docs/ARCHITECTURE.md, "Cold-build measurement".)

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Blocks larger than this are mapped directly: the system allocator's
/// small-block limit, above which its large-block cache begins.
pub(crate) const DIRECT_ABOVE: usize = 2 * 1024 * 1024;

pub(crate) struct DirectLarge;

/// The page size, read once.
fn page_size() -> usize {
    static PAGE: AtomicUsize = AtomicUsize::new(0);
    match PAGE.load(Ordering::Relaxed) {
        0 => {
            // SAFETY: sysconf reads a constant system value; no pointers.
            let size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
                .ok()
                .filter(|size| size.is_power_of_two())
                .unwrap_or(16 * 1024);
            PAGE.store(size, Ordering::Relaxed);
            size
        }
        size => size,
    }
}

/// Whether a block of `layout` is mapped directly. A mapping is page
/// aligned, so stricter alignments stay with the system allocator.
fn direct(layout: Layout) -> bool {
    layout.size() > DIRECT_ABOVE && layout.align() <= page_size()
}

/// `size` rounded up to whole pages. A valid layout's size is at most
/// `isize::MAX`, so this cannot overflow.
fn mapped_len(size: usize) -> usize {
    let page = page_size();
    (size + page - 1) & !(page - 1)
}

/// A fresh private anonymous mapping of `len` bytes (zeroed), or null.
/// `hint` asks for an address; the kernel may place it elsewhere.
///
/// # Safety
/// `len` is a nonzero multiple of the page size.
unsafe fn map(len: usize, hint: *mut u8) -> *mut u8 {
    // SAFETY: an anonymous private mapping touches no existing memory, and
    // without MAP_FIXED a hint never replaces a mapping.
    let ptr = unsafe {
        libc::mmap(
            hint.cast(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        std::ptr::null_mut()
    } else {
        ptr.cast()
    }
}

/// Unmap `len` bytes at `ptr`.
///
/// # Safety
/// `ptr..ptr + len` is whole pages of a mapping this allocator made, which
/// nothing uses any more.
unsafe fn unmap(ptr: *mut u8, len: usize) {
    // SAFETY: the caller's contract.
    unsafe { libc::munmap(ptr.cast(), len) };
}

// SAFETY: every block is either the system allocator's (the same calls
// `System` would get) or a whole private mapping of `mapped_len(size)` bytes
// at a page-aligned address, which satisfies any alignment `direct` admits.
// `dealloc` and `realloc` tell the two apart from the layout the caller
// passes, which is the one the block was allocated (or last reallocated)
// with.
unsafe impl GlobalAlloc for DirectLarge {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if direct(layout) {
            // SAFETY: a nonzero whole-page length; no hint.
            unsafe { map(mapped_len(layout.size()), std::ptr::null_mut()) }
        } else {
            // SAFETY: forwarded unchanged.
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if direct(layout) {
            // SAFETY: as `alloc`; an anonymous mapping is zero-filled.
            unsafe { map(mapped_len(layout.size()), std::ptr::null_mut()) }
        } else {
            // SAFETY: forwarded unchanged.
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if direct(layout) {
            // SAFETY: `ptr` is a mapping of exactly this length (see above),
            // and the caller frees it.
            unsafe { unmap(ptr, mapped_len(layout.size())) }
        } else {
            // SAFETY: forwarded unchanged.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller guarantees `new_size`, rounded up to the
        // alignment, does not overflow `isize`.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        match (direct(layout), direct(new_layout)) {
            // SAFETY: forwarded unchanged.
            (false, false) => unsafe { System.realloc(ptr, layout, new_size) },
            (true, true) => {
                let old_len = mapped_len(layout.size());
                let new_len = mapped_len(new_size);
                if new_len <= old_len {
                    if new_len < old_len {
                        // SAFETY: the tail pages are this mapping's own, and
                        // past what the block now holds.
                        unsafe { unmap(ptr.add(new_len), old_len - new_len) };
                    }
                    return ptr;
                }
                // Grow in place when the pages right after it are free.
                // SAFETY: `ptr + old_len` is the end of this mapping.
                let tail = unsafe { ptr.add(old_len) };
                let extra = new_len - old_len;
                // SAFETY: a nonzero whole-page length; the hint replaces
                // nothing.
                let placed = unsafe { map(extra, tail) };
                if placed == tail {
                    return ptr;
                }
                if !placed.is_null() {
                    // SAFETY: mapped just above, of that length, unused.
                    unsafe { unmap(placed, extra) };
                }
                // SAFETY: a nonzero whole-page length; no hint.
                let grown = unsafe { map(new_len, std::ptr::null_mut()) };
                if !grown.is_null() {
                    // SAFETY: both blocks hold at least the old size and do
                    // not overlap; the old mapping is the caller's to free.
                    unsafe {
                        std::ptr::copy_nonoverlapping(ptr, grown, layout.size());
                        unmap(ptr, old_len);
                    }
                }
                grown
            }
            _ => {
                // Crossing the threshold: a block of the other kind.
                // SAFETY: `new_layout` is valid (see above).
                let moved = unsafe { self.alloc(new_layout) };
                if !moved.is_null() {
                    // SAFETY: both blocks hold at least the smaller size and
                    // are distinct; the old block is freed with its layout.
                    unsafe {
                        std::ptr::copy_nonoverlapping(ptr, moved, layout.size().min(new_size));
                        self.dealloc(ptr, layout);
                    }
                }
                moved
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    /// Write a position pattern over the first `len` bytes at `ptr`.
    fn fill(ptr: *mut u8, len: usize) {
        // SAFETY: the block holds at least `len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
    }

    /// The first `len` bytes at `ptr` still hold `fill`'s pattern.
    fn check(ptr: *const u8, len: usize) {
        // SAFETY: the block holds at least `len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        if let Some(index) = (0..len).find(|index| bytes[*index] != (index % 251) as u8) {
            panic!("byte {index} of {len} lost its contents");
        }
    }

    #[test]
    fn only_large_blocks_of_page_alignment_or_less_are_mapped() {
        assert!(!direct(layout(DIRECT_ABOVE, 8)));
        assert!(direct(layout(DIRECT_ABOVE + 1, 8)));
        assert!(direct(layout(DIRECT_ABOVE + 1, page_size())));
        assert!(!direct(layout(DIRECT_ABOVE + 1, page_size() * 2)));
        assert_eq!(mapped_len(DIRECT_ABOVE + 1), DIRECT_ABOVE + page_size());
    }

    #[test]
    fn a_mapped_block_is_zeroed_page_aligned_and_writable() {
        let big = layout(3 * DIRECT_ABOVE + 17, 64);
        // SAFETY: allocated, used within its size and freed with its layout.
        unsafe {
            let ptr = DirectLarge.alloc_zeroed(big);
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % page_size(), 0);
            assert!(
                std::slice::from_raw_parts(ptr, big.size())
                    .iter()
                    .all(|byte| *byte == 0)
            );
            fill(ptr, big.size());
            check(ptr, big.size());
            DirectLarge.dealloc(ptr, big);
        }
    }

    #[test]
    fn a_strictly_aligned_large_block_stays_with_the_system() {
        let aligned = layout(DIRECT_ABOVE + 1, page_size() * 4);
        // SAFETY: allocated, used within its size and freed with its layout.
        unsafe {
            let ptr = DirectLarge.alloc(aligned);
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % aligned.align(), 0);
            fill(ptr, aligned.size());
            check(ptr, aligned.size());
            DirectLarge.dealloc(ptr, aligned);
        }
    }

    #[test]
    fn realloc_keeps_the_contents_across_every_transition() {
        let small = 1024 * 1024;
        let sizes = [
            DIRECT_ABOVE + 1,     // system -> mapped
            4 * DIRECT_ABOVE + 5, // mapped grows
            4 * DIRECT_ABOVE + 9, // mapped, the same pages
            DIRECT_ABOVE + 3,     // mapped shrinks
            small / 2,            // mapped -> system
            8 * DIRECT_ABOVE,     // system -> mapped again
        ];
        // SAFETY: every block is used within its size, reallocated and freed
        // with the layout it has.
        unsafe {
            let mut current = layout(small, 8);
            let mut ptr = DirectLarge.alloc(current);
            assert!(!ptr.is_null());
            fill(ptr, current.size());
            for size in sizes {
                ptr = DirectLarge.realloc(ptr, current, size);
                assert!(!ptr.is_null(), "realloc to {size}");
                check(ptr, current.size().min(size));
                current = layout(size, 8);
                fill(ptr, size);
            }
            DirectLarge.dealloc(ptr, current);
        }
    }

    #[test]
    fn a_vector_grows_and_shrinks_through_the_threshold() {
        let mut bytes: Vec<u8> = Vec::new();
        for index in 0..(3 * DIRECT_ABOVE) {
            bytes.push((index % 253) as u8);
        }
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == (index % 253) as u8)
        );
        bytes.truncate(DIRECT_ABOVE / 3);
        bytes.shrink_to_fit();
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == (index % 253) as u8)
        );
    }
}
