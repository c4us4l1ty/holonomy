//! `holonomy` — the binary.
//!
//! Report the build, verify this machine, and act on containers: `provision`, `save`, `open`,
//! `verify`, `remnants`, `recover` and `recover-copy`. The editor front end, once it links, is
//! started from here.
//!
//! # Why `--check` lives here and not in `holonomy-ui`
//!
//! It is the only part of the product a user runs when they are worried. Putting it behind
//! `wordcraft`'s six-crate dependency graph — which transitively pulls `wordcraft-engine` →
//! `wordcraft-pdf` → `craft-pdf` — means the diagnostic is unavailable on exactly the machines most
//! likely to need it. So the binary depends on the core crates plus `holonomy-session`, and the
//! editor becomes a front end that this binary starts once its graph builds.

use std::io::Write;
use std::process::ExitCode;

use zeroize::Zeroizing;

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

use holonomy_cli::store;

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
    if args.first().map(String::as_str) == Some("--check") {
        // `--check --device /dev/sdX` checks the stick as well. Anything else after `--check` is
        // rejected rather than ignored: a typo that silently checked nothing is the one failure mode a
        // health gate must not have.
        let mut device: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--check" if i == 0 => i += 1,
                "--device" => match args.get(i + 1) {
                    Some(d) if !d.starts_with("--") && device.is_none() => {
                        device = Some(d.clone());
                        i += 2;
                    }
                    _ => {
                        return usage_error(
                            "--device needs one path and may only be specified once",
                        )
                    }
                },
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

    // The container verbs that touch a file. They come before the banner because they are the whole
    // program for anyone who is not opening a window.
    if let Some(rest) = args.strip_prefix(&["save".to_string()][..]) {
        return save(rest);
    }
    if let Some(rest) = args.strip_prefix(&["open".to_string()][..]) {
        return open(rest);
    }
    if let Some(rest) = args.strip_prefix(&["verify".to_string()][..]) {
        return verify(rest);
    }
    if let Some(rest) = args.strip_prefix(&["provision".to_string()][..]) {
        return provision(rest);
    }
    if let Some(rest) = args.strip_prefix(&["remnants".to_string()][..]) {
        return remnants(rest);
    }
    if let Some(rest) = args.strip_prefix(&["recover".to_string()][..]) {
        return recover(rest);
    }
    if let Some(rest) = args.strip_prefix(&["recover-copy".to_string()][..]) {
        return recover_copy(rest);
    }

    if args.is_empty() {
        banner();
        ExitCode::SUCCESS
    } else {
        usage_error(format!("unknown command '{}'", args[0]))
    }
}

/// Parsed flags shared by `save`, `open`, `verify` and `provision`.
///
/// Hand-rolled rather than a dependency: the whole surface is four flags, and a CLI parsing crate in
/// a binary whose job is to be auditable is a crate somebody has to audit.
struct Flags {
    path: String,
    passphrase: Option<Zeroizing<String>>,
    kappa: Option<Zeroizing<[u8; 32]>>,
    cursor: u64,
    cursor_explicit: bool,
    /// The stick's Zone B, if one was named. `None` selects the stand-in pad, and the report says so.
    device: Option<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut path = None;
    let mut passphrase = None;
    let mut kappa = None;
    let mut cursor = None;
    let mut device = None;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag.starts_with("--") {
            if !matches!(flag, "--passphrase" | "--kappa" | "--device" | "--cursor") {
                return Err(format!("unknown flag '{flag}'"));
            }
            let value = args
                .get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{flag} needs a value"))?;
            match flag {
                "--passphrase" if passphrase.is_none() => {
                    passphrase = Some(Zeroizing::new(value.clone()))
                }
                "--kappa" if kappa.is_none() => kappa = Some(Zeroizing::new(parse_kappa(value)?)),
                "--device" if device.is_none() => device = Some(value.clone()),
                "--cursor" if cursor.is_none() => {
                    cursor = Some(
                        value
                            .parse()
                            .map_err(|_| format!("--cursor '{value}' is not a number"))?,
                    );
                }
                _ => return Err(format!("{flag} may only be specified once")),
            }
            i += 2;
        } else {
            if path.is_some() {
                return Err("only one container path may be specified".to_string());
            }
            path = Some(flag.to_string());
            i += 1;
        }
    }

    Ok(Flags {
        path: path.ok_or_else(|| "a container path is required".to_string())?,
        passphrase,
        kappa,
        cursor: cursor.unwrap_or(0),
        cursor_explicit: cursor.is_some(),
        device,
    })
}

