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

#[test]
fn live_reaper_arms_document_keys_and_residency_and_reports_honestly_in_panel() {
    use std::sync::Arc;
    use std::time::Duration;

    use holonomy_gui::SealWipeBridge;
    use holonomy_seal::{CanarySet, ReaperGuard, SecretArenas, TripReason, Tripwire};

    let path = temp_path();
    let document =
        Document::create_with(&path.0, PASSPHRASE, store::test_params()).expect("creates");

    let arenas = SecretArenas::new();
    let mut canary_set = CanarySet::new(4, 128);
    assert_eq!(canary_set.arm_all(&arenas), 4);
    let canaries = Arc::new(canary_set);

    assert_eq!(document.arm_reaper(&arenas), 2);
    assert!(!document.are_keys_zeroized());

    let mut residency = ResidentSet::new(Pages(64), 64 * 1024, 64).expect("budget");
    residency.set_wipe_registry(Arc::new(SealWipeBridge(Arc::clone(&arenas))));
    let _ = residency.fault_in(SegmentId::new(0)).expect("faults");

    let tripwire = Tripwire::new(None);
    let guard = ReaperGuard::spawn(
        Arc::clone(&arenas),
        Arc::clone(&canaries),
        Arc::clone(&tripwire),
        Duration::from_millis(25),
    );

    let armed_facts = Facts::gather(&residency, &document, None).with_reaper(
        &arenas,
        &canaries,
        tripwire.evaluate().is_some(),
    );
    assert!(armed_facts.reaper_line().contains("reaper: armed"));
    assert!(armed_facts.reaper_line().contains("4/4 canaries live"));
    // 4 canaries + 2 document keys + 1 resident slab = 7 armed arenas.
    assert_eq!(armed_facts.armed_arenas, 7);

    tripwire.fire(TripReason::Manual);
    let outcome = guard.wait_outcome().expect("reaper completed");
    assert_eq!(outcome.canaries.ghost_bytes, 0);
    assert!(document.are_keys_zeroized());
    assert!(residency
        .fault_in(SegmentId::new(0))
        .expect("still resident")
        .iter()
        .all(|&b| b == 0));

    let tripped_facts = Facts::gather(&residency, &document, None).with_reaper(
        &arenas,
        &canaries,
        tripwire.evaluate().is_some(),
    );
    assert!(tripped_facts.reaper_line().contains("TRIPPED"));
    assert!(tripped_facts
        .reaper_line()
        .contains("4/4 canaries zeroized"));
}

#[test]
fn document_persists_and_restores_ribbon_formatting_through_app() {
    use holonomy_eye::Fixed;
    use holonomy_gui::term::CellShaper;
    use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers};
    use holonomy_shell::Editor;
    use holonomy_text::Typing;

    let path = temp_path();
    let doc = Document::create_with(&path.0, PASSPHRASE, store::test_params()).expect("creates");
    let res = ResidentSet::new(doc.source(), store::BUDGET, store::SLACK).expect("budget");
    let editor = Editor::new(
        Typing::resume(doc.sink(), doc.spine().unwrap(), store::CAPACITY),
        res,
        CellShaper,
        Fixed::units(80),
        Fixed::units(24),
    );
    let mut app = App::new(
        editor,
        Keymap::new(Fixed::units(24)),
        CellSurface::new(80, 24, 1),
    );
    for ch in "Hello Ribbon".chars() {
        app.press(Key::Character(ch), Modifiers::none()).unwrap();
    }
    app.press(Key::Character('a'), Modifiers::control())
        .unwrap();
    app.press(Key::Character('b'), Modifiers::control())
        .unwrap();
    app.press(Key::Character('1'), Modifiers::control())
        .unwrap();
    app.press(Key::Character('s'), Modifiers::control())
        .unwrap();
    assert_eq!(app.toolbar_summary(), "Sans H1 [B]");
    drop(app);
    drop(doc);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens");
    reopened.verify().expect("authenticates v2 styled manifest");
    let res2 = ResidentSet::new(reopened.source(), store::BUDGET, store::SLACK).expect("budget");
    let editor2 = Editor::new(
        Typing::resume(reopened.sink(), reopened.spine().unwrap(), store::CAPACITY),
        res2,
        CellShaper,
        Fixed::units(80),
        Fixed::units(24),
    );
    let mut app2 = App::new(
        editor2,
        Keymap::new(Fixed::units(24)),
        CellSurface::new(80, 24, 1),
    );
    app2.press(Key::Character('a'), Modifiers::control())
        .unwrap();
    assert_eq!(app2.toolbar_summary(), "Sans H1 [B]");
}
