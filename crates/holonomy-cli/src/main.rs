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
use holonomy_crypto::ParamsClass;
use holonomy_session::check::{peak_rss_bytes, CheckReport, CheckResult};

mod store;

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

    // The three verbs that touch a file. They come before the banner because they are the whole
    // program for anyone who is not opening a window.
    if let Some(rest) = args.strip_prefix(&["save".to_string()][..]) {
        return save(rest);
    }
    if let Some(rest) = args.strip_prefix(&["open".to_string()][..]) {
        return open(rest);
    }
    if let Some(rest) = args.strip_prefix(&["provision".to_string()][..]) {
        return provision(rest);
    }

    // Everything else needs the editor.
    banner();
    ExitCode::SUCCESS
}

/// Parsed flags shared by `save`, `open` and `provision`.
///
/// Hand-rolled rather than a dependency: the whole surface is four flags, and a CLI parsing crate in
/// a binary whose job is to be auditable is a crate somebody has to audit.
struct Flags {
    path: String,
    passphrase: String,
    kappa: [u8; 32],
    cursor: u64,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut path = None;
    let mut passphrase = None;
    let mut kappa: Option<[u8; 32]> = None;
    let mut cursor = 0u64;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--passphrase" => {
                passphrase = args.get(i + 1).cloned();
                i += 2;
            }
            "--kappa" => {
                let hex = args
                    .get(i + 1)
                    .ok_or_else(|| "--kappa needs 64 hex characters".to_string())?;
                kappa = Some(parse_kappa(hex)?);
                i += 2;
            }
            "--cursor" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--cursor needs a number".to_string())?;
                cursor = v
                    .parse()
                    .map_err(|_| format!("--cursor '{v}' is not a number"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{other}'"));
            }
            other => {
                path = Some(other.to_string());
                i += 1;
            }
        }
    }

    Ok(Flags {
        path: path.ok_or_else(|| "a container path is required".to_string())?,
        passphrase: passphrase.ok_or_else(|| "--passphrase is required".to_string())?,
        kappa: kappa.ok_or_else(|| {
            "--kappa is required: the USB root share is half the key, and this build does not read \
             Zone A yet"
                .to_string()
        })?,
        cursor,
    })
}

/// Decode 64 hex characters into the 32-byte USB root share.
fn parse_kappa(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!(
            "--kappa must be 64 hex characters (32 bytes); got {}",
            hex.len()
        ));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = &hex[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("--kappa has a non-hex byte '{pair}' at position {i}"))?;
    }
    Ok(out)
}

fn context<'a>(
    passphrase: &'a holonomy_core::unlock::SealedPassphrase,
    kappa: &'a [u8; 32],
    cursor: u64,
) -> store::SaveContext<'a> {
    store::SaveContext {
        passphrase,
        kappa_usb: kappa,
        topology: holonomy_core::unlock::Topology::UmbilicalUsb,
        params: ParamsClass::Legacy,
        cursor,
    }
}

/// `holonomy provision <path> --passphrase P --kappa HEX [--cursor N]`
///
/// Creates the container. The work is in `store::provision`; this function owns only the argument
/// handling and the reporting, so there is one implementation of "what the first save does".
fn provision(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(&e),
    };
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    match store::provision(std::path::Path::new(&flags.path), &ctx) {
        Ok(()) => {
            println!("provisioned {}", flags.path);
            println!(
                "  {} bytes, one stratum, an empty document",
                CONTAINER_BYTES
            );
            println!("  the container is written under its own pad window, as every save is");
            println!(
                "  the next save must be run with --cursor {}",
                flags.cursor + 1
            );
            ExitCode::SUCCESS
        }
        Err(e) => failure(&e),
    }
}

