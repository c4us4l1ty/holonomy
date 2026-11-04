//! `holonomy` — the binary.
//!
//! Three jobs and no more: report the build, verify this machine, and (once the editor front end
//! links) hand over to it.
//!
//! # Why `--check` lives here and not in `holonomy-ui`
//!
//! It is the only part of the product a user runs when they are worried. Putting it behind
//! `wordcraft`'s six-crate dependency graph — which transitively pulls `wordcraft-engine` →
//! `wordcraft-pdf` → `craft-pdf` — means the diagnostic is unavailable on exactly the machines most
//! likely to need it. So the binary depends on the core crates plus `holonomy-session`, and the
//! editor becomes a front end that this binary starts once its graph builds.

use std::process::ExitCode;

use holonomy_core::budgets::{self, MIB};
use holonomy_core::capsule::{CapsuleWire, ENCRYPTED_DIRECTORY_BYTES};
use holonomy_core::feistel::{Feistel22, DOMAIN};
use holonomy_core::layout::{
    CONTAINER_BYTES, IO_ALIGN, PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE, SLOT_BYTES,
    ZONE_B_SECTORS,
};
use holonomy_crypto::lock::{self, LockCapability};
use holonomy_session::check::{peak_rss_bytes, CheckReport, CheckResult};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        version();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--check") {
        return check();
    }

    // Everything else needs the editor.
    banner();
    ExitCode::SUCCESS
}

fn version() {
    println!("holonomy {}", holonomy_core::VERSION);
    println!(
        "container {} bytes, {} slots of {} bytes",
        CONTAINER_BYTES,
        CONTAINER_BYTES / SLOT_BYTES,
        SLOT_BYTES
    );
    println!(
        "pad budget {SAVES_BEFORE_DOMAIN_REUSE} saves ({PAD_SECTORS_PER_SAVE} sectors each, \
         {ZONE_B_SECTORS} in Zone B)"
    );
}

fn help() {
    println!(
        "holonomy {}\n\
         \n\
         USAGE:\n\
         \x20   holonomy --check      verify this machine against the documented invariants\n\
         \x20   holonomy --version    build and layout constants\n\
         \x20   holonomy --help       this text\n\
         \n\
         `--check` exits 0 if every gate that *can* run here passed. Findings are classified:\n\
         \x20   ok        measured and correct\n\
         \x20   degraded  this machine cannot provide it; you should know which protection you lack\n\
         \x20   FAIL      a gate that should hold does not; exit code 1\n\
         \x20   deferred  could not be established here, with the reason -- never counted as a pass\n",
        holonomy_core::VERSION
    );
}

fn banner() {
    println!(
        "holonomy {} — the editor front end is not in this binary.",
        holonomy_core::VERSION
    );
    println!();
    println!("This build carries the audited v5.1.0 session layer:");
    println!("  * container geometry and the 65,536-byte capsule      (holonomy-core)");
    println!("  * the 22-bit sector permutation and the 54-save budget (holonomy-core)");
    println!("  * deterministic KDF, CMT-4 AEAD, ML-KEM-1024 envelope  (holonomy-crypto)");
    println!("  * aligned direct I/O, pad plan and sector ratchet      (holonomy-usb)");
    println!("  * the zero-I/O PHY reaper and secret arenas            (holonomy-runtime)");
    println!("  * session, save cadence, status line and --check       (holonomy-session)");
    println!();
    println!("The wordcraft front end lives in holonomy-ui, which needs crates.io plus its pinned");
    println!("craft-* git dependencies. docs/AUDIT-v5.1.0.md §8 records what that blocks.");
    println!();
    println!("Run `holonomy --check` to verify this machine.");
}

