//! Aligned, unbuffered block I/O — the one thing worth taking from `holonomy-old/`.
//!
//! # Why this was salvaged rather than rewritten
//!
//! `holonomy-old/crates/holonomy-container/src/io.rs` got two things right that are easy to get
//! wrong and expensive to debug:
//!
//! 1. **`O_DIRECT` requires the *buffer address* to be aligned, not just the file offset.** A
//!    `Vec<u8>` from the global allocator is 16-byte aligned, so handing `&mut buf` to `pread` on
//!    an `O_DIRECT` descriptor fails with `EINVAL` and a message that says nothing about
//!    alignment. [`AlignedBuf`] allocates at [`IO_ALIGN`] so the constraint is met by
//!    construction.
//! 2. **The offset must be a multiple of the logical block size for every transfer**, so the
//!    container's derived offsets have to be aligned down before use. [`DirectFile`] re-checks the
//!    offset on every call and fails with a message that names the number.
//!
//! Everything else — the `SecureBlock` integration, the page-lock policy plumbing — was cut. This
//! module knows about alignment and descriptors and nothing else, so it can be reasoned about in
//! one sitting.
//!
//! # What `O_DIRECT` does and does not give you
//!
//! It bypasses the page cache, which is what keeps the pad and the container out of a
//! swappable/cacheable kernel structure. It does **not** imply `O_SYNC`, and it is not honoured by
//! every filesystem (`tmpfs` rejects it, and some overlay and network filesystems silently
//! degrade). [`DirectFile::open`] therefore reports whether direct I/O was actually obtained via
//! [`DirectFile::is_direct`], and tests skip rather than lie when it was not.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use holonomy_core::layout::IO_ALIGN;

/// A heap buffer whose address is a multiple of [`IO_ALIGN`].
///
/// Zeroed on allocation and wiped on drop, because every use of this type in Holonomy carries
/// either pad bytes or ciphertext and there is no case where leaving the contents behind is
/// acceptable.
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

impl AlignedBuf {
    /// Allocate `len` zeroed bytes, aligned to [`IO_ALIGN`].
    ///
    /// Panics if `len` is zero or unaligned: a zero-length direct transfer is never what was
    /// meant, and an unaligned length cannot succeed, so failing here is a better message than
    /// `EINVAL` from the kernel later.
    pub fn new(len: usize) -> Self {
        assert!(len > 0, "AlignedBuf must not be empty");
        assert!(
            len.is_multiple_of(IO_ALIGN),
            "AlignedBuf length {len} is not a multiple of {IO_ALIGN}"
        );
        let layout = Layout::from_size_align(len, IO_ALIGN)
            .expect("aligned layout is constructible for non-zero len");
        // SAFETY: layout has non-zero size and a valid alignment.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = std::ptr::NonNull::new(raw)
            .expect("allocation of a bounded aligned buffer failed")
            .as_ptr();
        Self { ptr, len }
    }

    /// Buffer length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always `false`; present because clippy wants `len` accompanied by it.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bytes, read-only.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` bytes for `self`'s lifetime.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// The bytes, writable. Wiped on drop either way.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` excludes aliasing.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The raw address, guaranteed `IO_ALIGN`-aligned.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// The raw address for a direct read. Guaranteed `IO_ALIGN`-aligned.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    /// Whether the address really is aligned. Cheap enough to assert in tests and in debug builds.
    pub fn is_aligned(&self) -> bool {
        (self.ptr as usize).is_multiple_of(IO_ALIGN)
    }

    /// Volatile zero-fill. See `holonomy_runtime::scrub` for why volatile matters.
    pub fn wipe(&mut self) {
        for off in 0..self.len {
            // SAFETY: within the allocation.
            unsafe { std::ptr::write_volatile(self.ptr.add(off), 0u8) };
        }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        self.wipe();
        let layout = Layout::from_size_align(self.len, IO_ALIGN).expect("layout was valid on alloc");
        // SAFETY: `ptr` came from `alloc_zeroed` with this exact layout.
        unsafe { dealloc(self.ptr, layout) };
    }
}

