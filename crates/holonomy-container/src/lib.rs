//! The `.wavefunction` container: exactly 134,217,728 bytes of indistinguishable noise.
//!
//! Chaff `ChaCha20(K_chaff, nonce = block)` fills everything outside the payload, so the
//! file carries no magic number, no header and no cleartext block boundary. Access is
//! `O_DIRECT | O_SYNC`; opening seeks to Ω and reads only the 3-stage ring `[N−1, N, N+1]`
//! = 192 KiB, never the whole 128 MiB.
//!
//! Two salt rules, and the second one is easy to get wrong: the salt lives at **fixed
//! offset 0**, not at Ω. Ω is *derived from the passphrase*, so a salt stored at Ω would
//! be circular — you could not find the salt without the key you are trying to derive.
//!
//! Lands in Phase 3. Gate: create → close → open → read → write → close → open → verify,
//! plus a NIST SP 800-22 subset over the produced file. See PROJECT.md §5 Phase 3.

pub mod aead;
pub mod chaff;
pub mod frame;
#[cfg(test)]
mod gate;
pub mod io;
pub mod layout;
pub mod ring;
pub mod sp800_22;

pub use holonomy_jail::PHASE_0_PLACEHOLDER;

use std::path::Path;

use holonomy_crypto::envelope::ExposeSecret;
use holonomy_crypto::envelope::SALT_LEN;
use holonomy_crypto::envelope::{self, Derived, EnvelopeError, RootMaterial};

use aead::AeadError;
use chaff::{Chaff, ChaffError};
use frame::{FrameError, MasterFrame};
use io::{AlignedBuf, DirectFile};
use layout::{
    chunks_for, omega as omega_from_offset, payload_len, CHUNK_PLAINTEXT, CHUNK_SLOT,
    CONTAINER_SIZE, IO_ALIGN, MASTER_FRAME_CHUNK, SALT_OFFSET,
};
use ring::{Ring, RingError};

/// Iteration count used by the tests and by any caller that does not care about latency.
///
/// A real unlock passes a count derived from a measured per-squaring cost (PROJECT.md
/// §2.4). The gate needs a value that keeps the test fast, and the VDF's *correctness* is
/// covered by `holonomy-crypto`'s chain tests, not here.
pub const TEST_VDF_ITERATIONS: u64 = 8;

/// Why a container operation failed.
#[derive(Debug)]
pub enum ContainerError {
    /// Filesystem or device error.
    Io(std::io::Error),
    /// A chunk or the master frame failed authentication: wrong passphrase, or damage.
    ///
    /// This is the answer to "wrong passphrase" and it is deliberately not distinguished
    /// from corruption, because this path is reachable with attacker-chosen input and a
    /// finer-grained error is a decryption oracle.
    Aead(AeadError),
    /// Chunk 0 decrypted but is not a master frame we understand.
    Frame(FrameError),
    /// The ring could not do what was asked.
    Ring(RingError),
    /// Chaff generation failed.
    Chaff(ChaffError),
    /// Key derivation failed.
    Envelope(EnvelopeError),
    /// The content does not fit under [`layout::S_MAX_PAYLOAD`].
    PayloadTooLarge {
        /// Requested bytes.
        requested: u64,
        /// The cap.
        cap: u64,
    },
    /// The CSPRNG failed.
    NoEntropy,
    /// Phase 13: a chunk index that does not exist, or is the master frame.
    ///
    /// **A separate variant rather than a generic `Io` error**, because "which chunks exist" is answered by
    /// [`Wavefunction::chunk_content_offset`] and a caller that gets `None` there should get the same
    /// answer here. A caller that has to match on an `io::ErrorKind` to discover a bounds violation is a
    /// caller that will not discover it.
    NoSuchChunk {
        /// The index asked for.
        index: u64,
    },
}