/// Credentials held only as long as a command needs them. Prompt buffers and the
/// copied root share are wiped on drop; command-line arguments remain visible to the OS.
struct Credentials {
    passphrase: holonomy_core::unlock::SealedPassphrase,
    kappa: Zeroizing<[u8; 32]>,
}

fn credentials_with_prompt(
    flags: &Flags,
    confirm: bool,
    mut prompt: impl FnMut(&str) -> Result<String, String>,
) -> Result<Credentials, String> {
    let prompted = flags.passphrase.is_none();
    let pass = Zeroizing::new(match &flags.passphrase {
        Some(value) => value.to_string(),
        None => prompt("Passphrase: ")?,
    });
    if confirm && prompted {
        let repeated = Zeroizing::new(prompt("Confirm passphrase: ")?);
        if *pass != *repeated {
            return Err("passphrases do not match".into());
        }
    }
    let passphrase = holonomy_core::unlock::SealedPassphrase::new(&pass)
        .ok_or_else(|| "the passphrase must be 1..=255 bytes after lowercasing".to_string())?;
    let kappa = match &flags.kappa {
        Some(value) => **value,
        None => {
            let value = Zeroizing::new(prompt("USB root share (64 hex characters): ")?);
            let parsed = Zeroizing::new(parse_kappa(&value)?);
            if confirm {
                let repeated = Zeroizing::new(prompt("Confirm USB root share: ")?);
                let repeated = Zeroizing::new(parse_kappa(&repeated)?);
                if *parsed != *repeated {
                    return Err("USB root shares do not match".into());
                }
            }
            *parsed
        }
    };
    Ok(Credentials {
        passphrase,
        kappa: Zeroizing::new(kappa),
    })
}

fn credentials(flags: &Flags, confirm: bool) -> Result<Credentials, String> {
    credentials_with_prompt(flags, confirm, |label| {
        rpassword::prompt_password(label)
            .map_err(|e| format!("cannot read credentials from the terminal: {e}"))
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
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
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
fn pad_source(
    flags: &Flags,
    kappa: &[u8; 32],
    read_write: bool,
) -> Result<Box<dyn store::PadSource>, String> {
    match &flags.device {
        Some(path) => {
            let device = store::pad::DevicePad::open(std::path::Path::new(path), kappa, read_write)
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
    if flags.cursor != 0 {
        return usage_error("provision cursor must be 0; pad history is not tracked yet");
    }
    let credentials = match credentials(&flags, true) {
        Ok(c) => c,
        Err(e) => return usage_error(e),
    };

    let ctx = context(&credentials.passphrase, &credentials.kappa, flags.cursor);
    let mut pad = match pad_source(&flags, &credentials.kappa, true) {
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
    let credentials = match credentials(&flags, false) {
        Ok(c) => c,
        Err(e) => return usage_error(e),
    };
    // The document comes from stdin. A path argument would be a second way to name a secret and a
    // second thing to get wrong; stdin is what an editor and a pipe both produce.
    let document = match read_document(std::io::stdin(), max_document_bytes()) {
        Ok(document) => document,
        Err(e) => return usage_error(format!("reading the document from stdin: {e}")),
    };

    let ctx = context(&credentials.passphrase, &credentials.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), true) {
        Ok(f) => f,
        Err(e) => return usage_error(format!("opening {}: {e}", flags.path)),
    };
    let mut pad = match pad_source(&flags, &credentials.kappa, true) {
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
                    Some(n) =>
                        format!("source reports {n} sectors rewritten; not independently verified"),
                    None => "not run — the stand-in pad has no device to rewrite".to_string(),
                }
            );
            println!("  pad: {}", pad.describe());
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
    }
}

/// The maximum payload representable by the container, excluding per-slot authentication data.
fn max_document_bytes() -> usize {
    holonomy_core::layout::PAYLOAD_SLOTS * holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT
}

/// Read at most one byte beyond the format limit, so an unbounded pipe cannot exhaust memory.
fn read_document(input: impl std::io::Read, limit: usize) -> std::io::Result<Zeroizing<Vec<u8>>> {
    use std::io::Read;
    let mut document = Zeroizing::new(Vec::new());
    input.take(limit as u64 + 1).read_to_end(&mut document)?;
    if document.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("document exceeds the {limit}-byte container payload limit"),
        ));
    }
    Ok(document)
}