/// `holonomy save <path> --passphrase P --kappa HEX [--cursor N] < file`
fn save(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(&e),
    };
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };
    // The document comes from stdin. A path argument would be a second way to name a secret and a
    // second thing to get wrong; stdin is what an editor and a pipe both produce.
    let mut document = Vec::new();
    if let Err(e) = std::io::Read::read_to_end(&mut std::io::stdin(), &mut document) {
        return usage_error(&format!("reading the document from stdin: {e}"));
    }

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), true) {
        Ok(f) => f,
        Err(e) => return usage_error(&format!("opening {}: {e}", flags.path)),
    };
    match store::save(&mut file, &ctx, &document) {
        Ok(report) => {
            println!("saved {}", flags.path);
            println!(
                "  {} bytes of document in {} slot(s), {} pad sectors folded",
                report.plaintext_bytes, report.slots, report.pad_sectors
            );
            println!(
                "  image digest {}",
                report.digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            println!(
                "  ratchet: {}",
                match report.ratcheted {
                    Some(n) => format!("{n} sectors rewritten"),
                    None => "not run — this build writes a file, not a stick".to_string(),
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => failure(&e),
    }
}

/// `holonomy open <path> --passphrase P --kappa HEX [--cursor N]`
fn open(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(&e),
    };
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), false) {
        Ok(f) => f,
        Err(e) => return usage_error(&format!("opening {}: {e}", flags.path)),
    };
    match store::open_document(&mut file, &ctx) {
        Ok((opened, document)) => {
            eprintln!(
                "opened {}: stratum L_{}, {} slot(s), save cursor {}",
                flags.path,
                opened.stratum.id().index(),
                opened.directory.slot_count(),
                opened.cursor()
            );
            use std::io::Write;
            if let Err(e) = std::io::stdout().write_all(&document) {
                return failure(&format!("writing the document to stdout: {e}"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => failure(&e),
    }
}

/// `impl AsRef<str>` rather than `&str`: a `String` from `format!` and a `&'static str` literal are
/// both call sites, and a plain `&str` forces every formatted one to be borrowed at the point of
/// the call -- which reads as `&e` where `e: String` and then fails with "the size for values of
/// type `str` cannot be known", an error that names the deref rather than the mismatch.
fn usage_error(message: impl AsRef<str>) -> ExitCode {
    eprintln!("holonomy: {}", message.as_ref());
    eprintln!("run `holonomy --help` for usage");
    ExitCode::from(2)
}

fn failure(message: impl AsRef<str>) -> ExitCode {
    eprintln!("holonomy: {}", message.as_ref());
    ExitCode::FAILURE
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
         CONTAINERS:\n\
         \x20   holonomy provision <path> --passphrase P --kappa HEX [--cursor N]\n\
         \x20   holonomy save <path> --passphrase P --kappa HEX [--cursor N] < document\n\
         \x20   holonomy open <path> --passphrase P --kappa HEX [--cursor N]\n\
         \n\
         `--kappa` is the 32-byte USB root share, as 64 hex characters. It is required: this build\n\
         does not read Zone A yet, so the half of the key that lives on the stick has to be handed\n\
         in. `--cursor` is the save counter that selects the pad window; it must be the number the\n\
         previous save recorded, and 0 for a container that has only been provisioned.\n\
         \n
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
    //
    // What this process has actually done matters to how the number is judged. It verified
    // constants and probed the host; it did not open the container or run Argon2id. Measuring a few
    // MiB is therefore correct, and comparing that to the 180 MiB steady-state ceiling would be
    // comparing an observation to a claim about a different program. `Workload::Idle` says so
    // explicitly, so the check still fails if a few MiB becomes hundreds.
    //
    // The unlock peak is *not* measured here, because it has not happened. Reporting the steady
    // figure as the peak would be a fabrication of exactly the kind this project exists to avoid,
    // so the peak is a Deferred with its reason.
    match peak_rss_bytes() {
        Some(rss) => {
            let measured = rss as usize;
            match budgets::check_measurement(measured, budgets::Workload::Idle) {
                Ok(()) => report.push(CheckResult::ok(
                    "measured peak RSS is inside the envelope",
                    format!(
                        "{} KiB against {} MiB ceiling; this process did not unlock, so the \
                         {} MiB steady-state claim is not being checked here",
                        measured / 1024,
                        budgets::UNLOCK_PEAK_CEILING_BYTES / MIB,
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
        "steady-state and unlock-peak RSS",
        "requires opening the container and running Argon2id; run against a real device once one \
         is attached, because a claim measured without the workload is not a measurement of it",
    ));

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