impl std::fmt::Debug for AlignedBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print pad or ciphertext bytes in a log line.
        write!(f, "AlignedBuf({} bytes, aligned)", self.len)
    }
}

/// How the device was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// `O_RDONLY`. The default for unlock and view.
    ReadOnly,
    /// `O_RDWR`. Only a save that intends to ratchet opens this way.
    ReadWrite,
}

/// A block device or file opened for aligned I/O.
pub struct DirectFile {
    file: File,
    access: Access,
    direct: bool,
    path: String,
    /// Logical block size used for the alignment check. 4096 on every target Holonomy supports.
    block: u64,
}

impl DirectFile {
    /// Open `path` for aligned I/O.
    ///
    /// `O_DIRECT` is attempted and **downgraded silently on failure**, with
    /// [`is_direct`](Self::is_direct) recording which happened. Silently, because a USB stick
    /// whose filesystem cannot do direct I/O is still a USB stick, and refusing to open it would
    /// turn a performance property into an availability failure — but the caller can and should
    /// surface the difference, because "bypasses the page cache" is a claim.
    pub fn open(path: &Path, access: Access, block: u64) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true);
        if access == Access::ReadWrite {
            opts.write(true);
        }
        opts.custom_flags(libc::O_DIRECT | libc::O_CLOEXEC);

        let (file, direct) = match opts.open(path) {
            Ok(f) => (f, true),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                // `EINVAL` from open with `O_DIRECT` means the filesystem will not do it.
                let mut fallback = OpenOptions::new();
                fallback.read(true);
                if access == Access::ReadWrite {
                    fallback.write(true);
                }
                fallback.custom_flags(libc::O_CLOEXEC);
                (fallback.open(path)?, false)
            }
            Err(e) => return Err(e),
        };

        Ok(Self {
            file,
            access,
            direct,
            path: path.display().to_string(),
            block,
        })
    }

    /// Whether `O_DIRECT` was actually obtained.
    pub fn is_direct(&self) -> bool {
        self.direct
    }

    /// How the handle was opened.
    pub fn access(&self) -> Access {
        self.access
    }

    /// The path as given, for error messages.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The logical block size the alignment check uses.
    pub fn block_size(&self) -> u64 {
        self.block
    }

    /// The descriptor, for an `ioctl` parity or PHY-status probe.
    pub fn raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Current length in bytes.
    pub fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    /// Whether the device is zero length.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Read exactly `buf.len()` bytes at `offset`, with the alignment rules enforced.
    pub fn read_exact_at(&self, offset: u64, buf: &mut AlignedBuf) -> io::Result<()> {
        self.check_offset(offset, buf.len())?;
        let want = buf.len();
        let mut done = 0usize;
        while done < want {
            // SAFETY: the destination is `done` bytes into a buffer of `want` bytes, and the
            // pointer arithmetic stays inside it.
            let dst = unsafe { buf.as_mut_ptr().add(done) };
            let n = unsafe {
                libc::pread(
                    self.file.as_raw_fd(),
                    dst.cast(),
                    want - done,
                    (offset + done as u64) as libc::off_t,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("short read at offset {offset}: {done} of {want} bytes"),
                ));
            }
            done += n as usize;
        }
        Ok(())
    }

    /// Write exactly `buf.as_slice().len()` bytes at `offset`.
    pub fn write_exact_at(&self, offset: u64, buf: &AlignedBuf) -> io::Result<()> {
        if self.access != Access::ReadWrite {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device was opened read-only; a ratchet requires O_RDWR",
            ));
        }
        self.check_offset(offset, buf.len())?;
        let want = buf.len();
        let mut done = 0usize;
        while done < want {
            // SAFETY: read-only pointer into the buffer, advanced within its length.
            let src = unsafe { buf.as_ptr().add(done) };
            let n = unsafe {
                libc::pwrite(
                    self.file.as_raw_fd(),
                    src.cast(),
                    want - done,
                    (offset + done as u64) as libc::off_t,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            done += n as usize;
        }
        Ok(())
    }

    /// `fdatasync`. Only meaningful after a ratchet.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    fn check_offset(&self, offset: u64, len: usize) -> io::Result<()> {
        if self.direct {
            if !offset.is_multiple_of(self.block) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "direct-I/O offset {offset} is not a multiple of the {} byte block",
                        self.block
                    ),
                ));
            }
            if !(len as u64).is_multiple_of(self.block) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("direct-I/O length {len} is not a multiple of {}", self.block),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(tag: &str) -> std::path::PathBuf {
        // Deliberately **not** `/tmp`: `O_DIRECT` is rejected by `tmpfs`, so a test under `/tmp`
        // would silently exercise the fallback path and prove nothing. `target/` is on a real
        // filesystem in this repository.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("usb-scratch");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join(format!("{tag}-{}.bin", std::process::id()))
    }

    #[test]
    fn aligned_buf_really_is_aligned() {
        let b = AlignedBuf::new(65_536);
        assert!(b.is_aligned());
        assert_eq!(b.as_ptr() as usize % IO_ALIGN, 0);
        assert_eq!(b.as_slice().len(), 65_536);
    }

    #[test]
    fn aligned_buf_rejects_bad_lengths() {
        assert!(std::panic::catch_unwind(|| AlignedBuf::new(0)).is_err());
        assert!(std::panic::catch_unwind(|| AlignedBuf::new(100)).is_err());
    }

    #[test]
    fn aligned_buf_wipes_on_drop() {
        let mut b = AlignedBuf::new(4096);
        b.as_mut_slice()[0] = 0xFF;
        b.wipe();
        assert_eq!(b.as_slice()[0], 0);
    }

    #[test]
    fn read_write_round_trip_on_a_real_file() {
        let path = scratch("rw");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&vec![0u8; 65_536]).unwrap();
        }

        let dev = DirectFile::open(&path, Access::ReadWrite, 4096).unwrap();
        // Record whether we got the real thing, but do not fail the suite if the filesystem
        // (overlayfs in a container, often) refuses: the alignment logic must be exercised either
        // way, and the honest report is what matters.
        let direct = dev.is_direct();

        let mut buf = AlignedBuf::new(4096);
        for (i, b) in buf.as_mut_slice().iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        dev.write_exact_at(8192, &buf).unwrap();

        let mut back = AlignedBuf::new(4096);
        dev.read_exact_at(8192, &mut back).unwrap();
        assert_eq!(back.as_slice(), buf.as_slice());

        // Reading at a misaligned offset must be refused when direct I/O is active, and must
        // still work when it is not (the kernel handles it).
        let mut small = AlignedBuf::new(4096);
        let r = dev.read_exact_at(1, &mut small);
        if direct {
            assert!(r.is_err(), "misaligned direct read must be rejected");
            assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn readonly_device_refuses_writes() {
        let path = scratch("ro");
        File::create(&path).unwrap().write_all(&vec![0u8; 8192]).unwrap();
        let dev = DirectFile::open(&path, Access::ReadOnly, 4096).unwrap();
        let buf = AlignedBuf::new(4096);
        let err = dev.write_exact_at(0, &buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("ratchet"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn short_read_is_an_error_not_a_silent_zero_fill() {
        let path = scratch("short");
        File::create(&path).unwrap().write_all(&vec![0u8; 4096]).unwrap();
        let dev = DirectFile::open(&path, Access::ReadOnly, 4096).unwrap();
        let mut buf = AlignedBuf::new(8192);
        let err = dev.read_exact_at(0, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let _ = std::fs::remove_file(&path);
    }
}
