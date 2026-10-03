//! Progress reporting and cancellation for a 54-second export.
//!
//! # Why this is a separate suite from `export.rs`
//!
//! Because the two hold different kinds of claim. `export.rs` holds that an export *produces a
//! correct PDF*; this holds that a caller can *watch it happen and stop it*. The second is what a
//! user touches during the 45 seconds that dominate their experience of the feature, and it is
//! the part with no other evidence: a phase report that never fires is invisible to a test that
//! only checks the PDF.
//!
//! # What is asserted, and what is not
//!
//! Asserted: the phases arrive in order, every boundary is checked for cancellation, a cancel
//! takes effect at the boundary, a cancelled export produces no PDF, and a cancel after the last
//! boundary changes nothing.
//!
//! Not asserted: how long anything takes. The one timing claim this project actually enforces
//! anywhere is `export.rs`'s `translate_ms` budget; wall-clock here would be a flaky test
//! pretending to be a measurement.

// # The store is not held across the compile, and this is where that is checked
//
// Every other test in this file calls `export_pdf`, which takes `&Store` for the whole call and
// so cannot distinguish "the compile needs the store" from "the caller kept the reference".
// `export_pdf`'s own documentation, `HoloWorld`'s documentation and the Tauri command's
// documentation all claimed the lock was released before layout. It was not: the command held
// a `MutexGuard` bound to a `let`, which lives to the end of the closure, so a 45-second compile
// blocked `get_section`, `commit_section_edit` and every height sync in the application.
//
// The fix was to split the function, and this test is what keeps the split honest: the store is
// *destroyed* between the two halves. If anything in the compile half reaches for a database —
// now, or after a refactor that merges the halves back together — this stops compiling, and the
// lock comes back with it.

use std::sync::Arc;

use holonomy_core::Store;
use holonomy_shell_lib::export::pdf::{self as pdf, export_pdf, ExportOutcome};
use holonomy_shell_lib::export::progress::{
    self, Cancel, ExportPhase, Progress, Recorder, Reporter,
};
use serde_json::json;

/// A document with enough in it that the layout phase is not instantaneous.
fn document(name: &str, paragraphs: usize) -> (Store, String) {
    let store = Store::open_in_memory().expect("store");
    let document = store.create_document(name).expect("document");
    let body = "The quick brown fox jumps over the lazy dog, and then does it again. ".repeat(10);
    let blocks: Vec<_> = (0..paragraphs)
        .map(|i| {
            json!({"type":"paragraph","content":[{"type":"text","text": format!("{i}. {body}")}]})
        })
        .collect();
    store
        .add_section(&document.id, &json!({"type":"doc","content": blocks}))
        .expect("section");
    (store, document.id)
}

/// A sink that cancels the export the moment it reaches a named phase.
struct CancelAt {
    recorder: Recorder,
    /// The phase that sets the flag when reported.
    at: ExportPhase,
    flag: Cancel,
}

impl Progress for CancelAt {
    fn report(&self, progress: progress::ExportProgress) {
        if progress.phase == self.at {
            progress::cancel(&self.flag);
        }
        self.recorder.report(progress);
    }
}

#[test]
fn the_phases_arrive_in_order_and_end_with_done() {
    let (store, id) = document("Phases", 30);
    let recorder = Recorder::default();
    let reporter = Reporter::new(
        "job-1",
        Box::new(recorder.clone()),
        progress::not_cancelled(),
    );

    let outcome = export_pdf(&store, &id, "Phases", &reporter).expect("export");
    let phases = recorder.phases();

    // The order is the assertion, not the presence. A UI renders "typesetting pages" when it sees
    // `Layout`, and if the phases arrived in any other order it would show the wrong one.
    assert_eq!(
        phases,
        vec![
            ExportPhase::Translating,
            ExportPhase::ReadingAssets,
            ExportPhase::Layout,
            ExportPhase::Serializing,
            ExportPhase::Done,
        ],
        "the phase sequence is what the status line renders"
    );
    assert!(matches!(outcome, ExportOutcome::Done(_)));
}

