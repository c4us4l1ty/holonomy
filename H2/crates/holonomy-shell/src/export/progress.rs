//! Export progress: what the caller can observe while an export runs, and how to stop one.
//!
//! # Why progress is not a log line
//!
//! Because a 1.12M-word document spends **45 seconds** in Typst's layout and 9 in PDF
//! serialisation. That is not a slow operation, it is a long one, and a UI that shows nothing for
//! 54 seconds is indistinguishable from a UI that has hung. A log line is written once at the
//! end; the person watching the window needs to know the difference between "working" and
//! "stopped", and which of the four phases it is in is most of that difference.
//!
//! # Why these four phases and not a percentage
//!
//! Because the phases are known and their *durations are not*. Translating 1.33M words took 35ms;
//! layout took 45,000; serialisation took 9,361. A progress bar computed from a guess at the
//! ratios would be wrong by an order of magnitude in the first phase and roughly right in the
//! third, so it would read as stuck and then jump. Phase plus elapsed is the honest version: it
//! says "this is where it is" rather than pretending to know how much is left.
//!
//! A future version with a measured page count could do better, and the phase enum is the place
//! that would change — the UI would keep the same shape.
//!
//! # Why cancellation is checked between phases and not during one
//!
//! Because Typst's `compile` takes `&dyn World` and offers no way to interrupt it. A check
//! inside the layout loop would have to be a callback the compiler invokes between pages, which
//! `World` does not expose. So cancellation is *cooperative and coarse*: it takes effect at the
//! next phase boundary.
//!
//! Two consequences, both asserted in `tests/export-progress.rs` so neither is a surprise:
//!
//! - **A cancel during layout waits out the layout.** On a full-length document that is 45 of the
//!   54 seconds, so cancelling is useful for stopping a *second* export or a long serialisation
//!   and only partly useful for the layout itself.
//! - **A cancel during serialisation does nothing at all.** `Serializing` is the last boundary
//!   and is followed only by `Done`, so there is nowhere for a flag set then to take effect. The
//!   alternative -- checking once more after serialising -- was rejected deliberately: it would
//!   honour the click by discarding 69MB of written PDF after the user had already waited 54
//!   seconds, and handing them nothing. Finishing and giving them the file is the better failure.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Where an export has got to.
///
/// The order is the order phases run in, and the phase *before* a long one is the useful signal:
/// a UI that shows "typesetting 4,245 pages" for 45 seconds is telling the truth, and one that
/// shows a spinner is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts", rename_all = "camelCase")]
pub enum ExportPhase {
    /// Reading section JSON and turning it into Typst. Fast, and the only phase whose cost
    /// Holonomy controls.
    Translating,
    /// Reading the assets the document names out of SQLite. One pass, not one per figure.
    ReadingAssets,
    /// Typst parsing, evaluating and laying out pages. The long one, and not Holonomy's code.
    Layout,
    /// Turning laid-out pages into PDF bytes.
    Serializing,
    /// Finished.
    Done,
    /// Stopped at a phase boundary because the caller asked.
    Cancelled,
    /// Stopped by an error. The message says which.
    Failed,
}

impl ExportPhase {
    /// A human sentence, for a status line and for the log.
    ///
    /// Second person, present continuous, no percentages. "Typesetting 4,245 pages" is a
    /// statement about what is happening; "42%" would be a claim about a future this code cannot
    /// make yet.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Translating => "Translating the document into typesetting markup",
            Self::ReadingAssets => "Reading figures from the store",
            Self::Layout => "Typesetting pages",
            Self::Serializing => "Writing the PDF",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
        }
    }

    /// Whether this phase is the last one before the result is in hand.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled | Self::Failed)
    }
}

/// One progress report.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct ExportProgress {
    /// Which export this is about.
    ///
    /// Carried on every report because a user can start a second export while the first is still
    /// running -- the window is not modal over the document -- and a status line that showed the
    /// first job's phase during the second job's would be actively misleading.
    pub job_id: String,
    pub phase: ExportPhase,
    /// Milliseconds since the export started. A `number`, not a `bigint`: `ts-rs` emits `bigint`
    /// for a `u128` unless told otherwise, and a millisecond count is a `number`.
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    /// Anything worth saying beyond the phase. Empty for the common case.
    pub message: String,
}

/// Where progress reports go.
///
/// A trait rather than a closure so the test double is a named thing with recorded output, and so
/// the Tauri side can implement it with an event emitter without this module knowing what Tauri
/// is.
pub trait Progress: Send + Sync {
    fn report(&self, progress: ExportProgress);
}

/// A sink that does nothing.
///
/// Not an `Option` and not a no-op default inside the reporting function: a test that wants to
/// assert *no* progress happened still passes one of these and gets to say so.
pub struct Silent;

impl Progress for Silent {
    fn report(&self, _progress: ExportProgress) {}
}

/// The cancel flag, shared between the caller and the worker.
pub type Cancel = Arc<AtomicBool>;

/// A cancel flag nobody has set.
pub fn not_cancelled() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

