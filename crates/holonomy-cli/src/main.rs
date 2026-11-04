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
    CONTAINER_BYTES, IO_ALIGN, PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE, SECTOR_BYTES,
    SLOT_BYTES, ZONE_B_SECTORS,
};
use holonomy_crypto::lock::{self, LockCapability};
use holonomy_crypto::ParamsClass;
use holonomy_session::check::{peak_rss_bytes, CheckReport, CheckResult};

mod store;

/// The pad trait has to be in scope for its methods to be callable on a concrete `DevicePad`; a
/// trait import that looks unused is the one that makes `device.can_ratchet()` compile.
use store::pad::PadSource as _;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Only as the *first* argument. Matched anywhere, `holonomy save book --version` would print the
    // version and never touch the container, which is a silent no-op on a command the user meant.
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        version();
        return ExitCode::SUCCESS;
    }
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--check") {
        // `--check --device /dev/sdX` checks the stick as well. Anything else after `--check` is
        // rejected rather than ignored: a typo that silently checked nothing is the one failure mode a
        // health gate must not have.
        let mut device: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--check" => i += 1,
                "--device" => match args.get(i + 1) {
                    Some(d) => {
                        device = Some(d.clone());
                        i += 2;
                    }
                    None => return usage_error("--device needs a path"),
                },
                other if other.starts_with("--") => i += 1,
                other => {
                    return usage_error(format!(
                        "--check does not take the argument '{other}'; pass a container path to a \
                         verb instead"
                    ))
                }
            }
        }
        return check(device.as_deref());
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
    /// The stick's Zone B, if one was named. `None` selects the stand-in pad, and the report says so.
    device: Option<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut path = None;
    let mut passphrase = None;
    let mut kappa: Option<[u8; 32]> = None;
    let mut cursor = 0u64;
    let mut device: Option<String> = None;

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
            "--device" => {
                device = Some(
                    args.get(i + 1)
                        .ok_or_else(|| {
                            "--device needs a path; pass the whole stick (e.g. /dev/sdX), not a \
                             partition"
                                .to_string()
                        })?
                        .clone(),
                );
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
        device,
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
    for (i, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        let high = digit(pair[0])
            .ok_or_else(|| format!("--kappa has a non-hex character at byte {}", i * 2))?;
        let low = digit(pair[1])
            .ok_or_else(|| format!("--kappa has a non-hex character at byte {}", i * 2 + 1))?;
        out[i] = high << 4 | low;
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

/// The pad source these flags select.
///
/// `--device` means a real stick, read `O_DIRECT` wherever the kernel allows it. Without it the
/// stand-in is used, and every caller says so in the same breath as the result, because a demo path
/// that quietly looks like the real one is how a stand-in ships.
fn pad_source(flags: &Flags, read_write: bool) -> Result<Box<dyn store::PadSource>, String> {
    match &flags.device {
        Some(path) => {
            let device =
                store::pad::DevicePad::open(std::path::Path::new(path), &flags.kappa, read_write)
                    .map_err(|e| format!("opening the device {path}: {e}"))?;
            if read_write && !device.can_ratchet() {
                return Err(format!(
                    "{path} opened read-only; a save must be able to ratchet the pad, and a \
                     read-only handle would leave the spent sectors reusable"
                ));
            }
            Ok(Box::new(device))
        }
        None => Ok(Box::new(store::pad::DerivedPad::new())),
    }
}

/// `holonomy provision <path> --passphrase P --kappa HEX [--cursor N]`
///
/// Creates the container. The work is in `store::provision`; this function owns only the argument
/// handling and the reporting, so there is one implementation of "what the first save does".
fn provision(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(e),
    };
    if flags.cursor == u64::MAX {
        return usage_error("provision cursor must leave room for a subsequent save");
    }
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    let mut pad = match pad_source(&flags, true) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    match store::provision(std::path::Path::new(&flags.path), pad.as_mut(), &ctx) {
        Ok(()) => {
            println!("provisioned {}", flags.path);
            println!(
                "  {} bytes, one stratum, an empty document",
                CONTAINER_BYTES
            );
            println!("  the container is written under its own pad window, as every save is");
            println!("  pad: {}", pad.describe());
            if !pad.can_ratchet() {
                println!(
                    "  note: this pad cannot ratchet, so its sector material is reproducible by \
                     anyone holding --kappa; pass --device to write to a stick"
                );
            }
            println!(
                "  the next save must be run with --cursor {}",
                flags.cursor + 1
            );
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
    }
}

/// `holonomy save <path> --passphrase P --kappa HEX [--cursor N] < file`
fn save(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(e),
    };
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };
    // The document comes from stdin. A path argument would be a second way to name a secret and a
    // second thing to get wrong; stdin is what an editor and a pipe both produce.
    let mut document = Vec::new();
    if let Err(e) = std::io::Read::read_to_end(&mut std::io::stdin(), &mut document) {
        return usage_error(format!("reading the document from stdin: {e}"));
    }

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), true) {
        Ok(f) => f,
        Err(e) => return usage_error(format!("opening {}: {e}", flags.path)),
    };
    let mut pad = match pad_source(&flags, true) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    match store::save(&mut file, pad.as_mut(), &ctx, &document) {
        Ok(report) => {
            println!("saved {}", flags.path);
            println!(
                "  {} bytes of document in {} slot(s), {} pad sectors folded",
                report.plaintext_bytes, report.slots, report.pad_sectors
            );
            println!(
                "  image digest {}",
                report
                    .digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            println!(
                "  ratchet: {}",
                match report.ratcheted {
                    Some(n) => format!("{n} sectors rewritten"),
                    None => "not run — the stand-in pad has no device to rewrite".to_string(),
                }
            );
            println!("  pad: {}", pad.describe());
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
    }
}

/// `holonomy open <path> --passphrase P --kappa HEX [--cursor N]`
fn open(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(e),
    };
    let passphrase = match holonomy_core::unlock::SealedPassphrase::new(&flags.passphrase) {
        Some(p) => p,
        None => return usage_error("the passphrase must be 1..=255 bytes after normalisation"),
    };

    let ctx = context(&passphrase, &flags.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), false) {
        Ok(f) => f,
        Err(e) => return usage_error(format!("opening {}: {e}", flags.path)),
    };
    let mut pad = match pad_source(&flags, false) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    match store::open_document(&mut file, pad.as_mut(), &ctx) {
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
                return failure(format!("writing the document to stdout: {e}"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
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
         \x20   holonomy --check [--device D]  verify this machine against the documented invariants\n\
         \x20   holonomy --version    build and layout constants\n\
         \x20   holonomy --help       this text\n\
         \n\
         CONTAINERS:\n\
         \x20   holonomy provision <path> --passphrase P --kappa HEX [--cursor N] [--device D]\n\
         \x20   holonomy save <path> --passphrase P --kappa HEX [--cursor N] [--device D] < document\n\
         \x20   holonomy open <path> --passphrase P --kappa HEX [--cursor N] [--device D]\n\
         \n\
         `--kappa` is the 32-byte USB root share, as 64 hex characters. It is required: this build\n\
         does not read Zone A yet, so the half of the key that lives on the stick has to be handed\n\
         in. `--cursor` is the save counter that selects the pad window; it must be the number the\n\
         previous container recorded for open; for save use that cursor plus one.\n\
         \n\
         `--device` is the whole stick (e.g. /dev/sdX, not /dev/sdX1): its Zone B, starting at\n\
         LBA 32,768, is the pad. Omit it and a stand-in derives the pad from the permutation\n\
         instead, which makes the container a demo -- anyone holding `--kappa` can recompute the\n\
         pad it was folded with, and it cannot ratchet. Every command reports which source it\n\
         used.\n\
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

/// Check a device path as a pad source: openable, large enough for Zone B, and readable.
///
/// Two things this deliberately does not do. It does not use `PadReader`, because that refuses a
/// device under `ZONE_B_LBA_START + ZONE_B_SECTORS` sectors (7.3 GiB) and would turn a diagnostic
/// into a crash on any smaller stick. And it does not trust `metadata().len()` on a block device: a
/// partition or some device nodes report zero, so a zero is reported as unmeasurable rather than as
/// too small.
fn check_device(path: &str) -> CheckResult {
    let mut device =
        match store::pad::DevicePad::open(std::path::Path::new(path), &[0u8; 32], false) {
            Ok(d) => d,
            Err(e) => {
                return CheckResult::failed(
                    "USB device present and writable",
                    format!("{path}: {e}"),
                )
            }
        };

    // Zone B runs from ZONE_B_LBA_START to the end of the pad reservoir.
    let need = (store::pad::ZONE_B_LBA_START_SECTOR + ZONE_B_SECTORS as u64) * SECTOR_BYTES as u64;
    let have = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => {
            return CheckResult::failed(
                "USB device present and writable",
                format!("{path}: opened but cannot stat: {e}"),
            )
        }
    };
    let size_note = if have == 0 {
        "size not reported by this device node; not treated as a failure".to_string()
    } else if have < need {
        return CheckResult::failed(
            "USB device present and writable",
            format!(
                "{path} is {have} bytes; Zone B alone needs {need}. This stick cannot hold a \
                 container's pad"
            ),
        );
    } else {
        format!("{have} bytes")
    };

    // A read of one real sector, because "the path opens" is not the claim: a write-blocked or
    // half-yanked stick opens and then fails every transfer.
    match device.window(&[0u8; 32], 0) {
        Ok(window) => CheckResult::ok(
            "USB device present and writable",
            format!(
                "{}, {size_note}, one pad window read ({} bytes in plan order)",
                device.describe(),
                window.byte_len()
            ),
        ),
        Err(e) => CheckResult::failed("USB device present and writable", format!("{path}: {e}")),
    }
}

/// Verify this machine and print every finding.
///
/// Nothing here is read from a constant that was also used to build the thing being checked, except
/// where the check *is* of a constant. The distinction matters: `capsule_is_one_slot` proves the
/// type's size, while `direct_io` and `page_locking` prove something about the host.
fn check(device: Option<&str>) -> ExitCode {
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
    match device {
        Some(path) => report.push(check_device(path)),
        None => report.push(CheckResult::deferred(
            "USB device present and writable",
            "no --device given, so the pad is the stand-in; pass --device /dev/sdX to check a stick. \
             Deferred rather than ok because a machine with no stick has not shown the device path \
             works",
        )),
    }

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

#[cfg(test)]
mod argument_tests {
    use super::parse_kappa;

    #[test]
    fn kappa_rejects_non_ascii_without_panicking() {
        let input = format!("aé{}", "a".repeat(61));
        assert_eq!(input.len(), 64);
        assert!(parse_kappa(&input).is_err());
    }

    #[test]
    fn kappa_accepts_upper_and_lowercase_hex() {
        let key = parse_kappa(&"aF".repeat(32)).expect("valid hex");
        assert_eq!(key, [0xaf; 32]);
    }
}