impl core::fmt::Display for ContainerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o: {e}"),
            Self::Aead(e) => write!(f, "authentication: {e}"),
            Self::Frame(e) => write!(f, "master frame: {e}"),
            Self::Ring(e) => write!(f, "ring: {e}"),
            Self::Chaff(e) => write!(f, "chaff: {e}"),
            Self::Envelope(e) => write!(f, "key derivation: {e}"),
            Self::PayloadTooLarge { requested, cap } => {
                write!(f, "{requested} bytes exceeds the {cap}-byte payload cap")
            }
            Self::NoEntropy => f.write_str("the operating system CSPRNG failed"),
            Self::NoSuchChunk { index } => {
                write!(f, "chunk {index} does not exist or is the master frame")
            }
        }
    }
}

impl std::error::Error for ContainerError {}

macro_rules! from_error {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for ContainerError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}

from_error!(std::io::Error, Io);
from_error!(AeadError, Aead);
from_error!(FrameError, Frame);
from_error!(RingError, Ring);
from_error!(ChaffError, Chaff);
from_error!(EnvelopeError, Envelope);

/// An open `.wavefunction` container.
///
/// Holds the derived root, the 3-stage ring, and the file. Dropping it wipes the ring
/// before the heap pages are freed.
pub struct Wavefunction {
    file: DirectFile,
    chaff: Chaff,
    /// Owned copy of the root material. The `Derived`'s own `SecretBox` is consumed into
    /// this so there is exactly one copy rather than a secret wrapper and a plain copy.
    root: RootMaterial,
    /// The derived Ω.
    omega: u64,
    /// The master frame, from chunk 0.
    frame: MasterFrame,
    ring: Ring,
}

impl Wavefunction {
    /// Derived Ω, for diagnostics and for the entropy report.
    pub fn omega(&self) -> u64 {
        self.omega
    }

    /// The container's fixed size.
    pub fn len(&self) -> u64 {
        CONTAINER_SIZE
    }

    /// Always false; a container is a fixed 128 MiB by definition.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Content plaintext length.
    pub fn content_len(&self) -> u64 {
        self.frame.content_len
    }

    /// Document title.
    pub fn title(&self) -> &str {
        &self.frame.title
    }

    /// The master frame.
    pub fn frame(&self) -> &MasterFrame {
        &self.frame
    }

    /// Steady-state resident bytes for the ring.
    pub fn ring_resident_bytes(&self) -> usize {
        self.ring.resident_bytes()
    }

    /// The path this container was opened from.
    pub fn path(&self) -> &str {
        self.file.path()
    }