/// `holonomy open <path> --passphrase P --kappa HEX [--cursor N]`
fn open(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return usage_error(e),
    };
    let credentials = match credentials(&flags, false) {
        Ok(c) => c,
        Err(e) => return usage_error(e),
    };

    let ctx = context(&credentials.passphrase, &credentials.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), false) {
        Ok(f) => f,
        Err(e) => return usage_error(format!("opening {}: {e}", flags.path)),
    };
    let mut pad = match pad_source(&flags, &credentials.kappa, false) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    match store::open_document(&mut file, pad.as_mut(), &ctx) {
        Ok((opened, document)) => {
            eprintln!("pad: {}", pad.describe());
            eprintln!(
                "opened {}: stratum L_{}, {} slot(s), save cursor {}",
                flags.path,
                opened.stratum.id().index(),
                opened.directory.slot_count(),
                opened.cursor()
            );
            if let Err(e) = std::io::stdout().write_all(document.as_slice()) {
                return failure(format!("writing the document to stdout: {e}"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
    }
}

/// Authenticate a single candidate without writing the document to stdout.
fn verify(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(flags) if flags.cursor_explicit => flags,
        Ok(_) => return usage_error("verify requires an explicit --cursor"),
        Err(e) => return usage_error(e),
    };
    let credentials = match credentials(&flags, false) {
        Ok(c) => c,
        Err(e) => return usage_error(e),
    };
    let ctx = context(&credentials.passphrase, &credentials.kappa, flags.cursor);
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), false) {
        Ok(file) => file,
        Err(e) => return usage_error(format!("opening {}: {e}", flags.path)),
    };
    let mut pad = match pad_source(&flags, &credentials.kappa, false) {
        Ok(pad) => pad,
        Err(e) => return usage_error(e),
    };
    match store::verify_candidate(&mut file, pad.as_mut(), &ctx) {
        Ok(digest) => {
            println!(
                "Candidate authenticated at cursor {}. No document bytes were printed.",
                flags.cursor
            );
            println!(
                "image digest {} (unkeyed fingerprint of the bytes read; not a durability or authenticity proof)",
                digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            println!("Only the directory and declared payload are authenticated; unused image bytes are not checked.");
            println!("Pad source: {}", pad.describe());
            println!("This does not verify document format, pad freshness, durability, rollback history, or complete recovery.");
            ExitCode::SUCCESS
        }
        Err(e) => failure(e),
    }
}

/// `holonomy recover <path> [--all]`: find the cursor that opens a container, read-only.
///
/// A crash can leave a user holding an image and no record of which cursor wrote it. Guessing is
/// unsafe; `verify` only tests the cursor the caller already believes. This verb tries every cursor
/// a save may legally have written and reports the ones that authenticate. It writes nothing, does
/// not ratchet, and prints no document bytes.
/// Lowercase hex, so digests print the way `verify` prints them.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `holonomy recover <path> [--all] [--remnants]`: find the cursor an image needs, read-only.
///
/// The question a crash leaves behind is *which* cursor this image was folded with, and guessing it
/// is how a recovery turns into a second incident. So every cursor a save may legally have written
/// is tried, and the answer is a report. Nothing here writes: the container is opened read-only,
/// the image is only read, and the pad is never ratcheted -- a scan that repaired anything would be
/// making the decision the user has not made yet.
///
/// `--remnants` extends the same scan to the `.holonomy-save-*` copies a crashed save left behind,
/// because after a crash the newest image may be the one that never finished its rename. Testing
/// them is the whole claim: it does not rank them, rename one into place, or reconcile the pad.
fn recover(args: &[String]) -> ExitCode {
    let mut scan_all = false;
    let mut test_remnants = false;
    let mut remaining: Vec<String> = Vec::with_capacity(args.len());
    for arg in args {
        match arg.as_str() {
            "--all" => {
                if scan_all {
                    return usage_error("--all may only be specified once");
                }
                scan_all = true;
            }
            "--remnants" => {
                if test_remnants {
                    return usage_error("--remnants may only be specified once");
                }
                test_remnants = true;
            }
            _ => remaining.push(arg.clone()),
        }
    }
    let flags = match parse_flags(&remaining) {
        Ok(flags) => flags,
        Err(error) => return usage_error(error),
    };
    if flags.cursor_explicit {
        return usage_error(
            "recover tries the cursor range itself; use verify to test one cursor you already trust",
        );
    }
    let credentials = match credentials(&flags, false) {
        Ok(credentials) => credentials,
        Err(error) => return usage_error(error),
    };
    let mut file = match store::ContainerFile::open(std::path::Path::new(&flags.path), false) {
        Ok(file) => file,
        Err(error) => return usage_error(format!("opening {}: {error}", flags.path)),
    };
    let mut pad = match pad_source(&flags, &credentials.kappa, false) {
        Ok(pad) => pad,
        Err(error) => return usage_error(error),
    };
    let limit = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
    eprintln!(
        "Trying cursors 0 to {} against {}. Read-only: nothing is written and the pad is not ratcheted.",
        limit - 1,
        flags.path
    );
    eprintln!(
        "Each cursor costs one read of the image and one key derivation, so this can take a while."
    );

    let scan = store::scan_cursors(
        &mut file,
        pad.as_mut(),
        &context(&credentials.passphrase, &credentials.kappa, 0),
        !scan_all,
    );

    let opened = !scan.hits.is_empty();
    if opened {
        for hit in &scan.hits {
            println!(
                "Cursor {} authenticates. Unkeyed image digest {}",
                hit.cursor,
                hex(&hit.digest)
            );
        }
        if scan_all {
            println!(
                "Tried all {} cursors. More than one hit means this image, pad and credentials do not \
                 identify a single save; treat that as a fault, not a choice.",
                scan.attempts
            );
        } else {
            println!(
                "Stopped at the first cursor that opened after {} attempt(s); pass --all to test the \
                 whole range for aliasing.",
                scan.attempts
            );
        }
    } else {
        eprintln!(
            "No cursor from 0 to {} authenticates this image.",
            limit - 1
        );
        if let Some(error) = &scan.last_error {
            eprintln!("Last refusal: {error}");
        }
        eprintln!(
            "Check the passphrase, the root share, the device path, and whether the image is \
             damaged or truncated. This does not prove the container is unrecoverable; it proves \
             it did not open with these credentials in that range."
        );
    }

    let staged_opened = test_remnants
        && report_staged_copies(&flags.path, pad.as_mut(), &credentials, scan_all, limit);

    println!("Pad source: {}", pad.describe());
    println!(
        "Nothing was written or ratcheted. A hit proves the image opens at that cursor -- not \
         that it is fresh, undamaged, the newest copy, or the only copy."
    );
    if opened {
        return ExitCode::SUCCESS;
    }
    if staged_opened {
        eprintln!(
            "A staged copy opened while the container did not. That makes it a candidate for a \
             manual repair, not a repair: this command chose nothing and wrote nothing."
        );
    }
    ExitCode::FAILURE
}

/// Test the bounded, lexicographically ordered set of regular `.holonomy-save-*` files beside
/// `container` with the same credentials and report which cursors open each candidate.
///
/// Returns whether any copy opened, so the caller can say that a candidate exists without implying
/// the container is fixed. A copy that is not a whole image is reported as incomplete instead of
/// failing the scan, and one that cannot be opened is reported and skipped. Neither condition
/// establishes why the file is present or incomplete.
fn report_staged_copies(
    container: &str,
    pad: &mut dyn store::PadSource,
    credentials: &Credentials,
    scan_all: bool,
    limit: u64,
) -> bool {
    let container_path = std::path::Path::new(container);
    let (staged, truncated) = match store::find_save_remnants(container_path) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("Staged copies beside {container} could not be listed: {error}");
            return false;
        }
    };
    if staged.is_empty() {
        println!("No regular staged-name files beside {container}.");
        return false;
    }
    println!(
        "Testing {} matching file{} with the same credentials and pad window; nothing is ratcheted.",
        staged.len(),
        if staged.len() == 1 { "" } else { "s" }
    );
    let current_origin = staged
        .iter()
        .any(|copy| matches!(copy.origin, store::RemnantOrigin::Bound { .. }))
        .then(|| store::current_container_origin(container_path));
    let mut any_opened = false;
    for copy in &staged {
        println!(
            "Candidate file {} ({} bytes)",
            copy.path.display(),
            copy.len
        );
        if copy.len != CONTAINER_BYTES as u64 {
            println!(
                "  Not a whole image ({} of {} bytes); no cursor can authenticate it; the \
                 cause of this incomplete image is not established.",
                copy.len, CONTAINER_BYTES
            );
            continue;
        }
        let association = match copy.origin {
            store::RemnantOrigin::Bound { .. } => match current_origin.as_ref() {
                Some(Ok(origin)) => {
                    store::remnant_association_with_origin(container_path, &copy.path, Some(origin))
                }
                Some(Err(error)) => {
                    println!("  Skipped while checking origin: {error}");
                    continue;
                }
                None => {
                    println!("  Skipped: current container origin was not computed.");
                    continue;
                }
            },
            store::RemnantOrigin::LegacyUnbound | store::RemnantOrigin::MalformedBound => {
                store::remnant_association_with_origin(container_path, &copy.path, None)
            }
        };
        match association {
            Ok(store::RemnantAssociation::BoundToCurrentContainer) => println!(
                "  Origin marker matches current container device/inode and prior image digest (not a MAC)."
            ),
            Ok(store::RemnantAssociation::LegacyUnbound) => println!(
                "  Legacy unbound name: this may be from another container; authentication only tests it as a candidate."
            ),
            Ok(store::RemnantAssociation::DoesNotMatch) => {
                println!("  Skipped: origin marker does not match this container's current identity and bytes.");
                continue;
            }
            Ok(store::RemnantAssociation::MalformedBinding) => {
                println!("  Skipped: malformed new-format origin marker.");
                continue;
            }
            Err(error) => {
                println!("  Skipped while checking origin: {error}");
                continue;
            }
        }
        let open_result = store::open_save_remnant_beside(container_path, &copy.path);
        let mut file = match open_result {
            Ok(file) => file,
            Err(error) => {
                println!("  Skipped: {error}");
                continue;
            }
        };
        let scan = store::scan_cursors(
            &mut file,
            pad,
            &context(&credentials.passphrase, &credentials.kappa, 0),
            !scan_all,
        );
        if scan.hits.is_empty() {
            println!(
                "  No cursor from 0 to {} authenticates this copy.",
                limit - 1
            );
            continue;
        }
        for hit in &scan.hits {
            println!(
                "  Cursor {} authenticates. Unkeyed image digest {}",
                hit.cursor,
                hex(&hit.digest)
            );
        }
        any_opened = true;
    }
    if truncated {
        println!(
            "Only the first {} matching files by filename were tested; more exist.",
            staged.len()
        );
    }
    println!(
        "A matching file that opens is a candidate, not a choice: this does not say which copy is \
         newest, does not put one in place, and does not prove the copy is durable."
    );
    any_opened
}

