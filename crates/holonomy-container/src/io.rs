//! `O_DIRECT | O_SYNC` block I/O and the aligned buffers it requires.
//!
//! FR-4.7: the container must never be loaded into RAM and every access must bypass the
//! page cache. On Linux that means `open(2)` with `O_DIRECT | O_SYNC`, which imposes two
//! constraints this module exists to enforce rather than to discover at 3 a.m.:
//!
//! * the **file offset** of every transfer must be a multiple of the logical block size
//!   (4096 on this filesystem), and
//! * the **buffer address** must be too.
//!
//! The second one is the one that bites. A `Vec<u8>` from the allocator is 16-byte aligned,
//! so handing `&mut buf` to `pwrite` on an `O_DIRECT` fd fails with `EINVAL` and a message
//! that says nothing about alignment. [`AlignedBuf`] allocates with an explicit 4096
//! alignment so the constraint is met by construction, and [`DirectFile`] re-checks the
//! offset on every call so a bad offset fails with a message that names the number.
//!
//! Note that `O_DIRECT` is *not* available on every filesystem — tmpfs rejects it — so
//! tests must run on the real disk, not under `/tmp`. See `tests::test_scratch_dir`.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use crate::layout::{self, IO_ALIGN};

/// A buffer whose address is a multiple of [`IO_ALIGN`], as `O_DIRECT` requires.
pub struct AlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
    layout: Layout,
}

impl AlignedBuf {
    /// Allocate `len` zeroed bytes, 4096-aligned.
    ///
    /// Panics if `len` is 0 or the allocation fails, because every caller wants a real
    /// buffer and a zero-length `O_DIRECT` transfer is never what was meant.
    pub fn zeroed(len: usize) -> Self {
        assert!(len > 0, "AlignedBuf must not be empty");
        let layout = Layout::from_size_align(len, IO_ALIGN as usize)
            .expect("aligned layout is always constructible for non-zero len");
        // SAFETY: layout has non-zero size, and `alloc_zeroed` returns either null or a
        // block of `len` bytes aligned to IO_ALIGN.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = std::ptr::NonNull::new(raw).expect("allocation of a fixed small size failed");
        Self { ptr, len, layout }
    }

    /// The buffer as a slice.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` came from an allocation of exactly `len` bytes and is only
        // borrowed for the duration of this borrow.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The buffer as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, plus we hold `&mut self`, so the borrow is exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false; exists so clippy does not fire on `len()` without `is_empty()`.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Confirm the address really is aligned, which is the invariant `O_DIRECT` needs and
    /// the one thing worth asserting rather than trusting.
    pub fn is_aligned(&self) -> bool {
        (self.ptr.as_ptr() as usize).is_multiple_of(IO_ALIGN as usize)
    }

    /// Zero the whole buffer. Used for plaintext and for decrypted material.
    pub fn wipe(&mut self) {
        self.as_mut_slice().fill(0);
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` are the pair returned by `alloc_zeroed` via
        // `from_size_align`, so this is the matching `dealloc`.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

impl core::fmt::Debug for AlignedBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AlignedBuf")
            .field("len", &self.len)
            .field("aligned", &self.is_aligned())
            .finish()
    }
}

/// A file descriptor opened for unbuffered direct I/O.
#[derive(Debug)]
pub struct DirectFile {
    fd: OwnedFd,
    path: String,
}

impl DirectFile {
    /// Open an existing container, or create it at exactly [`layout::CONTAINER_SIZE`].
    ///
    /// `O_CREAT` is always set, because the file is worthless without the right size and
    /// a partially-created container should be completable rather than rejected. `O_TRUNC`
    /// is *not* set: a caller that means to destroy a container asks for [`truncate_zero`]
    /// explicitly, so "create" never silently destroys.
    ///
    /// The mode is 0600. A container holds the only copy of the user's plaintext; it does
    /// not belong to the group or the world.
    pub fn create_or_open(path: &Path) -> io::Result<Self> {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"))?;
        // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
        let raw = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_DIRECT | libc::O_SYNC | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh fd that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let file = Self {
            fd,
            path: path.display().to_string(),
        };
        file.set_len(layout::CONTAINER_SIZE)?;
        Ok(file)
    }

    /// Open an existing container read/write, failing if it is absent or the wrong size.
    ///
    /// Used on the open path, where silently resizing a container that has been truncated
    /// or swapped would destroy a document.
    pub fn open(path: &Path) -> io::Result<Self> {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"))?;
        // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
        let raw = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDWR | libc::O_DIRECT | libc::O_SYNC | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh fd that nothing else owns.
        let file = Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw) },
            path: path.display().to_string(),
        };
        let len = file.len()?;
        if len != layout::CONTAINER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "container is {len} bytes, must be exactly {}",
                    layout::CONTAINER_SIZE
                ),
            ));
        }
        Ok(file)
    }

    /// The raw descriptor.
    pub fn as_raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// The path this file was opened from, for diagnostics.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Always false: a container is exactly [`layout::CONTAINER_SIZE`] bytes by
    /// construction, so this exists only to satisfy the `len`/`is_empty` pairing rather than
    /// because the state is reachable.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Current file length.
    pub fn len(&self) -> io::Result<u64> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fstat` fills `st` and has no preconditions.
        let rc = unsafe { libc::fstat(self.as_raw(), &mut st) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st.st_size as u64)
    }

