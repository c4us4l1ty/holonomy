// NOTE: an earlier version of Phase 8 added a `build_atlas` *here*, duplicating
// [`crate::build_atlas`]. That was wrong twice over. It returned `Atlas` where the real one returns
// `(Atlas, BootReport)`, so every caller silently dropped the phase timings -- including the one that
// measures boot. And it decompressed into a plain `Vec<u8>` where the real one decompresses into a
// page-locked, `mlock`ed, zeroizing `SecureBlock`, which is the entire point of Phase 4's
// one-time pass: the expanded fonts are *supposed* to be scrubbed, and the duplicate made them
// ordinary heap that a later `Drop` would leave behind.
//
// There is one `build_atlas`. It lives at the crate root because it is the whole pass, not a
// payload detail, and it is the only place that knows the phases have to happen in order.