/// Ask an export to stop at its next phase boundary.
///
/// Deliberately advisory and coarse — see this module's documentation. A caller that sets this
/// and then sees `Layout` reported for another 45 seconds has been told the truth by the phase
/// documentation rather than surprised.
pub fn cancel(flag: &Cancel) {
    flag.store(true, Ordering::SeqCst);
}

/// Whether cancellation has been asked for.
pub fn is_cancelled(flag: &Cancel) -> bool {
    flag.load(Ordering::SeqCst)
}

/// The reporting side an export holds: the sink, the job id, the clock, and the cancel flag.
///
/// Held as one value so every phase boundary is a single call and cannot forget one of the four
/// things. The first version threaded them separately and the cancel check ended up in only two
/// of the three boundaries, which is the kind of omission that only shows up as "sometimes it
/// ignores the cancel".
pub struct Reporter {
    job_id: String,
    started: std::time::Instant,
    sink: Box<dyn Progress>,
    cancel: Cancel,
}

impl Reporter {
    pub fn new(job_id: impl Into<String>, sink: Box<dyn Progress>, cancel: Cancel) -> Self {
        Self {
            job_id: job_id.into(),
            started: std::time::Instant::now(),
            sink,
            cancel,
        }
    }

    /// Report a phase, and say whether to stop.
    ///
    /// The order is cancel-then-report, so a cancelled export's *last* report is the phase it
    /// stopped at rather than the phase it would have entered. A user who cancels during
    /// serialisation should be told it stopped, not that it finished writing.
    ///
    /// Returns `false` when the export should stop.
    pub fn enter(&self, phase: ExportPhase) -> bool {
        if self.is_cancelled() {
            self.sink.report(ExportProgress {
                job_id: self.job_id.clone(),
                phase: ExportPhase::Cancelled,
                elapsed_ms: self.elapsed_ms(),
                message: format!("stopped before {}", phase.describe().to_lowercase()),
            });
            return false;
        }
        self.sink.report(ExportProgress {
            job_id: self.job_id.clone(),
            phase,
            elapsed_ms: self.elapsed_ms(),
            message: phase.describe().to_string(),
        });
        true
    }

    /// Report a phase with extra text, and say whether to stop.
    pub fn enter_with(&self, phase: ExportPhase, message: impl Into<String>) -> bool {
        if self.is_cancelled() {
            self.sink.report(ExportProgress {
                job_id: self.job_id.clone(),
                phase: ExportPhase::Cancelled,
                elapsed_ms: self.elapsed_ms(),
                message: format!("stopped before {}", phase.describe().to_lowercase()),
            });
            return false;
        }
        self.sink.report(ExportProgress {
            job_id: self.job_id.clone(),
            phase,
            elapsed_ms: self.elapsed_ms(),
            message: message.into(),
        });
        true
    }

    /// Report a terminal phase. Never consults the cancel flag: this *is* the ending.
    pub fn finish(&self, phase: ExportPhase, message: impl Into<String>) {
        self.sink.report(ExportProgress {
            job_id: self.job_id.clone(),
            phase,
            elapsed_ms: self.elapsed_ms(),
            message: message.into(),
        });
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// A sink that records, for tests.
///
/// Records what it was told rather than approximating it, so an assertion is about the sequence
/// the export actually produced.
///
/// `Clone`, and the clone shares the recording. That is what makes a test able to do two things
/// with one export -- hand a *recording* sink to the reporter and a *cancelling* sink that also
/// has to record -- without a shim type and a bespoke constructor on a type defined in another
/// crate, which is not a thing a test file can declare.
#[derive(Clone, Default)]
pub struct Recorder {
    reports: std::sync::Arc<std::sync::Mutex<Vec<(ExportPhase, String)>>>,
}

impl Recorder {
    /// The phases in order, which is the property most worth asserting.
    pub fn phases(&self) -> Vec<ExportPhase> {
        self.reports
            .lock()
            .expect("recorder mutex")
            .iter()
            .map(|(phase, _)| *phase)
            .collect()
    }

    /// Every message, for assertions about wording.
    pub fn messages(&self) -> Vec<String> {
        self.reports
            .lock()
            .expect("recorder mutex")
            .iter()
            .map(|(_, message)| message.clone())
            .collect()
    }
}

impl Progress for Recorder {
    fn report(&self, progress: ExportProgress) {
        self.reports
            .lock()
            .expect("recorder mutex")
            .push((progress.phase, progress.message));
    }
}

/// A short value that is unlikely to repeat within a process.
///
/// # Why the temp directory needs one
///
/// Two exports from two Holonomy instances -- or from the same instance in a test run --
/// would otherwise pick the same temp path and overwrite each other's payload mid-layout.
/// The pid alone is not enough: a recycled pid within one session would collide, and a test
/// run that forks would. The clock alone is not enough either, because `now_ms` has
/// millisecond resolution and two calls in the same millisecond are likely.
///
/// Time and pid together, hashed to a short string, so a directory name stays readable in
/// a file listing while still being unique in practice.
pub fn unique_suffix() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (holonomy_core::now_ms(), std::process::id()).hash(&mut h);
    format!("{:016x}", h.finish())
}