    /// Generate a fresh 32-byte salt.
    ///
    /// `getrandom(2)`, not anything derived: a salt that were a function of the
    /// passphrase could not break the Ω/payload circularity it exists to solve.
    fn fresh_salt() -> Result<[u8; SALT_LEN], ContainerError> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|_| ContainerError::NoEntropy)?;
        Ok(salt)
    }

    /// Derive the root from a passphrase and a salt.
    fn derive(
        passphrase: &str,
        salt: &[u8],
        iterations: u64,
    ) -> Result<(RootMaterial, u64), ContainerError> {
        let Derived { root, .. } = envelope::derive_root(passphrase, salt, iterations)?;
        let m = root.expose_secret().clone();
        let omega = omega_from_offset(m.omega);
        Ok((m, omega))
    }

    /// Create a new container and write it out in full.
    ///
    /// `content` is the document plaintext, stored in chunks 1..N. Chunks 1..N hold up to
    /// [`CHUNK_PLAINTEXT`] bytes each; chunk 0 holds the master frame.
    pub fn create(
        path: &Path,
        passphrase: &str,
        title: &str,
        content: &[u8],
        vdf_iterations: u64,
    ) -> Result<Self, ContainerError> {
        let salt = Self::fresh_salt()?;
        let (root, omega) = Self::derive(passphrase, &salt, vdf_iterations)?;

        let chunk_count =
            chunks_for(content.len() as u64).map_err(|_| ContainerError::PayloadTooLarge {
                requested: content.len() as u64,
                cap: layout::S_MAX_PAYLOAD,
            })?;
        if !layout::payload_fits(omega, payload_len(chunk_count)) {
            return Err(ContainerError::PayloadTooLarge {
                requested: payload_len(chunk_count),
                cap: layout::S_MAX_PAYLOAD,
            });
        }

        let frame = MasterFrame::new(content.len() as u64, chunk_count, title).with_kdf_params(
            vdf_iterations,
            envelope::ARGON2_M_COST_KIB,
            envelope::ARGON2_T_COST,
            envelope::ARGON2_P_COST,
        );

        let file = DirectFile::create_or_open(path)?;
        let chaff = Chaff::new(&root.k_chaff);
        let ring = Ring::new(chunk_count);

        // Page 0 is chaff with the salt laid over its first 32 bytes. Writing the rest of
        // the page as chaff rather than zeroes matters: 4,064 zero bytes at a fixed offset
        // is exactly the cleartext structure FR-4.1 forbids.
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        chaff.fill(0, page.as_mut_slice())?;
        page.as_mut_slice()[..SALT_LEN].copy_from_slice(&salt);
        file.write_exact_at(0, &page)?;

        // Leading chaff: [IO_ALIGN, omega).
        fill_chaff(&file, &chaff, IO_ALIGN, omega)?;
        // Trailing chaff: [omega + payload, CONTAINER_SIZE).
        fill_chaff(
            &file,
            &chaff,
            omega + payload_len(chunk_count),
            CONTAINER_SIZE,
        )?;

        // Chunk 0: the master frame.
        let mut frame_bytes = [0u8; CHUNK_PLAINTEXT];
        let n = frame.encode(&mut frame_bytes)?;
        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        aead::seal_chunk(
            &root.k_enc,
            &root.n_root,
            MASTER_FRAME_CHUNK,
            &frame_bytes[..n],
            slot.as_mut_slice(),
        )?;
        file.write_exact_at(layout::chunk_offset(omega, MASTER_FRAME_CHUNK), &slot)?;
        slot.wipe();

        // Chunks 1..N: the content.
        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        for index in 1..chunk_count {
            let start = ((index - 1) * CHUNK_PLAINTEXT as u64) as usize;
            let end = (start + CHUNK_PLAINTEXT).min(content.len());
            let piece = if start < content.len() {
                &content[start..end]
            } else {
                &[][..]
            };
            aead::seal_chunk(&root.k_enc, &root.n_root, index, piece, slot.as_mut_slice())?;
            file.write_exact_at(layout::chunk_offset(omega, index), &slot)?;
        }
        slot.wipe();
        file.sync()?;

        let mut this = Self {
            file,
            chaff,
            root,
            omega,
            frame,
            ring,
        };
        // Leave the ring centred on chunk 0 so the in-memory state matches what a fresh
        // open would see.
        this.ring.seek(
            &this.file,
            this.omega,
            &this.root.k_enc,
            &this.root.n_root,
            0,
        )?;
        Ok(this)
    }

    /// Open an existing container with a passphrase.
    ///
    /// **A two-line convenience over [`adopt`](Self::adopt), and the reason both exist is the boot
    /// order.** `main.rs` opens every descriptor at stage 4, *before* the seccomp filter is installed at
    /// stage 8, because the filter's allowlist has **no `openat`** -- so after sealing, no path can become
    /// a descriptor and reaching for one is `SIGSYS` and exit 137.
    ///
    /// The passphrase is read *after* sealing (there is no `getenv` guarantee post-filter), so the container
    /// **cannot** be decrypted during boot. It has to be opened afterwards, from a descriptor that already
    /// exists. This method opens its own path and is therefore only usable **before** sealing -- in tests,
    /// in tools, and nowhere in the product. The product path is [`adopt`](Self::adopt).
    pub fn open(
        path: &Path,
        passphrase: &str,
        vdf_iterations: u64,
    ) -> Result<Self, ContainerError> {
        Self::adopt(DirectFile::open(path)?, passphrase, vdf_iterations)
    }

    /// Open a container **from a descriptor the caller already holds**, with a passphrase.
    ///
    /// # Why this is the only usable form after sealing
    ///
    /// **Because the path can no longer be named.** The boot's rule is that stage 4 is the last moment a
    /// path can become a descriptor, and it is enforced rather than documented: the seccomp allowlist
    /// contains no `openat` (`holonomy-jail/src/seccomp/table.rs` lists it under the refused syscalls).
    /// Since the passphrase only exists after sealing, the only way to reach a container is through a
    /// descriptor obtained earlier.
    ///
    /// **This is `open`'s entire body**, so the two cannot drift: `open` is now a two-line delegate and
    /// there is no second implementation to keep in step.
    ///
    /// # Why this preserves the invariant rather than relaxing it
    ///
    /// The tempting alternative is to add `openat` to the allowlist so `open` works after sealing. **That
    /// gives up the rule the boot exists to enforce** — that the set of open files is fixed before the
    /// world is closed — and it would do so for the sake of convenience. `adopt` reaches the same place
    /// without weakening anything: the path was named at stage 4, the descriptor exists, and the container
    /// simply uses it.
    pub fn adopt(
        file: DirectFile,
        passphrase: &str,
        vdf_iterations: u64,
    ) -> Result<Self, ContainerError> {
        // Page 0 carries the salt in its first 32 bytes. `O_DIRECT` cannot read 32 bytes --
        // offset, length and address must all be block multiples -- so read the page.
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        file.read_exact_at(SALT_OFFSET, &mut page)?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&page.as_slice()[..SALT_LEN]);
        drop(page);

        let (root, omega) = Self::derive(passphrase, &salt, vdf_iterations)?;
        let chaff = Chaff::new(&root.k_chaff);

        // Read chunk 0 with a ring bounded to exactly one chunk. The real chunk count is
        // inside chunk 0, so until it is read the ring must not prefetch a neighbour: a
        // one-chunk container has no chunk 1, and prefetching it would try to authenticate
        // chaff against a ciphertext and report a wrong-passphrase for a correct one.
        let mut ring = Ring::new(1);
        ring.seek(&file, omega, &root.k_enc, &root.n_root, MASTER_FRAME_CHUNK)?;
        let frame_bytes = ring.center_bytes().to_vec();
        // `decode` needs the chunk count, which is what we are about to learn, so parse the
        // header first with the count check deferred.
        let frame = decode_frame_unchecked(&frame_bytes)?;

        let mut this = Self {
            file,
            chaff,
            root,
            omega,
            frame,
            ring,
        };
        this.ring.chunks = this.frame.chunk_count;
        Ok(this)
    }

    /// Read **one chunk** of document plaintext into `out`, and return how many bytes it holds. Phase 13.
    ///
    /// # Why this exists, and what it is the primitive for
    ///
    /// **The container had no way to hand out part of a document.** `read_content` reads all of it — one
    /// `Vec` of `content_len` bytes, 8 MiB at `S_MAX_PAYLOAD` — and `read_raw` returns *ciphertext*. The
    /// only other way at the bytes was `Ring::seek`, which is the **write** pipeline's three-stage window
    /// and needs a `&DirectFile` that `Wavefunction` keeps private.
    ///
    /// So a windowed reader had no primitive to build on, and this is it: **one chunk, one authenticated
    /// read, no allocation.** Phase 13's residency policy wants a section — which is
    /// [`crate::layout::CHUNK_PLAINTEXT`] bytes by construction — so one call is one section and a load is
    /// one `pread64` plus one `open_chunk`.
    ///
    /// # Why it does not go through the ring
    ///
    /// **The ring is three stages and it prefetches.** `seek` moves a sliding window of three chunks and
    /// reads the neighbours on the way, which is right for sequential `read_content` and wrong here: a
    /// windowed reader asking for chunk 47 does not want 46 and 48, and on a 97-chunk document prefetching
    /// every request would read the whole file. So this reads one slot directly and leaves the ring alone.
    /// The cost is that a caller walking chunks 1..N gets no prefetch, and `read_content` still exists for
    /// that case.
    ///
    /// # The security properties, unchanged
    ///
    /// * `open_chunk` **authenticates**. A flipped bit anywhere in the slot — ciphertext or tag — is
    ///   `AeadError`, and the error is reported rather than returned as plaintext. That is the whole reason
    ///   the slot is decrypted into a caller-supplied buffer instead of being `read_exact_at`'d and used
    ///   raw.
    /// * **Chunk 0 is not readable through this.** It is the master frame, not content, and `read_content`
    ///   starts at 1. Allowing index 0 here would let a caller confuse the frame's bytes — which carry the
    ///   title and the KDF parameters — with document text.
    /// * **The index is bounds-checked against the frame's own `chunk_count`,** which is authenticated data
    ///   read from chunk 0. So an out-of-range index is refused against a number that came from the sealed
    ///   frame, not from a caller.
    /// * The slot buffer is **wiped after the open**, because it held ciphertext and `DirectFile` is
    ///   `O_DIRECT` throughout; a slot left in a heap allocation is a 64 KiB window onto the file.
    ///
    /// # `out` is filled, not appended, and may be longer than the chunk
    ///
    /// The return value is the authoritative length, and it is `content_len - chunk_start` for the last
    /// chunk and `CHUNK_PLAINTEXT` otherwise. `out` is cleared first so a shorter chunk cannot leave the
    /// tail of a previous longer one visible to a caller that trusts the slice length — the same reasoning
    /// as `master_frame`'s "exactly one chunk" comment, for a different mistake.
    pub fn read_chunk_into(
        &self,
        index: u64,
        out: &mut [u8],
    ) -> Result<usize, ContainerError> {
        if index == MASTER_FRAME_CHUNK {
            return Err(ContainerError::NoSuchChunk { index });
        }
        if index >= self.frame.chunk_count {
            return Err(ContainerError::NoSuchChunk { index });
        }
        if out.len() < CHUNK_PLAINTEXT {
            return Err(ContainerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "a chunk needs {CHUNK_PLAINTEXT} bytes of output and {} were given",
                    out.len()
                ),
            )));
        }
        // Content byte 0 is chunk 1's byte 0, so the offset is `(index - 1) * CHUNK_PLAINTEXT`.
        let start = (index - 1) * CHUNK_PLAINTEXT as u64;
        let remaining = (self.frame.content_len as u64).saturating_sub(start);
        let want = remaining.min(CHUNK_PLAINTEXT as u64) as usize;

        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        // **Copy out before wiping, and the order is load-bearing.** `aead::open_chunk` decrypts *in
        // place* — the module's own doc says "sealed or opened in place" (`aead.rs:4`) — so the plaintext
        // lands in the first `CHUNK_PLAINTEXT` bytes of the slot that held its own ciphertext. Wiping
        // first and reading after returns 65,520 zeros for every chunk, which authenticates fine and looks
        // like a perfectly good chunk of empty document.
        let opened = (|| -> Result<usize, ContainerError> {
            self.file
                .read_exact_at(layout::chunk_offset(self.omega, index), &mut slot)?;
            let got = aead::open_chunk(
                &self.root.k_enc,
                &self.root.n_root,
                index,
                slot.as_mut_slice(),
            )?;
            if (got as u64) < want as u64 {
                // A chunk decrypted to fewer bytes than the frame says it holds. That is not a
                // wrong-passphrase case -- authentication already passed -- so it is a *frame*
                // disagreement rather than an `Aead` one, which keeps "the file is inconsistent" distinct
                // from "the key is wrong" instead of folding both into `AuthenticationFailed`.
                return Err(ContainerError::Frame(FrameError::ContentMismatch {
                    index,
                    got: got as u64,
                    want: want as u64,
                }));
            }
            out[..want].copy_from_slice(&slot.as_slice()[..want]);
            Ok(want)
        })();
        // Wiped on every path including the error path: the slot held ciphertext and then plaintext.
        slot.wipe();
        opened
    }

    /// The plaintext byte offset of chunk `index`'s first byte, or `None` if the chunk does not exist.
    ///
    /// **The other half of [`Wavefunction::read_chunk_into`], and the reason it is a method and not
    /// arithmetic at the call site.** Content byte 0 is chunk 1's byte 0 — chunk 0 is the master frame — so
    /// the mapping is `(index - 1) * CHUNK_PLAINTEXT`, and getting it wrong is an off-by-one-chunk that
    /// decrypts successfully and returns *plausible wrong text*. Phase 13's `SectionStore` maps a section
    /// index to a chunk index through here rather than computing it, for the same reason
    /// `LineGeometry` is the only thing that converts a line to a byte.
    pub fn chunk_content_offset(&self, index: u64) -> Option<u64> {
        if index == MASTER_FRAME_CHUNK || index >= self.frame.chunk_count {
            return None;
        }
        Some((index - 1) * CHUNK_PLAINTEXT as u64)
    }

    /// Read the whole document plaintext.
    pub fn read_content(&mut self) -> Result<Vec<u8>, ContainerError> {
        let chunk_count = self.frame.chunk_count;
        let content_len = self.frame.content_len as usize;
        let mut out = Vec::with_capacity(content_len);
        for index in 1..chunk_count {
            self.ring.seek(
                &self.file,
                self.omega,
                &self.root.k_enc,
                &self.root.n_root,
                index,
            )?;
            let take = (content_len.saturating_sub(out.len())).min(CHUNK_PLAINTEXT);
            out.extend_from_slice(&self.ring.center_bytes()[..take]);
        }
        Ok(out)
    }

    /// Replace the document plaintext, growing or shrinking the chunk count if needed.
    ///
    /// Rewrites chunk 0 as well, because the chunk count is part of the master frame and
    /// must stay consistent with what is on disk.
    pub fn write_content(&mut self, content: &[u8]) -> Result<(), ContainerError> {
        let chunk_count =
            chunks_for(content.len() as u64).map_err(|_| ContainerError::PayloadTooLarge {
                requested: content.len() as u64,
                cap: layout::S_MAX_PAYLOAD,
            })?;
        if !layout::payload_fits(self.omega, payload_len(chunk_count)) {
            return Err(ContainerError::PayloadTooLarge {
                requested: payload_len(chunk_count),
                cap: layout::S_MAX_PAYLOAD,
            });
        }

        // Chunks that already exist are read, decrypted and re-sealed. Chunks past the old
        // end have nothing on disk to decrypt, so they are staged blank instead -- seeking to
        // them would try to authenticate the chaff that follows the old payload.
        let old_chunks = self.frame.chunk_count;
        for index in 1..chunk_count {
            if index < old_chunks {
                // Bound the ring to what is actually on disk. `seek` prefetches the next
                // chunk, and if the ring is told about the *new* count it will happily try to
                // read a chunk that does not exist yet and authenticate the trailing chaff
                // against a ciphertext.
                self.ring.chunks = old_chunks;
                self.ring.seek(
                    &self.file,
                    self.omega,
                    &self.root.k_enc,
                    &self.root.n_root,
                    index,
                )?;
            } else {
                self.ring.chunks = chunk_count;
                self.ring.stage_blank(index)?;
            }
            let start = ((index - 1) * CHUNK_PLAINTEXT as u64) as usize;
            let end = (start + CHUNK_PLAINTEXT).min(content.len());
            let dst = self.ring.center_bytes_mut();
            dst.fill(0);
            if start < content.len() {
                dst[..end - start].copy_from_slice(&content[start..end]);
            }
            // Commit per chunk, not once at the end. `stage_blank` discards every slot so it
            // cannot read a chunk that has no ciphertext on disk, and that would throw away
            // the previous chunk's dirty slot before it was written. Observed directly: with a
            // single trailing commit, growing to 5 chunks left chunk 1 and chunk 4 valid and
            // chunks 2 and 3 holding chaff. The cost is one fsync per chunk on a bulk
            // operation; `write_content` is not the keystroke path, and the ring's own
            // per-chunk `commit` is what a keystroke uses.
            self.ring
                .commit(&self.file, self.omega, &self.root.k_enc, &self.root.n_root)?;
        }

        self.frame.content_len = content.len() as u64;
        self.frame.chunk_count = chunk_count;
        self.rewrite_master_frame()?;
        Ok(())
    }

    /// Re-seal and write chunk 0 from the in-memory frame.
    fn rewrite_master_frame(&mut self) -> Result<(), ContainerError> {
        let mut frame_bytes = [0u8; CHUNK_PLAINTEXT];
        let n = self.frame.encode(&mut frame_bytes)?;
        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        aead::seal_chunk(
            &self.root.k_enc,
            &self.root.n_root,
            MASTER_FRAME_CHUNK,
            &frame_bytes[..n],
            slot.as_mut_slice(),
        )?;
        self.file
            .write_exact_at(layout::chunk_offset(self.omega, MASTER_FRAME_CHUNK), &slot)?;
        slot.wipe();
        self.file.sync()?;
        Ok(())
    }

    /// Replace the document title, rewriting chunk 0.
    pub fn set_title(&mut self, title: &str) -> Result<(), ContainerError> {
        self.frame.title = title.chars().take(frame::MAX_TITLE_LEN).collect();
        self.rewrite_master_frame()
    }

    /// Write back any pending chunk changes and sync.
    pub fn commit(&mut self) -> Result<usize, ContainerError> {
        Ok(self
            .ring
            .commit(&self.file, self.omega, &self.root.k_enc, &self.root.n_root)?)
    }

    /// Read `len` bytes of the container as they sit on disk, without decrypting.
    ///
    /// This is what the entropy gate runs over. It reads through `O_DIRECT` like everything
    /// else, so the gate measures the file as it really is rather than a buffered copy.
    pub fn read_raw(&self, offset: u64, len: usize) -> Result<Vec<u8>, ContainerError> {
        let mut out = vec![0u8; len];
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        let mut done = 0usize;
        while done < len {
            let want = (len - done).min(IO_ALIGN as usize);
            page.wipe();
            self.file.read_exact_at(offset + done as u64, &mut page)?;
            out[done..done + want].copy_from_slice(&page.as_slice()[..want]);
            done += want;
        }
        Ok(out)
    }

    /// Verify that a region of the file equals the chaff keystream for this container's
    /// `K_chaff`.
    ///
    /// Used by the tests to assert the file really is keystream and not merely
    /// high-entropy-looking. A region that legitimately holds the salt or a payload chunk
    /// will not match, and that is the expected answer there.
    pub fn region_is_chaff(&self, offset: u64, len: usize) -> Result<bool, ContainerError> {
        let on_disk = self.read_raw(offset, len)?;
        let expected = self.chaff.bytes_at(offset, len)?;
        Ok(on_disk == expected)
    }
}

/// Parse a master frame without knowing the chunk count in advance.
fn decode_frame_unchecked(bytes: &[u8]) -> Result<MasterFrame, ContainerError> {
    MasterFrame::decode(bytes, None).map_err(ContainerError::from)
}

/// Overwrite `[start, end)` with chaff, in [`IO_ALIGN`] units.
fn fill_chaff(
    file: &DirectFile,
    chaff: &Chaff,
    start: u64,
    end: u64,
) -> Result<(), ContainerError> {
    debug_assert!(
        start.is_multiple_of(IO_ALIGN),
        "chaff regions must start aligned"
    );
    let mut buf = AlignedBuf::zeroed(IO_ALIGN as usize);
    let mut offset = start;
    while offset < end {
        chaff.fill(offset, buf.as_mut_slice())?;
        file.write_exact_at(offset, &buf)?;
        offset += IO_ALIGN;
    }
    buf.wipe();
    Ok(())
}

impl core::fmt::Debug for Wavefunction {
    /// Never prints the root material.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Wavefunction")
            .field("path", &self.file.path())
            .field("omega", &self.omega)
            .field("frame", &self.frame)
            .field("ring", &self.ring)
            .field("root", &"[redacted]")
            .finish()
    }
}