#[test]
fn elapsed_time_is_reported_and_never_goes_backwards() {
    let (store, id) = document("Elapsed", 30);
    let recorder = Recorder::default();
    let reporter = Reporter::new(
        "job-2",
        Box::new(recorder.clone()),
        progress::not_cancelled(),
    );
    export_pdf(&store, &id, "Elapsed", &reporter).expect("export");

    // `Recorder` keeps only phase and message, so the monotonicity check reads the reporter's
    // own clock through a second sink. Kept as its own test because the property is about the
    // clock rather than about the export.
    struct Times(Arc<std::sync::Mutex<Vec<u64>>>);
    impl Progress for Times {
        fn report(&self, progress: progress::ExportProgress) {
            self.0.lock().expect("times mutex").push(progress.elapsed_ms);
        }
    }
    let times = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reporter = Reporter::new(
        "job-3",
        Box::new(Times(times.clone())),
        progress::not_cancelled(),
    );
    export_pdf(&store, &id, "Elapsed", &reporter).expect("export");

    let seen = times.lock().expect("times mutex").clone();
    assert_eq!(seen.len(), 5, "one report per phase, including the terminal one");
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]),
        "elapsed_ms went backwards: {seen:?}. A progress bar that counts up then down reads as a \
         bug, and this is the kind of thing a numeric field invites"
    );
    assert!(seen.last().copied().unwrap_or(0) > 0, "the last report should carry a duration");
}

#[test]
fn a_cancel_stops_the_export_at_the_next_boundary() {
    // Every boundary *except the last*, and the exception is the point rather than an oversight.
    //
    // `Reporter::enter_with` checks the flag and then reports, so a cancel set by the report for
    // phase N can only take effect at phase N+1. `Serializing` is followed by `Done`, so a cancel
    // raised during serialisation has no boundary left to stop at and the export completes.
    //
    // Which of the two behaviours is right is worth stating, because the alternative was
    // available and worse: checking *after* serialising would mean honouring the click by
    // discarding 69MB of written PDF and handing the user nothing, after they had already waited
    // 54 seconds. Finishing the work and giving them the file is the better failure, so that is
    // what this does -- and this test is what makes it a decision rather than an oversight.
    for phase in [
        ExportPhase::Translating,
        ExportPhase::ReadingAssets,
        ExportPhase::Layout,
    ] {
        let (store, id) = document("Cancel", 20);
        let recorder = Recorder::default();
        let flag = progress::not_cancelled();
        let reporter = Reporter::new(
            format!("cancel-at-{phase:?}"),
            Box::new(CancelAt {
                recorder: recorder.clone(),
                at: phase,
                flag: flag.clone(),
            }),
            flag.clone(),
        );

        let outcome = export_pdf(&store, &id, "Cancel", &reporter).expect("export");
        assert!(
            matches!(outcome, ExportOutcome::Cancelled { .. }),
            "cancelling at {phase:?} should have stopped the export, but it produced a PDF"
        );

        // The last thing the user is told is that it stopped. A cancelled export whose final
        // report is `Layout` or `Serializing` reads as though it is still running.
        let phases = recorder.phases();
        assert_eq!(
            phases.last(),
            Some(&ExportPhase::Cancelled),
            "cancelling at {phase:?} ended with {phases:?}; the last report must be Cancelled"
        );
        assert!(
            !phases.contains(&ExportPhase::Done),
            "a cancelled export must not also report Done"
        );

        // And it must not have entered the phase *after* the one it was cancelled in.
        let entered: Vec<_> = phases
            .iter()
            .take_while(|p| **p != ExportPhase::Cancelled)
            .copied()
            .collect();
        let index = [
            ExportPhase::Translating,
            ExportPhase::ReadingAssets,
            ExportPhase::Layout,
        ]
        .iter()
        .position(|p| *p == phase)
        .expect("the phase is one of the three cancellable ones");
        assert_eq!(
            entered.len(),
            index + 1,
            "cancelling at {phase:?} should have entered exactly up to that phase, not further: \
             {entered:?}"
        );
    }
}