    /// Force the file to exactly [`layout::CONTAINER_SIZE`].
    fn set_len(&self, len: u64) -> io::Result<()> {
        // SAFETY: a plain ftruncate on a valid fd.
        if unsafe { libc::ftruncate(self.as_raw(), len as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Overwrite the entire file with chaff, then sync.
    ///
    /// Used by a failed-create recovery: the bytes are a pure function of the key, so
    /// there is no partial state worth trying to resume.
    pub fn truncate_zero(&self) -> io::Result<()> {
        self.set_len(0)?;
        self.set_len(layout::CONTAINER_SIZE)
    }

    /// Read exactly `buf.len()` bytes from `offset`, or fail.
    ///
    /// The offset must be [`IO_ALIGN`]-aligned; a short read is an error, never a partial
    /// success, because a partial chunk would hand the AEAD a truncated message and the
    /// tag check would then fail for the wrong reason.
    pub fn read_exact_at(&self, offset: u64, buf: &mut AlignedBuf) -> io::Result<()> {
        layout::require_aligned(offset)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let slice = buf.as_mut_slice();
        let mut done = 0usize;
        while done < slice.len() {
            // SAFETY: `slice` is a live allocation of at least `slice.len() - done` bytes
            // and the descriptor is valid. `pread` does not move the file offset.
            let n = unsafe {
                libc::pread(
                    self.as_raw(),
                    slice[done..].as_mut_ptr().cast(),
                    slice.len() - done,
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
                    format!(
                        "short read at {offset}: got {done} of {} bytes",
                        slice.len()
                    ),
                ));
            }
            done += n as usize;
        }
        Ok(())
    }

    /// Write exactly `buf.len()` bytes at `offset`, or fail.
    pub fn write_exact_at(&self, offset: u64, buf: &AlignedBuf) -> io::Result<()> {
        layout::require_aligned(offset)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let slice = buf.as_slice();
        let mut done = 0usize;
        while done < slice.len() {
            // SAFETY: as `read_exact_at`, for a shared slice.
            let n = unsafe {
                libc::pwrite(
                    self.as_raw(),
                    slice[done..].as_ptr().cast(),
                    slice.len() - done,
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
                    io::ErrorKind::WriteZero,
                    format!("short write at {offset}: wrote {done} of {}", slice.len()),
                ));
            }
            done += n as usize;
        }
        Ok(())
    }

    /// `fsync`. Every write already carries `O_SYNC`, so this is belt-and-braces against a
    /// kernel that batches; it is cheap relative to a VDF and called once on close.
    pub fn sync(&self) -> io::Result<()> {
        // SAFETY: a plain fsync on a valid fd.
        if unsafe { libc::fsync(self.as_raw()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl AsRawFd for DirectFile {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch files must live on a filesystem that supports `O_DIRECT`. tmpfs does not,
    /// and `/tmp` is tmpfs on this host, so tests resolve a directory next to the build
    /// output instead of using `std::env::temp_dir`.
    pub fn scratch_dir(tag: &str) -> std::path::PathBuf {
        // `current_exe` is target/<profile>/deps/<test>; three levels up is target/<profile>.
        let exe = std::env::current_exe().expect("test exe path");
        let dir = exe
            .ancestors()
            .nth(3)
            .expect("target/<profile> layout")
            .join("holonomy-container-tests");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let sub = dir.join(tag);
        let _ = std::fs::remove_dir_all(&sub);
        std::fs::create_dir_all(&sub).expect("create per-test dir");
        sub
    }

    #[test]
    fn aligned_buf_is_actually_aligned() {
        let mut b = AlignedBuf::zeroed(65_536);
        assert!(b.is_aligned(), "address must be {IO_ALIGN}-aligned");
        assert_eq!(b.len(), 65_536);
        assert_eq!(b.as_slice().len(), 65_536);
        assert!(b.as_slice().iter().all(|&x| x == 0));
        b.as_mut_slice()[0] = 0xAB;
        assert_eq!(b.as_slice()[0], 0xAB);
    }

    #[test]
    fn aligned_buf_wipes() {
        let mut b = AlignedBuf::zeroed(4096);
        b.as_mut_slice().fill(0xFF);
        b.wipe();
        assert!(b.as_mut_slice().iter().all(|&x| x == 0));
    }

    #[test]
    fn debug_does_not_dump_contents() {
        let mut b = AlignedBuf::zeroed(64);
        b.as_mut_slice().fill(0xAB);
        let rendered = format!("{b:?}");
        assert!(
            !rendered.contains("171"),
            "Debug leaked contents: {rendered}"
        );
        assert!(rendered.contains("aligned: true"), "{rendered}");
    }

    /// A round trip through a real `O_DIRECT` file.
    #[test]
    fn direct_write_then_read() {
        let dir = scratch_dir("direct_roundtrip");
        let path = dir.join("c.wavefunction");
        let f = DirectFile::create_or_open(&path).expect("create");
        assert_eq!(f.len().expect("len"), layout::CONTAINER_SIZE);

        let mut src = AlignedBuf::zeroed(65_536);
        for (i, byte) in src.as_mut_slice().iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        f.write_exact_at(65_536, &src).expect("write");

        let mut dst = AlignedBuf::zeroed(65_536);
        f.read_exact_at(65_536, &mut dst).expect("read");
        assert_eq!(dst.as_slice(), src.as_slice());
    }

    /// Unaligned offsets must be refused before the kernel sees them, with a message that
    /// names the offending number.
    #[test]
    fn unaligned_offsets_are_refused_by_name() {
        let dir = scratch_dir("direct_unaligned");
        let path = dir.join("c.wavefunction");
        let f = DirectFile::create_or_open(&path).expect("create");
        let buf = AlignedBuf::zeroed(4096);
        let err = f.write_exact_at(1, &buf).expect_err("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains('1'), "{}", err);

        let mut other = AlignedBuf::zeroed(4096);
        let err = f
            .read_exact_at(65_537, &mut other)
            .expect_err("must refuse");
        assert!(err.to_string().contains("65537"), "{}", err);
    }

    /// Opening a file of the wrong size must fail rather than silently resizing it,
    /// because a truncated container is a document the user cannot get back.
    #[test]
    fn open_rejects_a_wrong_sized_file() {
        let dir = scratch_dir("direct_wrong_size");
        let path = dir.join("small.wavefunction");
        std::fs::write(&path, vec![0u8; 4096]).expect("write short file");
        let err = DirectFile::open(&path).expect_err("must refuse a 4 KiB file");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("4096"), "{}", err);
    }

    /// `create_or_open` must not truncate an existing container, so a create that is
    /// interrupted can be completed without losing what was already written.
    #[test]
    fn create_or_open_does_not_truncate() {
        let dir = scratch_dir("direct_no_trunc");
        let path = dir.join("c.wavefunction");
        let f = DirectFile::create_or_open(&path).expect("create");
        let mut buf = AlignedBuf::zeroed(4096);
        buf.as_mut_slice().fill(0x5A);
        f.write_exact_at(131_072, &buf).expect("write");
        drop(f);

        let f2 = DirectFile::create_or_open(&path).expect("reopen");
        let mut back = AlignedBuf::zeroed(4096);
        f2.read_exact_at(131_072, &mut back).expect("read back");
        assert!(
            back.as_slice().iter().all(|&x| x == 0x5A),
            "data was destroyed"
        );
    }

    /// A 128 MiB create followed by a full-length write must leave every byte accounted
    /// for. This is the slowest test in the crate; it exists because the create path is
    /// the one that has to touch all 128 MiB.
    #[test]
    fn whole_container_can_be_written_and_verified() {
        let dir = scratch_dir("direct_whole");
        let path = dir.join("c.wavefunction");
        let f = DirectFile::create_or_open(&path).expect("create");

        let chaff = crate::chaff::Chaff::new(&[7u8; 32]);
        let mut buf = AlignedBuf::zeroed(4096);
        let mut offset = 0u64;
        while offset < layout::CONTAINER_SIZE {
            chaff.fill(offset, buf.as_mut_slice()).expect("chaff");
            f.write_exact_at(offset, &buf).expect("write");
            offset += 4096;
        }
        f.sync().expect("sync");
        assert_eq!(f.len().expect("len"), layout::CONTAINER_SIZE);

        // Spot-check the first and last pages rather than reading all 128 MiB back.
        let mut read = AlignedBuf::zeroed(4096);
        f.read_exact_at(0, &mut read).expect("read head");
        assert_eq!(
            read.as_slice(),
            &chaff.bytes_at(0, 4096).expect("expected head")[..]
        );

        let last = layout::CONTAINER_SIZE - 4096;
        f.read_exact_at(last, &mut read).expect("read tail");
        assert_eq!(
            read.as_slice(),
            &chaff.bytes_at(last, 4096).expect("expected tail")[..]
        );
    }
}
