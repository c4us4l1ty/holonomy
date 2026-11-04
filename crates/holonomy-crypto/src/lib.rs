//! Cryptography for Holonomy `v5.1.0`: deterministic derivation, a committed AEAD, the two-phase
//! post-quantum envelope, and the USB pad budget.
//!
//! # The rule that governs the whole crate
//!
//! **Nothing environmental enters a key or a keystream.** Not the clock, not the cycle counter, not
//! the instruction bytes at a symbol, not the return address, not the process id, not the OS, not
//! the build. Identical inputs produce identical keys, and a container encrypted on one machine
//! decrypts on another.
//!
//! This is a deliberate reversal of `v5.0.0-OMEGA`'s `PEED` construction, which hashed live `.text`
//! bytes, the caller's `RIP` and a `RDTSC` minimum into the `BLAKE2b` keystream derivation. That had
//! two independent defects: it is patchable (the constant it compares against is in `.rodata`), and
//! it is self-defeating (any patch, relocation, recompile or scheduler hiccup changes the key and
//! the document stops decrypting). `tests/static_audit.rs` enforces the absence at the source level
//! so this cannot regress quietly.
//!
//! # Module map
//!
//! | module | role |
//! | :--- | :--- |
//! | [`kdf`] | `Argon2id` → labelled subkeys. Deterministic. |
//! | [`commit`] | `CMT-4` key commitment + committed `XChaCha20-Poly1305`, and the nonce sequence. |
//! | [`envelope`] | Two-phase `ML-KEM-1024` + `X25519` envelope, `K_DEM` derivation. |
//! | [`unlock`] | The unlock state machine: passphrase + capsule + κ_USB → one `ActiveStratum`. |
//! | [`usb_pad`] | The 54-save pad budget and ratchet accounting. |
//! | [`lock`] | Per-OS page locking, with an honest capability ladder. |
//! | [`rng`] | The single OS-CSPRNG draw point. |

#![deny(missing_docs)]
// `unsafe` appears only in `lock.rs` (the mlock/madvise/vsyscall shims) and is documented at every
// site. Everything else is safe Rust.
#![deny(unsafe_op_in_unsafe_fn)]

/// `CMT-4` key commitment and the committed `XChaCha20-Poly1305` wrapper.
pub mod commit;
/// The two-phase `ML-KEM-1024` + `X25519` envelope.
pub mod envelope;
/// `Argon2id` → labelled subkeys. Deterministic by construction.
pub mod kdf;
/// Per-OS page locking, with an honest capability ladder.
pub mod lock;
/// The single OS-CSPRNG draw point.
pub mod rng;
/// The end-to-end unlock: passphrase + capsule + `κ_USB` → one `ActiveStratum`.
pub mod unlock;
/// The 54-save pad budget and ratchet accounting.
pub mod usb_pad;

pub use commit::{cmt4_tag, cmt4_verify, AeadError, CommittedCiphertext};
pub use envelope::{EnvelopeError, HybridKem, MlKem1024X25519};
pub use kdf::{derive_root_secret, derive_subkey, ParamsClass, RootSecret};
pub use lock::{LockCapability, LockReport};
pub use unlock::{unlock, unlock_fixed, UnlockRequest};
pub use usb_pad::{PadBudget, PadError, PadMode};