/// Copy a verified staged image to a new path without replacing either source.
///
/// This is a limited recovery aid, not a repair: the explicit cursor must authenticate the staged
/// image, the destination must not exist, and neither the original container nor the staged image
/// is modified. Origin checks are not atomic with later reads and do not coordinate concurrent
/// writers. It does not reconcile or prove the pad's ratchet history.
fn recover_copy(args: &[String]) -> ExitCode {
    let mut remnant = None;
    let mut output = None;
    let mut allow_unbound = false;
    let mut remaining = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--remnant" | "--output" => {
                let flag = args[i].as_str();
                let Some(value) = args.get(i + 1).filter(|value| !value.starts_with("--")) else {
                    return usage_error(format!("{flag} needs one path"));
                };
                let slot = if flag == "--remnant" {
                    &mut remnant
                } else {
                    &mut output
                };
                if slot.replace(value.clone()).is_some() {
                    return usage_error(format!("{flag} may only be specified once"));
                }
                i += 2;
            }
            "--allow-unbound" => {
                if allow_unbound {
                    return usage_error("--allow-unbound may only be specified once");
                }
                allow_unbound = true;
                i += 1;
            }
            _ => {
                remaining.push(args[i].clone());
                i += 1;
            }
        }
    }

    let remnant = match remnant {
        Some(path) => std::path::PathBuf::from(path),
        None => return usage_error("recover-copy requires --remnant PATH"),
    };
    let output = match output {
        Some(path) => std::path::PathBuf::from(path),
        None => return usage_error("recover-copy requires --output NEW_PATH"),
    };
    let flags = match parse_flags(&remaining) {
        Ok(flags) => flags,
        Err(error) => return usage_error(error),
    };
    if !flags.cursor_explicit {
        return usage_error("recover-copy requires --cursor N from a successful recover scan");
    }
    let limit = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
    if flags.cursor >= limit {
        return usage_error(format!("--cursor must be between 0 and {}", limit - 1));
    }

    // Do these filesystem refusals before prompting or reading pad material. The final
    // persist_noclobber below closes the race with a destination created after this check.
    match std::fs::symlink_metadata(&output) {
        Ok(_) => {
            return failure(format!(
                "refusing to overwrite existing destination {}",
                output.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return failure(format!(
                "checking destination {}: {error}",
                output.display()
            ));
        }
    }
    // The diagnostic listing is intentionally capped. Open the explicitly selected path directly,
    // or the twenty-first real remnant could become impossible to copy just because it was omitted
    // from `recover --remnants`. On Unix, the helper uses O_NOFOLLOW and keeps the validated file
    // handle for the rest of recovery; other platforms still need equivalent no-follow support.
    let container_path = std::path::Path::new(&flags.path);
    match std::fs::symlink_metadata(container_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return failure(
                "recover-copy requires a regular container file, not a symlink or directory",
            )
        }
        Err(error) => return failure(format!("checking container path: {error}")),
    }
    match store::remnant_association(container_path, &remnant) {
        Ok(store::RemnantAssociation::BoundToCurrentContainer) => println!(
            "Origin binding matches the current container identity and prior bytes; it is not authenticated."
        ),
        Ok(store::RemnantAssociation::LegacyUnbound) if allow_unbound => eprintln!(
            "Warning: explicitly using a legacy remnant with no container-origin association."
        ),
        Ok(store::RemnantAssociation::LegacyUnbound) => {
            return failure(
                "legacy remnant is unbound to this container; inspect it and pass --allow-unbound to proceed",
            );
        }
        Ok(store::RemnantAssociation::DoesNotMatch) => {
            return failure(
                "staged image origin does not match this container's current identity and bytes",
            );
        }
        Ok(store::RemnantAssociation::MalformedBinding) => {
            return failure("staged image has a malformed new-format origin marker");
        }
        Err(error) => {
            return failure(format!("checking selected remnant origin: {error}"));
        }
    }
    let source_result = store::open_save_remnant_beside(container_path, &remnant);
    let mut source = match source_result {
        Ok(file) => file,
        Err(error) => {
            return failure(format!(
                "opening selected staged image {}: {error}",
                remnant.display()
            ));
        }
    };
    let credentials = match credentials(&flags, false) {
        Ok(credentials) => credentials,
        Err(error) => return usage_error(error),
    };
    let mut pad = match pad_source(&flags, &credentials.kappa, false) {
        Ok(pad) => pad,
        Err(error) => return usage_error(error),
    };
    let context = context(&credentials.passphrase, &credentials.kappa, flags.cursor);
    let digest = match store::verify_candidate(&mut source, pad.as_mut(), &context) {
        Ok(digest) => digest,
        Err(error) => {
            return failure(format!(
                "staged image did not authenticate at cursor {}: {error}",
                flags.cursor
            ));
        }
    };
    let image = match source.read_image() {
        Ok(image) => image,
        Err(error) => return failure(format!("reading the verified staged image again: {error}")),
    };
    if holonomy_session::seal::image_digest(&image) != digest {
        return failure("the staged image changed after verification; no copy was made");
    }

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut staged_output = match tempfile::Builder::new()
        .prefix(".holonomy-save-recovery-")
        .tempfile_in(parent)
    {
        Ok(file) => file,
        Err(error) => {
            return failure(format!(
                "staging recovered copy beside {}: {error}",
                output.display()
            ));
        }
    };
    if let Err(error) = staged_output.as_file_mut().write_all(&image) {
        return failure(format!("writing staged recovered copy: {error}"));
    }
    if let Err(error) = staged_output.as_file().sync_all() {
        return failure(format!("syncing staged recovered copy: {error}"));
    }
    let mut copied_file = match staged_output.persist_noclobber(&output) {
        Ok(file) => file,
        Err(error) => {
            return failure(format!(
                "installing recovered copy at {} without overwriting: {}",
                output.display(),
                error.error
            ));
        }
    };

    #[cfg(unix)]
    if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        return failure(format!(
            "recovered copy was created at {}, but syncing its directory failed; durability is uncertain: {error}",
            output.display()
        ));
    }

    let copied_digest = match store::file_digest(&mut copied_file) {
        Ok(digest) => digest,
        Err(error) => {
            return failure(format!(
                "recovered copy was created at {}, but its read-back failed; its contents are not verified: {error}",
                output.display()
            ));
        }
    };
    if copied_digest != digest {
        return failure(format!(
            "recovered copy was created at {}, but its read-back digest differs from the authenticated source; its contents are not verified",
            output.display()
        ));
    }
    drop(copied_file);

    println!(
        "Verified staged image copied and read back at {}",
        output.display()
    );
    println!("Authenticated cursor: {}", flags.cursor);
    println!("Unkeyed image digest: {}", hex(&digest));
    println!("Pad source: {}", pad.describe());
    println!(
        "Container and remnant contents were not written; filesystem access metadata may change. The pad was not written or ratcheted."
    );
    println!(
        "This copy does not establish which image is newest, pad freshness, ratchet history, or complete recovery."
    );
    ExitCode::SUCCESS
}