#[test]
fn a_cancel_during_serialisation_finishes_rather_than_discarding_the_work() {
    // The last boundary has no boundary after it, so the export completes. Stated as its own test
    // rather than folded into the loop above, because it is the one case where the user's click
    // does nothing and the reason has to be findable.
    let (store, id) = document("Late serialising", 20);
    let recorder = Recorder::default();
    let flag = progress::not_cancelled();
    let reporter = Reporter::new(
        "cancel-serialising",
        Box::new(CancelAt {
            recorder: recorder.clone(),
            at: ExportPhase::Serializing,
            flag: flag.clone(),
        }),
        flag.clone(),
    );

    let outcome = export_pdf(&store, &id, "Late serialising", &reporter).expect("export");

    match &outcome {
        ExportOutcome::Done(result) => {
            assert!(result.pdf.starts_with(b"%PDF"), "the completed export should be a PDF");
            assert!(
                result.pages > 0,
                "finishing the serialisation should still produce pages"
            );
        }
        ExportOutcome::Cancelled { .. } => {
            panic!("serialisation is the last phase; there is no boundary for a cancel to stop at")
        }
    }
    assert_eq!(
        recorder.phases().last(),
        Some(&ExportPhase::Done),
        "so the last report is Done, and the user is not left watching a modal that never closes"
    );
}

#[test]
fn a_cancel_after_the_last_boundary_is_harmless() {
    // The UI can offer a cancel button while a 69MB file is being written, and the file can
    // finish before the click lands. Cancelling then must not turn a completed export into a
    // failure -- it is the ordinary race of a fast operation and a slow click.
    let (store, id) = document("Late cancel", 5);
    let flag = progress::not_cancelled();
    let recorder = Recorder::default();
    let reporter = Reporter::new("late", Box::new(recorder.clone()), flag.clone());
    let outcome = export_pdf(&store, &id, "Late cancel", &reporter).expect("export");
    assert!(matches!(outcome, ExportOutcome::Done(_)));

    // Set after the fact, as a click that landed too late would.
    progress::cancel(&flag);
    assert!(progress::is_cancelled(&flag), "the flag should be set");

    // A *second* export with the same flag is cancelled, which is the correct reading: the flag
    // belongs to the job, and reusing it across jobs is what the job registry in `lib.rs`
    // prevents by giving each export its own.
    let recorder2 = Recorder::default();
    let reporter2 = Reporter::new("late-2", Box::new(recorder2.clone()), flag);
    let outcome2 = export_pdf(&store, &id, "Late cancel", &reporter2).expect("export");
    assert!(
        matches!(outcome2, ExportOutcome::Cancelled { .. }),
        "a flag set before an export starts should stop it immediately"
    );
    assert_eq!(recorder2.phases(), vec![ExportPhase::Cancelled]);
}

#[test]
fn a_job_that_was_never_cancelled_reports_done_last() {
    // The property the other two depend on: `Cancelled` is only ever reported because the flag
    // was set. If a bug reported it unconditionally, the first test in this file would still pass
    // (it asserts the sequence of a *successful* export) and only the cancel test would notice.
    let (store, id) = document("Uncancelled", 5);
    let recorder = Recorder::default();
    let reporter = Reporter::new(
        "clean",
        Box::new(recorder.clone()),
        progress::not_cancelled(),
    );
    let outcome = export_pdf(&store, &id, "Uncancelled", &reporter).expect("export");
    assert!(matches!(outcome, ExportOutcome::Done(_)));
    assert!(
        !recorder.phases().contains(&ExportPhase::Cancelled),
        "nothing asked this export to stop"
    );
    assert!(!recorder.phases().contains(&ExportPhase::Failed));
}

