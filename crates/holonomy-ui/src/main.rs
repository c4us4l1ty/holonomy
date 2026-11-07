//! `holonomy` — the binary.
//!
//! This is an unfinished shell, not a running editor. An experimental Wordcraft
//! document codec and session exist separately in `holonomy-wordcraft`, but they
//! are not connected to this binary. The self-check covers selected invariants;
//! it is not a security review or a test of the full user workflow.
//! See the repository README for current limitations and recovery warnings.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("holonomy {}", holonomy_core::VERSION);
        println!(
            "container: {} bytes, {} slots",
            holonomy_core::layout::CONTAINER_BYTES,
            holonomy_core::layout::TOTAL_BLOCKS
        );
        println!(
            "pad budget: {} saves before domain reuse",
            holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE
        );
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
    println!("This prototype contains v5.1.0 core components:");
    println!("  * container geometry and the 65,536-byte capsule (holonomy-core)");
    println!("  * the 22-bit sector permutation and the 54-save pad budget (holonomy-core)");
    println!("  * deterministic KDF, CMT-4 committed AEAD, ML-KEM-1024 envelope (holonomy-crypto)");
    println!("  * aligned direct I/O, pad plan and sector ratchet (holonomy-usb)");
    println!("  * the zero-I/O PHY reaper and secret arenas (holonomy-runtime)");
    println!();
    println!("The wordcraft editor front end is not wired in this build. See");
    println!("the repository README for current limitations.");
    println!();
    println!("Run `holonomy --check` for selected local invariant checks.");
}

/// Run the machine-checkable invariants, so a user can confirm the build rather than trust it.
fn self_check() -> ExitCode {
    let mut ok = true;

    println!("holonomy {} self-check", holonomy_core::VERSION);

    // 1. Capsule size, which is a compile-time assert too — this is the runtime echo.
    let capsule = std::mem::size_of::<holonomy_core::capsule::StratumRootCapsuleV5>();
    report(
        "capsule is exactly one 65,536-byte slot",
        capsule == 65_536,
        &format!("{capsule} bytes"),
    );
    ok &= capsule == 65_536;

    // 2. Zone B geometry.
    let g = holonomy_usb::Geometry::nominal();
    report(
        "8 GB device fits the documented pad budget",
        g.meets_pad_budget(),
        &format!("{} saves afforded", g.saves_afforded()),
    );
    ok &= g.meets_pad_budget();

    // 3. The permutation actually reaches the whole of Zone B.
    let f = holonomy_core::feistel::Feistel22::provisioned(&[0x5Au8; 32]);
    let above_2_20 = (0..100_000u32)
        .filter(|&j| {
            f.permute(holonomy_core::feistel::Feistel22::logical_sector(0, j)) >= (1 << 20)
        })
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
        println!("selected checks passed; this is not a security audit");
        ExitCode::SUCCESS
    } else {
        println!("one or more invariants failed");
        ExitCode::FAILURE
    }
}

fn report(what: &str, pass: bool, detail: &str) {
    println!("  [{}] {what} — {detail}", if pass { "ok" } else { "FAIL" });
}