/// `holonomy remnants <path>`: report staged images a crashed save left behind.
///
/// Deliberately needs no credentials. A user who has just lost power to a machine should be able
/// to ask "what is lying around?" without first proving they can open the container, and the scan
/// reads a directory listing plus file sizes -- never a capsule, a pad or a document.
fn remnants(args: &[String]) -> ExitCode {
    let path = match args {
        [path] if !path.starts_with("--") => std::path::PathBuf::from(path),
        _ => return usage_error("remnants takes exactly one container path and no flags"),
    };
    let (found, truncated) = match store::find_save_remnants(&path) {
        Ok(found) => found,
        Err(error) => return failure(format!("scanning staged-image names: {error}")),
    };

    if found.is_empty() {
        println!(
            "No matching regular staged-image names next to {}.",
            path.display()
        );
        println!(
            "A crash leftover may use this prefix, but filenames alone cannot prove whether a save \
             created the file or why it remains."
        );
        return ExitCode::SUCCESS;
    }

    println!(
        "{} matching staged-image file{} next to {}:",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        path.display()
    );
    let current_origin = found
        .iter()
        .any(|remnant| matches!(remnant.origin, store::RemnantOrigin::Bound { .. }))
        .then(|| store::current_container_origin(&path));
    for remnant in &found {
        println!("  {}", remnant.path.display());
        println!("    {} bytes", remnant.len);
        let association = match remnant.origin {
            store::RemnantOrigin::Bound { .. } => match current_origin.as_ref() {
                Some(Ok(origin)) => Some(store::remnant_association_with_origin(
                    &path,
                    &remnant.path,
                    Some(origin),
                )),
                Some(Err(error)) => {
                    println!("    origin check unavailable: {error}");
                    None
                }
                None => {
                    println!("    current container origin was not computed");
                    None
                }
            },
            store::RemnantOrigin::LegacyUnbound | store::RemnantOrigin::MalformedBound => Some(
                store::remnant_association_with_origin(&path, &remnant.path, None),
            ),
        };
        if let Some(association) = association {
            match association {
                Ok(store::RemnantAssociation::BoundToCurrentContainer) => println!(
                    "    origin marker matches current container device/inode and prior image digest (not a MAC)"
                ),
                Ok(store::RemnantAssociation::LegacyUnbound) => {
                    println!("    legacy name: unbound to this container")
                }
                Ok(store::RemnantAssociation::DoesNotMatch) => {
                    println!("    origin marker does not match this container's current identity/bytes")
                }
                Ok(store::RemnantAssociation::MalformedBinding) => {
                    println!("    malformed new-format origin marker; do not treat as legacy")
                }
                Err(error) => println!("    origin check unavailable: {error}"),
            }
        }
        match store::path_digest(&remnant.path) {
            Ok(digest) => println!(
                "    unkeyed digest {} (fingerprint only; not authenticity or durability)",
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            Err(error) => println!("    could not read it for a digest: {error}"),
        }
        if remnant.len != CONTAINER_BYTES as u64 {
            println!(
                "    not a full container image: {} of {} bytes; cause of incompleteness is unknown",
                remnant.len, CONTAINER_BYTES
            );
        }
    }
    if truncated {
        println!(
            "Only the first {} are listed; the directory holds more matching names.",
            store::MAX_REMNANTS_REPORTED
        );
    }
    println!();
    println!("If a file is a genuine crash leftover from before rename, it is an encrypted staged");
    println!(
        "image and the container may still hold the previous save. The name alone cannot establish"
    );
    println!(
        "that history, and another writer or failure may break the expectation. Verify the container:"
    );
    println!();
    println!("    holonomy verify {} --cursor N", path.display());
    println!();
    println!("To test these files as candidates without being told which cursor each needs:");
    println!();
    println!("    holonomy recover {} --remnants", path.display());
    println!();
    println!(
        "That scan reports which cursor opens each file and writes nothing, but it does not rank"
    );
    println!(
        "the files or rename one into place. Candidate files may contain encrypted document bytes and occupy"
    );
    println!("full-container space; delete one only after deciding it is not the copy you need.");
    println!("Nothing above decrypts, repairs or ratchets anything.");
    ExitCode::SUCCESS
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
         \x20   holonomy provision <path> [--passphrase P] [--kappa HEX] [--cursor N] [--device D]\n\
         \x20   holonomy save <path> [--passphrase P] [--kappa HEX] [--cursor N] [--device D] < document\n\
         \x20   holonomy open <path> [--passphrase P] [--kappa HEX] [--cursor N] [--device D]\n\
         \x20   holonomy verify <path> --cursor N [--passphrase P] [--kappa HEX] [--device D]\n\
         \x20   holonomy remnants <path>  list regular files matching staged-image names (no credentials)\n\
         \x20   holonomy recover <path> [--all] [--remnants]  find the cursor that opens a container\n\
         \x20   holonomy recover-copy <path> --remnant R --cursor N --output NEW [--allow-unbound]  copy a candidate image\n\
         \n\
         Omit --passphrase and --kappa to enter them at a hidden terminal prompt (recommended).\n\
         Argument values may appear in process listings or shell history. The root share is 64 hex\n\
         characters; this build does not read it from Zone A yet. Passphrases are lowercased for\n\
         compatibility, so ASCII case variants are equivalent; Unicode normalization is not implemented.\n\
         \n\
         `--cursor` is the save counter that selects the pad window; it must be the number the\n\
         previous container recorded for open; for save use that cursor plus one.\n\
         Provision uses 0; saves refuse cursor 54 or higher until pad history is tracked.\n\
         Verify reads and authenticates a candidate without printing document bytes; it does not\n\
         repair the container or verify freshness, rollback history, or application document format.\n\
         Remnants lists `.holonomy-save-*` files next to a container and never decrypts them; it is a\n\
         directory report, not a recovery procedure.\n\
         Recover tries every cursor a save may have written (0 to 53) and reports the ones that open,\n\
         without writing or ratcheting. --remnants tests up to 20 regular matching files beside the\n\
         container in filename order. It is read-only diagnosis, not a repair: a hit does not make the\n\
         image fresh, undamaged, the newest copy, or the only copy, and nothing is renamed into place.\n\
         Recover-copy requires an explicit cursor and a regular `.holonomy-save-*` sibling. New\n\
         staged names include the original Unix device/inode and old image digest; this detects many\n\
         wrong-container or changed-container cases but is an unauthenticated, forgeable marker.\n\
         Legacy names are unbound and require explicit `--allow-unbound`. Authentication checks the\n\
         format's authenticated structures and declared payload, not every unused image byte. The\n\
         command writes a separate copy only if its destination is\n\
         absent. Source contents are not written, though access metadata may change; the pad is not\n\
         written. It does not choose the newest image, reconcile a ratchet, or complete recovery.\n\
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
    use super::{credentials_with_prompt, parse_flags, parse_kappa, read_document};

    #[test]
    fn prompted_credentials_are_confirmed_and_fail_closed() {
        let flags = parse_flags(&["book".to_string()]).expect("no credentials in arguments");
        let mut replies = vec![
            "secret".to_string(),
            "secret".to_string(),
            "ab".repeat(32),
            "AB".repeat(32),
        ]
        .into_iter();
        let credentials = credentials_with_prompt(&flags, true, |_| {
            replies
                .next()
                .ok_or_else(|| "unexpected prompt".to_string())
        })
        .expect("matching confirmation and valid share");
        assert_eq!(credentials.passphrase.as_bytes(), b"secret");
        assert_eq!(&*credentials.kappa, &[0xab; 32]);
        let mut wrong_share = vec![
            "secret".to_string(),
            "secret".to_string(),
            "ab".repeat(32),
            "cd".repeat(32),
        ]
        .into_iter();
        assert!(credentials_with_prompt(&flags, true, |_| wrong_share
            .next()
            .ok_or_else(|| "unexpected prompt".to_string()))
        .is_err());
        let mut mismatch = vec!["one".to_string(), "two".to_string()].into_iter();
        assert!(credentials_with_prompt(&flags, true, |_| {
            mismatch
                .next()
                .ok_or_else(|| "unexpected prompt".to_string())
        })
        .is_err());
        assert!(
            credentials_with_prompt(&flags, false, |_| Err("terminal unavailable".into())).is_err()
        );
    }

    #[test]
    fn explicit_credentials_do_not_prompt() {
        let flags = parse_flags(&[
            "book".into(),
            "--passphrase".into(),
            "Secret".into(),
            "--kappa".into(),
            "ab".repeat(32),
        ])
        .expect("arguments");
        let credentials =
            credentials_with_prompt(&flags, true, |_| Err("unexpected prompt".into()))
                .expect("explicit credentials");
        assert_eq!(credentials.passphrase.as_bytes(), b"secret");
    }

    #[test]
    fn input_is_bounded_and_exact_limit_is_accepted() {
        assert_eq!(read_document(&b"abcd"[..], 4).unwrap().as_slice(), b"abcd");
        assert!(read_document(&b"abcde"[..], 4).is_err());
        assert_eq!(read_document(&b""[..], 0).unwrap().as_slice(), b"");
    }

    #[test]
    fn flags_reject_missing_values_duplicates_and_extra_paths() {
        let args = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_flags(&args(&["book", "--passphrase", "--kappa"])).is_err());
        assert!(parse_flags(&args(&["book", "other"])).is_err());
        assert!(parse_flags(&args(&["book", "--cursor", "0", "--cursor", "1"])).is_err());
    }

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