#[test]
fn every_phase_has_a_sentence_a_person_can_read() {
    // The status line renders `describe()`, so a phase with an empty or placeholder string is a
    // blank status line. Asserted for all of them, including the terminal ones, because those
    // are the ones a reader sees longest.
    for phase in [
        ExportPhase::Translating,
        ExportPhase::ReadingAssets,
        ExportPhase::Layout,
        ExportPhase::Serializing,
        ExportPhase::Done,
        ExportPhase::Cancelled,
        ExportPhase::Failed,
    ] {
        let text = phase.describe();
        assert!(!text.is_empty(), "{phase:?} has no description");
        assert!(
            text.chars().next().is_some_and(|c| c.is_uppercase()),
            "{phase:?} describes itself as {text:?}, which is not a sentence"
        );
        assert!(
            text.chars().any(|c| c.is_ascii_alphabetic()),
            "{phase:?} describes itself as {text:?}"
        );
    }
    assert!(!ExportPhase::Layout.is_terminal());
    assert!(ExportPhase::Done.is_terminal());
    assert!(ExportPhase::Cancelled.is_terminal());
    assert!(ExportPhase::Failed.is_terminal());
}

#[test]
fn the_job_id_reaches_every_report() {
    // A UI showing two exports' progress at once correlates them by this. A report with a
    // different id would be attributed to the wrong job, which is worse than not reporting.
    struct Ids(Arc<std::sync::Mutex<Vec<String>>>);
    impl Progress for Ids {
        fn report(&self, progress: progress::ExportProgress) {
            self.0.lock().expect("ids mutex").push(progress.job_id);
        }
    }
    let (store, id) = document("Ids", 5);
    let ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reporter = Reporter::new("export-7", Box::new(Ids(ids.clone())), progress::not_cancelled());
    export_pdf(&store, &id, "Ids", &reporter).expect("export");

    let seen = ids.lock().expect("ids mutex").clone();
    assert_eq!(seen.len(), 5);
    assert!(
        seen.iter().all(|id| id == "export-7"),
        "every report should carry the job id; got {seen:?}"
    );
}


#[test]
fn the_compile_half_runs_with_the_store_destroyed() {
    // A figure, so the compile half genuinely needs bytes that came out of the database. A text
    // document would pass this test trivially and prove nothing about the asset path.
    let png: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
        0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
        0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
        0x42, 0x60, 0x82,
    ];
    let mime = "image/png";

    let (store, id) = document("No lock", 30);
    let hash = store.put_asset(&png, mime).expect("asset");
    store
        .add_section(
            &id,
            &json!({"type":"doc","content":[
                {"type":"paragraph","content":[{"type":"text","text":"before"}]},
                {"type":"image","attrs":{"src": format!("holo-asset://{hash}")}},
            ]}),
        )
        .expect("figure section");

    let reporter = Reporter::new(
        "split",
        Box::new(Recorder::default()),
        progress::not_cancelled(),
    );

    // Phase one, exactly as the Tauri command runs it: with the store locked.
    let prepared = {
        let locked = std::sync::Mutex::new(store);
        let guard = locked.lock().expect("store");
        match pdf::prepare(&guard, &id, "No lock", &reporter).expect("prepare") {
            pdf::PreparedOutcome::Prepared(prepared) => prepared,
            pdf::PreparedOutcome::Cancelled { elapsed_ms } => {
                panic!("an uncancelled export reported cancelled after {elapsed_ms}ms")
            }
        }
        // `guard` and the `Mutex` holding the store are dropped here.
    };

    // The database is gone. Not closed, not unlocked — gone. Whatever the compile half needs,
    // it is holding a copy of.
    let outcome = pdf::compile(prepared, "No lock", &reporter).expect("compile with no store");

    match outcome {
        ExportOutcome::Done(result) => {
            assert!(result.pages >= 1, "an empty document should still have a page");
            assert!(
                result.pdf.starts_with(b"%PDF"),
                "the compile half produced {} bytes that are not a PDF",
                result.pdf.len()
            );
        }
        ExportOutcome::Cancelled { elapsed_ms } => {
            panic!("an uncancelled export reported cancelled after {elapsed_ms}ms")
        }
    }
}
