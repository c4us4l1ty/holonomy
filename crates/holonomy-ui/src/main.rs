//! `holonomy` — the binary.
//!
//! The editor itself is `wordcraft`. This entry point does three things and hands over:
//! parse arguments, initialise the container session (or report that there is none), and start the
//! `wordcraft` UI. It is intentionally thin: everything worth testing is in [`holonomy_ui`] or in
//! the crates below it, so the untested surface of the product is as small as it can be.
//!
//! # Current state
//!
//! This is the v5.1.0 **skeleton**, delivered with the spec audit rather than after it. The
//! container round-trip, the reaper, the pad plan and every invariant in `docs/PRD-v5.1.0.md` are
//! implemented and tested in the library crates. What is not yet wired is the `wordcraft` document
//! <-> container bridge, which is blocked on the two questions at the bottom of
//! `docs/AUDIT-v5.1.0.md` (§7): the `wordcraft` dependency graph needs `crates.io` (unreachable
//! from the sandbox where this was written) and its `craft-ui`/`craft-libs` git pins, and the
//! bridge's shape depends on whether the user wants wordcraft vendored or tracked.
//!
//! Rather than stub a `wordcraft` integration that cannot be compiled or verified, `main` reports
//! exactly that and exits. A binary that pretends to have the editor would be worse than one that
//! says what is missing.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("holonomy {}", holonomy_core::VERSION);
        println!("container: {} bytes, {} slots", holonomy_core::layout::CONTAINER_BYTES, holonomy_core::layout::TOTAL_BLOCKS);
        println!("pad budget: {} saves before domain reuse", holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE);
        return ExitCode::SUCCESS;
    }

    if args.iter().any(|a| a == "--check") {
        return self_check();
    }

    print_banner();
    ExitCode::SUCCESS
}

fn print_banner() {
    println!("holonomy {} (editor shell pending)", holonomy_core::VERSION);
    println!();
    println!("This build carries the audited v5.1.0 core:");
    println!("  * container geometry and the 65,536-byte capsule (holonomy-core)");
    println!("  * the 22-bit sector permutation and the 54-save pad budget (holonomy-core)");
    println!("  * deterministic KDF, CMT-4 committed AEAD, ML-KEM-1024 envelope (holonomy-crypto)");
    println!("  * aligned direct I/O, pad plan and sector ratchet (holonomy-usb)");
    println!("  * the zero-I/O PHY reaper and secret arenas (holonomy-runtime)");
    println!();
    println!("The wordcraft editor front end is not wired in this build. See");
    println!("docs/AUDIT-v5.1.0.md §7 for what is blocking it.");
    println!();
    println!("Run `holonomy --check` to verify the invariants on this machine.");
}

/// Run the machine-checkable invariants, so a user can confirm the build rather than trust it.
fn self_check() -> ExitCode {
    let mut ok = true;

    println!("holonomy {} self-check", holonomy_core::VERSION);

    // 1. Capsule size, which is a compile-time assert too — this is the runtime echo.
    let capsule = std::mem::size_of::<holonomy_core::capsule::StratumRootCapsuleV5>();
    report("capsule is exactly one 65,536-byte slot", capsule == 65_536, &format!("{capsule} bytes"));
    ok &= capsule == 65_536;

    // 2. Zone B geometry.
    match holonomy_usb::Geometry::nominal() {
        Ok(g) => {
            report(
                "8 GB device fits the documented pad budget",
                g.meets_pad_budget(),
                &format!("{} saves afforded", g.saves_afforded()),
            );
            ok &= g.meets_pad_budget();
        }
        Err(e) => {
            report("8 GB device geometry", false, &e.to_string());
            ok = false;
        }
    }

    // 3. The permutation actually reaches the whole of Zone B.
    let f = holonomy_core::feistel::Feistel22::provisioned(&[0x5Au8; 32]);
    let above_2_20 = (0..100_000u32)
        .filter(|&j| f.permute(holonomy_core::feistel::Feistel22::logical_sector(0, j)) >= (1 << 20))
        .count();
    report(
        "22-bit Feistel reaches sectors above 2^20 (FPE-20 could not)",
        above_2_20 > 35_000,
        &format!("{above_2_20}/100,000 samples"),
    );
    ok &= above_2_20 > 35_000;

    // 4. Memory envelope.
    let steady = holonomy_core::budgets::STEADY_STATE_CEILING_BYTES;
    let peak = holonomy_core::budgets::UNLOCK_PEAK_CEILING_BYTES;
    let fits = holonomy_core::budgets::check_against_envelope(steady, peak).is_ok();
    report(
        "memory envelope fits a 2 GiB host",
        fits,
        &format!(
            "{} MiB steady, {} MiB peak",
            steady / holonomy_core::budgets::MIB,
            peak / holonomy_core::budgets::MIB
        ),
    );
    ok &= fits;

    // 5. Memory locking: report the rung, do not assume it.
    let cap = holonomy_crypto::lock::probe_capability();
    println!("  [info] memory locking: {}", cap.claim());

    // 6. Capture exclusion.
    let capture = holonomy_ui::CaptureReport::platform_default();
    println!("  [info] capture exclusion: {}", capture.describe());

    println!();
    if ok {
        println!("all invariants hold");
        ExitCode::SUCCESS
    } else {
        println!("one or more invariants failed");
        ExitCode::FAILURE
    }
}

fn report(what: &str, pass: bool, detail: &str) {
    println!("  [{}] {what} — {detail}", if pass { "ok" } else { "FAIL" });
}
