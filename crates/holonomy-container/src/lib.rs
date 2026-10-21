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
use layout::{VDF_ITERATIONS_LEN, VDF_ITERATIONS_OFFSET};

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
    /// **FR-4.9: the decoy's byte range overlaps the primary's.** Duress refused.
    ///
    /// **Refused rather than merged, and the reason is that the alternative destroys the document
    /// duress exists to protect.** `Ω` is uniform over the container, so a small decoy almost never
    /// collides — but "almost never" is a property of the sizes, and a large document makes it
    /// likely. Writing anyway would overwrite the primary with the decoy, and the operator would find
    /// out when the real document no longer opens, which is both the worst time and the worst way.
    ///
    /// **Both ranges are carried.** A bare error would say *something* is wrong; the two ranges are
    /// what makes it say which, and `duress` is a create-time decision the caller can re-take.
    DuressOverlap {
        /// The primary's `[start, end)` byte range.
        primary: std::ops::Range<u64>,
        /// The decoy's, which intersects it.
        decoy: std::ops::Range<u64>,
    },
    /// **FR-4.8: the ring's slots could not be page-locked.** Phase 16.
    ///
    /// **One variant for "the mapping failed", "the page lock was refused" and "the scrub registry
    /// is full"**, because all three have the same remedy and none of them is the caller's to fix:
    /// the container does not open. The usual cause is `RLIMIT_MEMLOCK`, which the boot chain prints
    /// and `the_reported_memlock_ceiling_is_the_real_one` pins against a live `getrlimit`.
    ///
    /// **It does not fall back to unlocked slots.** The fallback would be a ring running the weaker
    /// of two protections while its caller believed it was running the stronger, and plaintext
    /// residency is exactly what the ring exists to bound.
    SecureBlock,
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
    /// **The container's recorded VDF iteration count is above the ceiling.** FR-4.4 / DoS.
    ///
    /// **A separate variant, because this is the one error in this enum that is about an attack rather
    /// than about the file.** The iteration count is written in the clear beside the salt and read back
    /// before any key exists, so the file chooses how long the unlock takes. A file that asks for
    /// `u64::MAX` iterations is not a corrupt file; it is a file that hangs the program that opens it.
    ///
    /// **The refusal happens before the Argon2 stage**, so a hostile container costs one 4 KiB read and
    /// nothing else — not 128 MiB of memory and not half a second of CPU.
    VdfIterationsTooLarge {
        /// What the container asked for.
        requested: u64,
        /// The ceiling.
        cap: u64,
    },
}

/// **The decoy document a duress passphrase mounts.** FR-4.9.
///
/// A struct rather than three more parameters on `create_with_duress`, which would have had eight.
/// **And it is optional**, so [`create`](Wavefunction::create) is the same function with `None` and
/// every existing gate keeps testing the same code.
///
/// # The decoy should be boring, and "boring" is the caller's problem
///
/// Nothing here checks that `content` looks like something an operator would plausibly be working
/// on. **That is deliberate**: a predicate for "innocent enough to be believed" cannot be written by
/// the same project that also knows what a real document looks like, and a decoy that fails the
/// predicate would be *refused at write time* — which means the operator finds out they had no
/// decoy, at exactly the moment they needed one. A container with no decoy is a visible gap; a
/// container whose decoy was rejected is a surprise under duress.
#[derive(Debug, Clone, Copy)]
pub struct DuressDecoy<'a> {
    /// The duress passphrase. `P_B`.
    pub passphrase: &'a str,
    /// The decoy's title, sealed in the decoy's own master frame.
    pub title: &'a str,
    /// The decoy's content. Sealed in chunks at `Ω_B`.
    pub content: &'a [u8],
}