/// Verify this machine and print every finding.
///
/// Nothing here is read from a constant that was also used to build the thing being checked, except
/// where the check *is* of a constant. The distinction matters: `capsule_is_one_slot` proves the
/// type's size, while `direct_io` and `page_locking` prove something about the host.
fn check() -> ExitCode {
    let mut report = CheckReport::new();

    // ---- Build-level invariants. These are cheap and they catch a miscompile. ----
    let capsule_size = std::mem::size_of::<CapsuleWire>();
    report.push(if capsule_size == SLOT_BYTES {
        CheckResult::ok(
            "capsule is exactly one slot",
            format!(
                "{capsule_size} bytes, aligned to {}",
                std::mem::align_of::<CapsuleWire>()
            ),
        )
    } else {
        CheckResult::failed(
            "capsule is exactly one slot",
            format!("{capsule_size} bytes, expected {SLOT_BYTES}"),
        )
    });

    let align = std::mem::align_of::<CapsuleWire>();
    report.push(if align == IO_ALIGN {
        CheckResult::ok("capsule alignment", format!("{align} bytes"))
    } else {
        CheckResult::failed(
            "capsule alignment",
            format!("{align} bytes, expected {IO_ALIGN}"),
        )
    });

    report.push(
        if CONTAINER_BYTES.is_multiple_of(SLOT_BYTES) && CONTAINER_BYTES == 134_217_728 {
            CheckResult::ok(
                "container is a whole number of slots",
                format!(
                    "{CONTAINER_BYTES} bytes = {} slots",
                    CONTAINER_BYTES / SLOT_BYTES
                ),
            )
        } else {
            CheckResult::failed(
                "container is a whole number of slots",
                format!("{CONTAINER_BYTES}"),
            )
        },
    );

    let dir_bytes = ENCRYPTED_DIRECTORY_BYTES + 16;
    report.push(if dir_bytes == 60_608 {
        CheckResult::ok(
            "directory field",
            format!("{ENCRYPTED_DIRECTORY_BYTES} + 16 tag = {dir_bytes}"),
        )
    } else {
        CheckResult::failed("directory field", format!("{dir_bytes}, expected 60,608"))
    });

    // ---- The pad claim, verified on this build rather than trusted. ----
    let f = Feistel22::provisioned(&[0x5Au8; 32]);
    let mut seen = std::collections::HashSet::new();
    let mut collisions = 0u64;
    for cursor in 0..SAVES_BEFORE_DOMAIN_REUSE {
        for j in 0..PAD_SECTORS_PER_SAVE {
            if !seen.insert(f.permute(Feistel22::logical_sector(cursor, j))) {
                collisions += 1;
            }
        }
    }
    report.push(
        if collisions == 0
            && seen.len() as u64 == SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64
        {
            CheckResult::ok(
                "54 saves read disjoint pad",
                format!("{} distinct sectors, 0 collisions", seen.len()),
            )
        } else {
            CheckResult::failed(
                "54 saves read disjoint pad",
                format!("{} distinct sectors, {collisions} collisions", seen.len()),
            )
        },
    );

    let above = seen.iter().filter(|&&s| s >= (1 << 20)).count();
    report.push(if above > 0 {
        CheckResult::ok(
            "permutation reaches the sectors FPE-20 excluded",
            format!(
                "{above} of {} read sectors are at or above 2^20 ({:.1}% of Zone B is up there)",
                seen.len(),
                (ZONE_B_SECTORS - (1 << 20)) as f64 / ZONE_B_SECTORS as f64 * 100.0
            ),
        )
    } else {
        CheckResult::failed(
            "permutation reaches the sectors FPE-20 excluded",
            "no sector at or above 2^20 was produced".to_string(),
        )
    });

    report.push(if DOMAIN >= ZONE_B_SECTORS {
        CheckResult::ok(
            "permutation domain covers Zone B",
            format!("2^22 = {DOMAIN} >= {ZONE_B_SECTORS}"),
        )
    } else {
        CheckResult::failed(
            "permutation domain covers Zone B",
            format!("{DOMAIN} < {ZONE_B_SECTORS}"),
        )
    });

    // ---- Host properties. These are why --check exists. ----
    let report_lock = lock::probe_capability();
    report.push(match report_lock {
        LockCapability::DirectMapExcluded => CheckResult::ok(
            "page locking",
            format!("{} — {}", report_lock.claim(), lock::describe_how()),
        ),
        LockCapability::ResidentNoDump | LockCapability::Resident => CheckResult::degraded(
            "page locking",
            format!("{} — {}", report_lock.claim(), lock::describe_how()),
        ),
        LockCapability::None => CheckResult::degraded(
            "page locking",
            format!("{} — {}", report_lock.claim(), lock::describe_how()),
        ),
    });

    report.push(
        match std::panic::catch_unwind(holonomy_runtime::reaper::probe_sched_class) {
            Ok(class) => {
                let name = class.describe();
                if class.is_real_time() {
                    CheckResult::ok("reaper priority", name.to_string())
                } else {
                    CheckResult::degraded("reaper priority", name.to_string())
                }
            }
            Err(_) => CheckResult::deferred("reaper priority", "probe panicked"),
        },
    );

    // ---- Memory. The gate that used to be a placeholder. ----
    match peak_rss_bytes() {
        Some(rss) => {
            // Unlock peak is not measured here: this process has not run Argon2id. Passing the
            // steady-state figure as both would be a fabrication, so the *ceiling* is passed for
            // the peak term and the report says what was actually measured.
            let steady = rss as usize;
            match budgets::check_against_envelope(steady, budgets::UNLOCK_PEAK_CEILING_BYTES) {
                Ok(()) => report.push(CheckResult::ok(
                    "measured RSS is inside the steady-state ceiling",
                    format!(
                        "{} KiB against {} MiB steady (unlock peak not exercised in this process)",
                        steady / 1024,
                        budgets::STEADY_STATE_CEILING_BYTES / MIB,
                    ),
                )),
                Err(e) => report.push(CheckResult::failed("measured RSS", e.to_string())),
            }
            report.peak_rss_bytes = Some(rss);
        }
        None => report.push(CheckResult::deferred(
            "peak RSS",
            "no /proc/self/status on this platform; not estimated, because a wrong number in a \
             security tool is worse than a visible absence",
        )),
    }

    report.push(CheckResult::deferred(
        "capture exclusion",
        "requires a window; lives in holonomy-ui, which cannot be built where wordcraft's \
         craft-pdf dependency fails",
    ));
    report.push(CheckResult::deferred(
        "USB device present and writable",
        "no --device argument given; pass one once the device path is known",
    ));

    print!("{}", report.render());

    // Exit code semantics, stated here because they are a product decision rather than an
    // implementation detail:
    //   0  every gate that could run passed. `degraded` and `deferred` do not fail: the first is a
    //      property of the machine (no memfd_secret, no CAP_SYS_NICE) and the second is a property
    //      of this build environment. Failing on either would make the exit code noise, and a noisy
    //      security signal gets ignored.
    //   1  something that should hold does not. That is always worth stopping for.
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
