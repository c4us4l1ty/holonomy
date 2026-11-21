//! The security panel, held to the things it describes — `docs/TO-APP.md` M6.
//!
//! A panel is worth having only if every number in it comes from the live state. So these tests do
//! what the milestone asks: they assemble [`Facts`] from a real resident set, a real world-line and
//! a real `mlock` probe, then check that each rendered line agrees with the field it came from — and
//! that changing the field changes the line. A hard-coded claim fails here.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use holonomy_gui::panel::{human, Facts};
use holonomy_gui::store::{self, Document};
use holonomy_seal::{generate_container_id, probe_capability, LockCapability};
use holonomy_text::{ResidentSet, SegmentId, SegmentSource};

const PASSPHRASE: &[u8] = b"the panel's passphrase";

/// A document path that is deleted when the test ends.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_path() -> Temp {
    let id = generate_container_id().expect("entropy");
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Temp(std::env::temp_dir().join(format!("holonomy-panel-{suffix}.wavefunction")))
}

/// A source that serves one fixed segment length, for the residency half of the facts.
#[derive(Debug, Clone, Copy)]
struct Pages(usize);

impl SegmentSource for Pages {
    fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
        Ok(vec![b'a'; self.0])
    }
}

#[test]
fn the_panel_reports_what_the_resident_set_holds() {
    let path = temp_path();
    let document =
        Document::create_with(&path.0, PASSPHRASE, store::test_params()).expect("creates");
    let mut residency = ResidentSet::new(Pages(10), 64 * 1024, 256).expect("a budget");

    let empty = Facts::gather(&residency, &document, None);
    assert_eq!(empty.resident_bytes, 0);
    assert!(
        empty.resident_line().contains("0 B"),
        "{}",
        empty.resident_line()
    );

    residency.fault_in(SegmentId::new(0)).expect("faults in");
    let held = Facts::gather(&residency, &document, None);
    assert_eq!(held.resident_bytes, residency.resident_bytes());
    assert_eq!(held.resident_segments, residency.resident_segments());
    assert_eq!(held.budget_bytes, residency.budget_bytes());
    assert_eq!(held.allocated_bytes, residency.allocated_bytes());
    assert!(
        held.resident_line().contains("10 B") && held.resident_line().contains("1 leaf"),
        "{}",
        held.resident_line()
    );
    assert_ne!(empty.resident_line(), held.resident_line());
    println!("HOLONOMY_PANEL resident={}", held.resident_line());
}

#[test]
fn the_panel_reports_the_files_own_numbers() {
    let path = temp_path();
    let document =
        Document::create_with(&path.0, PASSPHRASE, store::test_params()).expect("creates");
    let residency = ResidentSet::new(Pages(8), 64 * 1024, 0).expect("a budget");

    let fresh = Facts::gather(&residency, &document, None);
    assert_eq!(fresh.segments, document.segment_count());
    assert_eq!(fresh.witnesses, document.witness_count());
    assert!(
        fresh.worldline_line().contains("0 segments"),
        "{}",
        fresh.worldline_line()
    );
    assert!(
        fresh.worldline_line().contains("not verified"),
        "{}",
        fresh.worldline_line()
    );
    assert!(fresh.recovery_line().contains("nothing"));

    // A real authentication, and the panel must say what it verified — not that it verified.
    let report = document.verify().expect("the file authenticates");
    let verified = Facts::gather(&residency, &document, Some(report.segments_verified));
    assert_eq!(
        verified.verified_segments,
        Some(0),
        "genesis has no segment bodies"
    );
    assert!(
        verified.worldline_line().contains("verified 0"),
        "{}",
        verified.worldline_line()
    );
    assert_ne!(fresh.worldline_line(), verified.worldline_line());
    println!("HOLONOMY_PANEL worldline={}", verified.worldline_line());
}

#[test]
fn the_lock_claim_is_the_rungs_own_and_never_a_constant() {
    // The probe is real: whatever this machine achieves is what the panel will show.
    let probed = probe_capability();
    let path = temp_path();
    let document =
        Document::create_with(&path.0, PASSPHRASE, store::test_params()).expect("creates");
    let residency = ResidentSet::new(Pages(8), 64 * 1024, 0).expect("a budget");
    let facts = Facts::gather(&residency, &document, None);
    assert_eq!(facts.capability, probed, "the panel must probe, not assume");
    assert_eq!(facts.lock_line(), format!("lock: {}", probed.claim()));

    // Every rung renders its own claim, and no two rungs render alike: a hard-coded string would
    // fail one of these.
    let rungs = [
        LockCapability::None,
        LockCapability::Resident,
        LockCapability::ResidentNoDump,
    ];
    let mut seen = Vec::new();
    for rung in rungs {
        let mut facts = facts;
        facts.capability = rung;
        let line = facts.lock_line();
        assert!(line.contains(rung.claim()), "{line}");
        assert!(
            !seen.contains(&line),
            "two rungs rendered the same claim: {line}"
        );
        seen.push(line);
    }
    assert!(
        !facts.mechanism_line().is_empty(),
        "the panel must say how protection is attempted"
    );
    println!("HOLONOMY_PANEL lock={}", facts.lock_line());
}

#[test]
fn the_panel_never_claims_more_than_its_bytes() {
    // Formatting is exact, so a number that would need rounding says so instead of lying.
    assert_eq!(human(0), "0 B");
    assert_eq!(human(1023), "1023 B");
    assert_eq!(human(1024), "1 KiB");
    assert_eq!(
        human(1024 * 1024 - 1),
        "1023.9 KiB",
        "no rounding up into a lie"
    );
}