impl core::fmt::Display for ContainerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o: {e}"),
            Self::Aead(e) => write!(f, "authentication: {e}"),
            Self::Frame(e) => write!(f, "master frame: {e}"),
            Self::Ring(e) => write!(f, "ring: {e}"),
            Self::SecureBlock => f.write_str("the ring's slots could not be page-locked"),
            Self::DuressOverlap { primary, decoy } => write!(
                f,
                "the duress payload at {}..{} overlaps the primary at {}..{}, so writing it would \
                 destroy the document duress exists to protect",
                decoy.start, decoy.end, primary.start, primary.end
            ),
            Self::Chaff(e) => write!(f, "chaff: {e}"),
            Self::Envelope(e) => write!(f, "key derivation: {e}"),
            Self::PayloadTooLarge { requested, cap } => {
                write!(f, "{requested} bytes exceeds the {cap}-byte payload cap")
            }
            Self::NoEntropy => f.write_str("the operating system CSPRNG failed"),
            Self::NoSuchChunk { index } => {
                write!(f, "chunk {index} does not exist or is the master frame")
            }
            Self::VdfIterationsTooLarge { requested, cap } => write!(
                f,
                "the container asks for {requested} VDF iterations, above the {cap}-iteration ceiling"
            ),
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

    /// The VDF iteration count this container was written with.
    ///
    /// **From the master frame**, which is the copy that is authenticated — the plaintext one at
    /// [`layout::VDF_ITERATIONS_OFFSET`] is what the *derivation* used, and this is what the container
    /// then says about itself. They agree for any container this code wrote; they are read separately
    /// because only one of them is verifiable before the key exists and only one of them is
    /// authenticated after it does.
    ///
    /// Public so a test can assert a container reopened under a *different* caller's `T` is the one that
    /// got used, rather than inferring it from the fact that opening succeeded. `gate.rs`'s
    /// `a_container_opens_with_its_own_vdf_count_and_not_the_callers` is that test.
    pub fn vdf_iterations(&self) -> u64 {
        self.frame.vdf_iterations
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

    /// **FR-4.8: whether all three ring slots are page-locked.**
    ///
    /// Exposed because the requirement says *page-locked* and the size accessor above says nothing
    /// about what the bytes are. See [`Ring::slots_are_page_locked`] for the full argument; this is
    /// the same question one level up, where a caller holding a `Wavefunction` can ask it.
    pub fn ring_slots_are_page_locked(&self) -> bool {
        self.ring.slots_are_page_locked()
    }

    /// **Whether every ring slot's address satisfies `O_DIRECT`'s buffer-alignment rule.**
    ///
    /// A `SecureBlock`'s payload is `mmap`ed and therefore page-aligned, which is the whole reason a
    /// page-locked slot can be used as a direct-I/O buffer at all — **so swapping the backing is not
    /// free, and this is what says it was not broken.**
    pub fn ring_slot_alignment_is_direct_io_safe(&self) -> bool {
        self.ring.slots_are_direct_io_aligned()
    }

    /// The path this container was opened from.
    pub fn path(&self) -> &str {
        self.file.path()
    }

    /// **This container's file descriptor.**
    ///
    /// Exists for the teardown plan, which needs to `fsync` and `close` every descriptor the boot
    /// opened — and **after `seccomp` there is no `openat`**, so a descriptor that is not closed
    /// here is not closed at all. It was previously unreachable, which is why `main.rs` called
    /// `teardown_and_exit` with an empty plan and every edit committed since the last explicit
    /// `fsync` was left to the kernel's own writeback.
    ///
    /// **Read-only in practice.** Nothing in this crate re-opens, duplicates or rebinds it; it exists
    /// so the shutdown path can name it.
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.file.as_raw_fd()
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

    /// **Refuse an iteration count above the ceiling.**
    ///
    /// The count is either the caller's or the container's own recorded one, and the container's
    /// is attacker-controlled. See [`ContainerError::VdfIterationsTooLarge`] and
    /// [`holonomy_crypto::modulus::MAX_VDF_ITERATIONS`] for why.
    fn check_vdf_iterations(iterations: u64) -> Result<(), ContainerError> {
        let cap = holonomy_crypto::modulus::MAX_VDF_ITERATIONS;
        if iterations > cap {
            return Err(ContainerError::VdfIterationsTooLarge {
                requested: iterations,
                cap,
            });
        }
        Ok(())
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
        Self::create_with_duress(path, passphrase, title, content, None, vdf_iterations)
    }

    /// **Create a container that holds a second, innocent document.** FR-4.9.
    ///
    /// `duress: None` is exactly [`create`](Self::create) — the wrapper above is the only difference —
    /// so a file written this way is indistinguishable in *format* from one written without a decoy,
    /// and every existing gate keeps testing the same thing.
    ///
    /// # What FR-4.9 asks for, and which half already existed
    ///
    /// ```text
    /// Entry of Primary Passcode P_A resolves offset Ω_A, accessing the real document payload.
    /// Entry of Duress  Passcode P_B resolves decoy offset Ω_B (Ω_B ≠ Ω_A), mounting an alternate,
    ///                   non-sensitive document.
    /// Mounting the decoy must securely wipe local ephemeral cache files and emit realistic dummy
    /// modification timestamps without altering bits at Ω_A.
    /// ```
    ///
    /// **The offsets already worked.** `Self::derive` maps a passphrase to an `Ω` over the file's own
    /// salt, and `a_second_passphrase_resolves_a_different_offset` has gated that since Phase 3 —
    /// including the negative half, that the duress passphrase *cannot read the primary*.
    ///
    /// **What did not exist was a decoy to find.** A container had one payload, so `P_B` resolved a
    /// slot of chaff and the open failed — correct, and useless: the coerced operator who types `P_B`
    /// gets an error, not a boring document. **An error under duress is the worst possible answer**,
    /// because it tells the person holding the rubber hose that `P_B` is not the interesting one.
    ///
    /// # The three claims, and the one this does not make
    ///
    /// 1. `P_A` mounts the real document; `P_B` mounts the decoy; neither reads the other.
    /// 2. **Writing the decoy does not alter a single bit at `Ω_A`.** Checked, not assumed: the two
    ///    byte ranges are tested for overlap before anything is written, and the container's own
    ///    primary is read back afterwards.
    /// 3. **Caches are wiped when the decoy mounts.** This is *not* done here and **cannot be done
    ///    here** — see [`DuressDecoy`] and the note on the cache wipe below.
    ///
    /// **"Realistic dummy modification timestamps" is not implemented and is named as missing.**
    /// `MasterFrame` carries no timestamp field and the container is written through `O_DIRECT`,
    /// so the only timestamp that exists is the file's own `mtime` — which is a property of the
    /// single file, identical whichever payload is open, and therefore already "realistic" in the only
    /// sense the format has. **Adding a timestamp to the frame would be a format change** to close a
    /// requirement that the format does not otherwise present, and that is a decision to make
    /// deliberately rather than to smuggle in with a duress feature.
    pub fn create_with_duress(
        path: &Path,
        passphrase: &str,
        title: &str,
        content: &[u8],
        duress: Option<&DuressDecoy<'_>>,
        vdf_iterations: u64,
    ) -> Result<Self, ContainerError> {
        // **The caller's count is checked too, not just the container's.** A caller that asks for
        // `u64::MAX` iterations is asking for the same hang a hostile file would produce, and the
        // check costs nothing.
        Self::check_vdf_iterations(vdf_iterations)?;
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
        let ring = Ring::new(chunk_count)?;

        // Page 0 is chaff with the salt laid over its first 32 bytes. Writing the rest of
        // the page as chaff rather than zeroes matters: 4,064 zero bytes at a fixed offset
        // is exactly the cleartext structure FR-4.1 forbids.
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        chaff.fill(0, page.as_mut_slice())?;
        page.as_mut_slice()[..SALT_LEN].copy_from_slice(&salt);
        // **The VDF's iteration count, beside the salt, in the clear.**
        //
        // It has to be in the clear for the reason in [`layout::VDF_ITERATIONS_OFFSET`]: the master frame
        // that also records it is sealed under a key derived from the very number being recorded, so
        // reading it back requires having already run the computation.
        page.as_mut_slice()
            [VDF_ITERATIONS_OFFSET as usize..VDF_ITERATIONS_OFFSET as usize + VDF_ITERATIONS_LEN]
            .copy_from_slice(&vdf_iterations.to_le_bytes());
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

        Self::write_payload(&file, &root, omega, &frame, content, chunk_count)?;

        // **The decoy, if one was asked for.** Written after the primary so that the "no bit at Ω_A
        // changed" claim is checked against a file that is already complete.
        if let Some(d) = duress {
            let (root_b, omega_b) = Self::derive(d.passphrase, &salt, vdf_iterations)?;
            let chunks_b = chunks_for(d.content.len() as u64)
                .map_err(|_| ContainerError::PayloadTooLarge {
                    requested: d.content.len() as u64,
                    cap: layout::S_MAX_PAYLOAD,
                })?;
            let payload_b = payload_len(chunks_b);

            // **Overlap is refused, not merged.** A decoy that landed on the primary's bytes would
            // silently destroy the very document plausible deniability is protecting, and it would do
            // so at *write* time — the operator discovers it when the real file is already damaged.
            // Ω is uniform over the container, so for a small document this is rare; for a large one
            // it is not, and refusing is the only answer that is safe in both cases.
            let primary = omega..omega + payload_len(chunk_count);
            let decoy = omega_b..omega_b + payload_b;
            if decoy.start < primary.end && primary.start < decoy.end {
                return Err(ContainerError::DuressOverlap { primary, decoy });
            }
            if !layout::payload_fits(omega_b, payload_b) {
                return Err(ContainerError::PayloadTooLarge {
                    requested: payload_b,
                    cap: layout::S_MAX_PAYLOAD,
                });
            }

            let frame_b = MasterFrame::new(d.content.len() as u64, chunks_b, d.title)
                .with_kdf_params(
                    vdf_iterations,
                    envelope::ARGON2_M_COST_KIB,
                    envelope::ARGON2_T_COST,
                    envelope::ARGON2_P_COST,
                );
            Self::write_payload(&file, &root_b, omega_b, &frame_b, d.content, chunks_b)?;
            file.sync()?;
        }

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

    /// **Seal and write one payload at `omega`: the master frame at chunk 0, then chunks 1..N.**
    ///
    /// Extracted from `create` because FR-4.9 needs to write a *second* payload into the same file
    /// with a *different* root, and copying the seal-and-write loop would be two copies of the one
    /// piece of this crate that must not drift: a chunk sealed with the wrong nonce, or written at the
    /// wrong offset, is a container that opens as garbage.
    ///
    /// **`file` is borrowed, not taken, and nothing is synced.** The caller decides when the file is
    /// durable: `create` writes the chaff and the primary and then decides about the decoy, so an
    /// `fsync` between the two would be a wasted barrier on a 128 MiB file.
    fn write_payload(
        file: &DirectFile,
        root: &envelope::RootMaterial,
        omega: u64,
        frame: &MasterFrame,
        content: &[u8],
        chunk_count: u64,
    ) -> Result<(), ContainerError> {
        // Chunk 0: the master frame.
        let mut frame_bytes = [0u8; CHUNK_PLAINTEXT];
        let n = frame.encode(&mut frame_bytes)?;
        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        // **The predecessor tag is read, even on a path that is creating the container.**
        //
        // `create` lays chaff over the whole file before calling this, so the tag region of
        // every slot holds unpredictable chaff rather than the zeroes a zeroed buffer would
        // hold. Reading it is what makes that true — and `write_payload` is also the FR-4.9
        // decoy path, which writes a *second* payload at a different Ω over the first one, so
        // "this slot has never been written" is not a safe assumption to bake in here even if
        // it happens to hold for the primary.
        let prev = ring::prev_tag_from_disk(file, omega, MASTER_FRAME_CHUNK)?;
        aead::seal_chunk(
            &root.k_enc,
            &root.n_root,
            MASTER_FRAME_CHUNK,
            &prev,
            &frame_bytes[..n],
            slot.as_mut_slice(),
        )?;
        file.write_exact_at(layout::chunk_offset(omega, MASTER_FRAME_CHUNK), &slot)?;
        slot.wipe();

        // Chunks 1..N: the content.
        for index in 1..chunk_count {
            let start = ((index - 1) * CHUNK_PLAINTEXT as u64) as usize;
            let end = (start + CHUNK_PLAINTEXT).min(content.len());
            let piece = if start < content.len() {
                &content[start..end]
            } else {
                &[][..]
            };
            let prev = ring::prev_tag_from_disk(file, omega, index)?;
            aead::seal_chunk(
                &root.k_enc,
                &root.n_root,
                index,
                &prev,
                piece,
                slot.as_mut_slice(),
            )?;
            file.write_exact_at(layout::chunk_offset(omega, index), &slot)?;
        }
        slot.wipe();
        Ok(())
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
        // Page 0 carries the salt in its first 32 bytes and the VDF's iteration count in the next 8.
        // `O_DIRECT` cannot read 40 bytes -- offset, length and address must all be block multiples --
        // so read the page.
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        file.read_exact_at(SALT_OFFSET, &mut page)?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&page.as_slice()[..SALT_LEN]);
        let recorded_vdf = u64::from_le_bytes(
            page.as_slice()[VDF_ITERATIONS_OFFSET as usize
                ..VDF_ITERATIONS_OFFSET as usize + VDF_ITERATIONS_LEN]
                .try_into()
                .map_err(|_| {
                    ContainerError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the VDF iteration field is truncated",
                    ))
                })?,
        );
        drop(page);

        // **The container's own `T` wins over the caller's, and this is the whole point of the field.**
        //
        // Before it, `T` was a build-time constant the *caller* supplied and the container never said
        // what it had been built with. So a container written on a host whose squaring cost differs from
        // this one's could not be opened by this build: the root key derived here would not be the root
        // key the writer used. PROJECT.md recorded that as an ~8% gap on this host and an open
        // correctness hole on any host that moved.
        //
        // **A recorded zero falls back to the caller**, because that is what a container written before
        // this field existed has at this offset — and the chaff there is not guaranteed to be zero. The
        // fallback is safe in the only direction that matters: **if the count is wrong the derivation
        // produces a wrong root key and chunk 0 fails to authenticate**, so a wrong value cannot open a
        // container. A container that opens is one whose `T` matched; a fallback that guessed wrong fails
        // loudly rather than silently.
        let iterations = if recorded_vdf == 0 {
            vdf_iterations
        } else {
            recorded_vdf
        };

        // **The ceiling, checked here — at the boundary where the untrusted number enters.**
        //
        // This value came out of a file that anyone could write, and it is the length of a computation
        // this build is about to perform. Without the check, `T = u64::MAX` is not a failure but a
        // hang: the unlock never returns, the CPU is pinned at 100 % for as long as the user waits,
        // and the document cannot be opened at all.
        //
        // **The check is here and not only in the KDF** so that the refusal costs one 4 KiB read
        // instead of a 128 MiB Argon2 allocation and half a second of CPU. `holonomy-crypto` refuses
        // the same number independently — this is the outer of two walls, not the only one.
        Self::check_vdf_iterations(iterations)?;

        let (root, omega) = Self::derive(passphrase, &salt, iterations)?;
        let chaff = Chaff::new(&root.k_chaff);

        // Read chunk 0 with a ring bounded to exactly one chunk. The real chunk count is
        // inside chunk 0, so until it is read the ring must not prefetch a neighbour: a
        // one-chunk container has no chunk 1, and prefetching it would try to authenticate
        // chaff against a ciphertext and report a wrong-passphrase for a correct one.
        let mut ring = Ring::new(1)?;
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
    pub fn read_chunk_into(&self, index: u64, out: &mut [u8]) -> Result<usize, ContainerError> {
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
        // **No cast, and the cast is what this used to say.** `content_len` is already a `u64`; the
        // `as u64` was left from a version where it was `usize`, and it read as a deliberate widening
        // that a reader would otherwise have to check. **`saturating_sub` rather than `-` is the part
        // that matters here** and is unchanged: `start` can exceed the length when a page is short.
        let remaining = self.frame.content_len.saturating_sub(start);
        let want = remaining.min(CHUNK_PLAINTEXT as u64) as usize;

        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        // **Copy out before wiping, and the order is load-bearing.** `aead::open_chunk` decrypts *in
        // place* — the module's own doc says "sealed or opened in place" (`aead.rs:4`) — so the plaintext
        // lands in the first `CHUNK_PLAINTEXT` bytes of the slot that held its own ciphertext. Wiping
        // first and reading after returns 65,504 zeros for every chunk, which authenticates fine and looks
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

    /// Change the document's **length**, writing blank chunks for any that are new.
    ///
    /// # Why this is not `write_content`
    ///
    /// [`write_content`](Self::write_content) takes the whole document, which is precisely what Phase 13 exists
    /// to stop doing — a keystroke cannot hand over 8 MiB. But an edit *changes the document's length*, and a
    /// source that still believes the old length will hand back a short leaf at the end of the document, which
    /// `fault_leaf` refuses as `OutOfBounds`. **That is a length disagreement, not a truncation, and it is
    /// invisible at the edit site** — it surfaces as a failed read somewhere else in the file.
    ///
    /// The new chunks are blank because a grown region has no content yet: the rope's leaves hold it, and they
    /// are written back before they are evicted. So growth writes no document bytes at all, which is what makes
    /// it cheap enough to be called from an eviction.
    ///
    /// ## Shrinking leaves the freed chunks on disk
    ///
    /// Lowering `content_len` and `chunk_count` makes the trailing chunks **unreachable** — the ring refuses to
    /// seek past `chunk_count`, and `chunk_content_offset` returns `None` for them — so they can no longer be
    /// read as document content by this build. They are not *erased*, though: the bytes are ciphertext under a
    /// key that exists, so an attacker with the key could recover them. **This matches what
    /// [`write_content`](Self::write_content) already does on a shrink**, so it is not a new leak, but it is a
    /// real one and re-chaffing the freed region is the fix that neither path currently applies.
    pub fn set_content_len(&mut self, len: usize) -> Result<(), ContainerError> {
        let chunk_count = chunks_for(len as u64).map_err(|_| ContainerError::PayloadTooLarge {
            requested: len as u64,
            cap: layout::S_MAX_PAYLOAD,
        })?;
        if !layout::payload_fits(self.omega, payload_len(chunk_count)) {
            return Err(ContainerError::PayloadTooLarge {
                requested: payload_len(chunk_count),
                cap: layout::S_MAX_PAYLOAD,
            });
        }
        if chunk_count == self.frame.chunk_count && len as u64 == self.frame.content_len {
            return Ok(());
        }

        let old_chunks = self.frame.chunk_count;
        // **The ring's bound is `ring.chunks`, and this is the only caller that left it behind.**
        //
        // `Ring::check_range` refuses any index at or past `ring.chunks`, and `ring.chunks` is
        // assigned in exactly three places: on `open`, and twice inside `write_content`'s loop. This
        // function staged chunk `old_chunks` with the ring still bounded by the *old* count, so the
        // ring answered `OutOfRange { index: old_chunks, chunks: old_chunks }` on the first
        // iteration of the growth loop — **and the document could then never be saved again**, at
        // any size, because the count it is refusing is the count the frame has not been told about
        // yet. A 66 KiB document grew past one chunk, `Ctrl+S` reported the write failed, and
        // nothing but closing the file cleared it.
        //
        // Set before the loop, so `stage_blank` and `commit` are bounded by the count this call is
        // establishing. Narrowing on a shrink is the harmless direction: every read path is gated on
        // the authenticated `frame.chunk_count`, so a ring that knows about more chunks than the
        // frame does cannot serve a byte the frame would not ask for.
        self.ring.chunks = chunk_count;
        if chunk_count > old_chunks {
            // **Commit per chunk, as `write_content` does**, for the same reason: `stage_blank` discards every
            // slot, so a single trailing commit would drop the previous chunk's dirty slot before it was written.
            for index in old_chunks..chunk_count {
                self.ring.stage_blank(index)?;
                // **A blank chunk is not a written chunk, and the difference is this call.**
                // `stage_blank` deliberately leaves the slot *clean* -- a chunk that has been staged but not
                // given content must not be committed over the payload -- so `commit` below skipped it, and
                // the new chunk was never stored. The frame then claimed a chunk the disk did not have, which
                // is chaff, and the first read of the grown region failed authentication on it. The write was
                // the second half of the same defect as the missing `ring.chunks` above: neither the ring's
                // bound nor the ring's dirty flag was moved to the count this call was establishing.
                //
                // `center_bytes_mut` is what marks the slot dirty, and zeroes are what a grown region holds.
                self.ring.center_bytes_mut().fill(0);
                self.ring
                    .commit(&self.file, self.omega, &self.root.k_enc, &self.root.n_root)?;
            }
        }
        self.frame.content_len = len as u64;
        self.frame.chunk_count = chunk_count;
        self.rewrite_master_frame()
    }

    /// Re-seal and write chunk 0 from the in-memory frame.
    ///
    /// **Chunk 0 is the chunk that is rewritten most often** — every `write_content`, every
    /// `set_content_len`, every `set_title` — so it is the chunk a deterministic nonce reuses
    /// most, and the one whose two ciphertexts XOR to the most readable difference. The
    /// predecessor tag is therefore read from the file rather than assumed, which is what
    /// `ring::prev_tag_from_disk` is for. It also reads only 4 KiB of a slot that is about to
    /// be overwritten with 64 KiB.
    fn rewrite_master_frame(&mut self) -> Result<(), ContainerError> {
        let mut frame_bytes = [0u8; CHUNK_PLAINTEXT];
        let n = self.frame.encode(&mut frame_bytes)?;
        let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
        let prev = ring::prev_tag_from_disk(&self.file, self.omega, MASTER_FRAME_CHUNK)?;
        aead::seal_chunk(
            &self.root.k_enc,
            &self.root.n_root,
            MASTER_FRAME_CHUNK,
            &prev,
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
    ///
    /// # The frame is not touched until the rewrite has succeeded
    ///
    /// This assigns `self.frame.title` and *then* calls `rewrite_master_frame`, which can
    /// fail on `MasterFrame::encode` with `TitleTooLong`. Assigning first meant a rejected
    /// title left the handle holding a title it can never encode: **every later
    /// `write_content`, `set_content_len` and `commit` then failed for the rest of the
    /// process's life**, because they all go through the same frame, with an error that names
    /// nothing the caller did wrong. The candidate is built, encoded-checked by the rewrite,
    /// and only then adopted.
    pub fn set_title(&mut self, title: &str) -> Result<(), ContainerError> {
        // **Truncate by bytes, not by chars.**
        //
        // `MasterFrame::new` takes `MAX_TITLE_LEN` *chars* while `encode` refuses anything
        // over `MAX_TITLE_LEN` *bytes*, and a character is up to 4 bytes — so 512 chars of a
        // non-ASCII title is up to 2,048 bytes and always rejected. `frame::truncate_title` is
        // the shared, byte-budgeted truncator, so this and `MasterFrame::new` cannot drift back
        // into disagreeing about what the limit means.
        let previous = core::mem::replace(&mut self.frame.title, frame::truncate_title(title));
        match self.rewrite_master_frame() {
            Ok(()) => Ok(()),
            Err(e) => {
                // Put back exactly what was there, so a refused title leaves no trace.
                self.frame.title = previous;
                Err(e)
            }
        }
    }

    /// Replace chunk `index`'s **plaintext** and write it to disk.
    ///
    /// # The missing primitive behind Phase 13 part 8
    ///
    /// Everything else in this crate either reads a chunk or rewrites the whole payload through
    /// [`write_content`](Self::write_content). Neither can serve a document being edited in place, where
    /// **one chunk's 65,504 bytes have changed and the other 126 have not** — and
    /// `write_content` cannot be used for it because it takes the whole document, which is precisely what
    /// Phase 13 exists to stop doing.
    ///
    /// `index` is a **chunk index**, and index 0 is refused because it is the master frame
    /// ([`chunk_content_offset`](Self::chunk_content_offset) is the authority on that). `plaintext` may be
    /// shorter than a full chunk — the last chunk of a document is — and must not be longer.
    ///
    /// ## It is a synchronous write, and that is a property rather than an accident
    ///
    /// Stage, then commit, then return. A caller that wants to batch several chunks pays this once per
    /// chunk; a caller that stages without committing gets nothing, because `stage_plaintext` leaves the
    /// bytes in the ring's scratch slot and the ring holds only three. **A failed write leaves nothing
    /// half-applied to this chunk**: the ring drops the slot's resident and dirty flags at the moment it
    /// is sealed rather than after the write lands, so the next read re-fetches from the disk instead of
    /// handing back sealed bytes, and a retry cannot re-seal them into the chunk's plaintext. **Other
    /// chunks a single [`commit`](Self::commit) had already written are not rolled back** — it is a
    /// sequence of independent writes, and the error says which one failed.
    pub fn write_chunk(&mut self, index: u64, plaintext: &[u8]) -> Result<(), ContainerError> {
        if self.chunk_content_offset(index).is_none() {
            return Err(ContainerError::NoSuchChunk { index });
        }
        self.ring
            .stage_plaintext(index, plaintext)
            .map_err(ContainerError::Ring)?;
        self.commit().map(|_| ())
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
    ///
    /// # The range is checked, because this is a `pub` API with caller-supplied numbers
    ///
    /// `offset + len` was computed unchecked, and the release profile has `overflow-checks` on,
    /// so an offset near `u64::MAX` **panicked** rather than returning an error. The range is also
    /// refused when it runs past the end of the container, which bounds the allocation to 128 MiB
    /// — a caller cannot ask for a gigabyte by asking for a gigabyte.
    pub fn read_raw(&self, offset: u64, len: usize) -> Result<Vec<u8>, ContainerError> {
        let end = offset.checked_add(len as u64).ok_or_else(|| {
            ContainerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("offset {offset} plus {len} bytes is not representable"),
            ))
        })?;
        if end > CONTAINER_SIZE {
            return Err(ContainerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{offset}..{end} is outside the {CONTAINER_SIZE}-byte container"),
            )));
        }
        let mut out = vec![0u8; len];
        let mut page = AlignedBuf::zeroed(IO_ALIGN as usize);
        let mut done = 0usize;
        while done < len {
            let want = (len - done).min(IO_ALIGN as usize);
            page.wipe();
            self.file
                .read_exact_at(offset + done as u64, &mut page)?;
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
