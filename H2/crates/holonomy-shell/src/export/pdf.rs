//! Driving the compile, and reporting what it cost.
//!
//! # Why the timing is measured here and not in the frontend
//!
//! Because the question is "how long does the compile take", and the compile happens entirely
//! in this process. A round trip to the frontend would measure the bridge too, and the bridge's
//! cost is not what the directive asks about. Worse, an IPC measurement would include the
//! scheduling delay of a blocked command handler, so a slow export would look slower than it is
//! and the number would drift with machine load.
//!
//! # Why the budget is a hard number in a test
//!
//! 5 seconds for 667 sections. It is a `#[test]` rather than a log line because a performance
//! budget nobody checks is a comment. See [`PERF_BUDGET_MS`] and the `corpus_export_is_within_
//! budget` test.

use std::time::Instant;

use holonomy_core::error::Error;
use holonomy_core::Store;
use typst_layout::PagedDocument;

use super::progress::{ExportPhase, Reporter};
use super::translate::{self, TranslationReport};
use super::world::HoloWorld;

/// The budget for compiling a full-length document, in milliseconds.
///
/// # The arithmetic behind it
///
/// 667 sections is a 1.12M-word document at 1500 words per section — the ceiling the section
/// limits were measured against. Typst lays out roughly 2,000 words per second on one core for
/// body text, so 1.12M words is on the order of half a second of layout. Five seconds is
/// therefore about ten times the expected cost, which is the right ratio for a gate: loose
/// enough that a slower machine does not fail it, tight enough that an accidental
/// order-of-magnitude regression does.
///
/// # Why it is not tighter
///
/// Because the number is not the layout engine's, and the layout engine's cost is not the
/// variable being controlled. What is being controlled is *Holonomy's* share — the translation,
/// the asset reads, the page setup — and those are small and predictable. A tight budget would
/// fail on a slower CI machine for a reason that has nothing to do with the code.
pub const PERF_BUDGET_MS: u128 = 5_000;

/// What an export produced.
#[derive(Debug)]
pub struct ExportResult {
    /// The PDF bytes.
    pub pdf: Vec<u8>,
    /// Pages, which is the number a reader cares about and which the compile step never
    /// reports.
    pub pages: usize,
    /// Milliseconds spent inside this function.
    pub elapsed_ms: u128,
    /// Turning section JSON into Typst. Holonomy's share of the work.
    pub translate_ms: u128,
    /// Typst's parse, evaluate and layout. Not Holonomy's share.
    pub layout_ms: u128,
    /// Turning laid-out pages into PDF bytes.
    pub serialize_ms: u128,
    /// What the translator did.
    pub report: TranslationReport,
    /// Typst's warnings, as text.
    ///
    /// Kept rather than printed because a Typst warning is usually a *content* problem — a
    /// missing font variant, an unresolvable reference — and the caller needs to attach it to
    /// the document rather than to the terminal that ran the export.
    pub warnings: Vec<String>,
}

/// Everything the store was needed for, held in memory.
///
/// # Why this is a type rather than a comment
///
/// The `store: &Store` parameter on [`export_pdf`] made the whole "the store lock is not held
/// during the compile" design unenforceable. The function stops touching the store after
/// `preload_assets`, but a reference passed *into* it is a reference the **caller** holds for
/// the entire call — and the caller holds it through a `MutexGuard`. The doc comment on
/// `export_pdf`, on `HoloWorld` and on the Tauri command all said the lock was released before
/// the compile; all three were wrong, because `let store = ...lock()` is a binding that lives
/// to the end of the enclosing block, not to the end of the statement that uses it.
///
/// The effect was that a 45-second Typst compile held the document's `Mutex<DocumentCore>`
/// *and* the SQLite store lock, and every other bridge command — `get_section`,
/// `commit_section_edit`, and the height syncs from a document still being scrolled — blocked
/// behind it for the duration of an export. That is the bug this split exists to make
/// impossible rather than merely documented.
pub struct Prepared {
    // Public because `worker.rs` takes this apart to hand the world to a child process.
    // The alternative -- a `into_parts` method returning a five-tuple -- would put the same
    // information in a worse shape, and the fields are already documented here.
    pub world: HoloWorld,
    pub report: TranslationReport,
    pub section_count: usize,
    pub translate_ms: u128,
    /// Carried across the gap between [`prepare`] and [`compile`] so the reported total is the
    /// whole export rather than only the compile.
    pub started: Instant,
}

impl Prepared {
    /// Where an asset the document named could not be read.
    ///
    /// Reported rather than skipped, because an export that silently drops a figure is worse
    /// than one that refuses.
    pub fn report(&self) -> &TranslationReport {
        &self.report
    }
}

/// What [`prepare`] produced: the in-memory inputs, or a stop at a phase boundary.
pub enum PreparedOutcome {
    Prepared(Box<Prepared>),
    /// Stopped before the compile. `elapsed_ms` is what it had spent getting there.
    Cancelled { elapsed_ms: u64 },
}

/// Read the store, translate, and preload assets. Touches SQLite; touches nothing else.
///
/// # Why the result is `Box`ed
///
/// `HoloWorld` carries the whole translated source and every figure's bytes — tens of
/// megabytes for a long document. Boxing moves that to the heap so the `Cancelled` arm of the
/// enum stays two words, and a cancellation allocates nothing at all.
pub fn prepare(
    store: &Store,
    document_id: &str,
    title: &str,
    reporter: &Reporter,
) -> Result<PreparedOutcome, Error> {
    let started = Instant::now();

    // -- reading the document, and translating it ---------------------------------
    //
    // One phase for both, and that is a judgement rather than an omission: reading 667 sections
    // and turning them into Typst together took 35ms for 1.33M words, and splitting them would
    // produce two phase reports a user cannot tell apart.
    if !reporter.enter_with(ExportPhase::Translating, format!("translating \"{title}\"")) {
        return Ok(PreparedOutcome::Cancelled {
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let ids = store.section_ids(document_id)?;
    let mut sections = Vec::with_capacity(ids.len());
    for id in &ids {
        sections.push(store.load_section(id)?);
    }

    let translate_started = Instant::now();
    let (source, report) = translate::translate(&sections);
    let translate_ms = translate_started.elapsed().as_millis();
    // Dropped here. The section JSONs are the largest thing an export holds — a 1.12M-word
    // document is tens of megabytes — and holding them across the compile would double the peak
    // alongside Typst's own copy.
    drop(sections);

    // -- reading the assets --------------------------------------------------------
    if !reporter.enter_with(
        ExportPhase::ReadingAssets,
        format!("{} figure(s)", report.assets.len()),
    ) {
        return Ok(PreparedOutcome::Cancelled {
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let hashes: Vec<String> = report.assets.iter().cloned().collect();
    let mut world = HoloWorld::new(source)?;
    world.preload_assets(store, &hashes)?;
    // The store is not touched again, and — this is the point of the split — the caller can
    // drop its `&Store` before calling [`compile`]. Everything the remaining 54 seconds needs
    // is already in memory.

    Ok(PreparedOutcome::Prepared(Box::new(Prepared {
        world,
        report,
        section_count: ids.len(),
        translate_ms,
        started,
    })))
}

/// Lay out and serialise. Touches no store, and so can be called with nothing locked.
///
/// The phases it reports are the two that follow `prepare`'s, so a caller driving the two
/// halves separately sees the same phase sequence a caller using [`export_pdf`] does.
pub fn compile(
    prepared: Box<Prepared>,
    title: &str,
    reporter: &Reporter,
) -> Result<ExportOutcome, Error> {
    let Prepared {
        world,
        report,
        section_count,
        translate_ms,
        started,
    } = *prepared;

    // -- layout ---------------------------------------------------------------------
    if !reporter.enter_with(ExportPhase::Layout, format!("{section_count} section(s)")) {
        return Ok(ExportOutcome::Cancelled {
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    let layout_started = Instant::now();
    let warned = typst::compile::<PagedDocument>(&world);
    let layout_ms = layout_started.elapsed().as_millis();
    let warnings: Vec<String> = warned.warnings.iter().map(|w| format!("{w:?}")).collect();

    let document = warned.output.map_err(|errors| {
        Error::Other(anyhow::anyhow!(render_diagnostics(errors.as_slice(), title)))
    })?;

    // -- serialising ----------------------------------------------------------------
    //
    // Checked once more here, because it is the boundary a user is most likely to reach: nine
    // seconds of writing a 69MB file after a 45-second wait is the moment someone cancels, and a
    // check that stopped one boundary earlier would have saved none of it.
    let page_count = document.pages().len();
    if !reporter.enter_with(ExportPhase::Serializing, format!("{page_count} page(s)")) {
        return Ok(ExportOutcome::Cancelled {
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    let serialize_started = Instant::now();
    // `PdfOptions::default()` and nothing else, deliberately. Every field it has -- tagged PDF,
    // bookmarks, document metadata -- is a decision about what the exported file *is*, and
    // defaulting one of them is how an export silently becomes a different artefact than the
    // one before. When they are wanted they should be named, with a reason.
    let options = typst_pdf::PdfOptions::default();
    let pdf = typst_pdf::pdf(&document, &options).map_err(|errors| {
        Error::Other(anyhow::anyhow!(render_diagnostics(
            errors.as_slice(),
            "the serialised PDF",
        )))
    })?;

    let result = ExportResult {
        pdf,
        pages: page_count,
        elapsed_ms: started.elapsed().as_millis(),
        translate_ms,
        layout_ms,
        serialize_ms: serialize_started.elapsed().as_millis(),
        report,
        warnings,
    };
    reporter.finish(
        ExportPhase::Done,
        format!("{} page(s) in {} ms", result.pages, result.elapsed_ms),
    );
    Ok(ExportOutcome::Done(result))
}

/// [`prepare`] then [`compile`], for a caller that is not holding a contended lock.
///
/// # Who must not use this
///
/// Anything whose `&Store` comes out of a `Mutex`. Calling this means holding that reference
/// for the whole compile, which is the defect [`Prepared`] exists to prevent — so a caller
/// with a lock calls the two halves instead. Test suites and one-shot tools, which own their
/// store outright, want this.
pub fn export_pdf(
    store: &Store,
    document_id: &str,
    title: &str,
    reporter: &Reporter,
) -> Result<ExportOutcome, Error> {
    match prepare(store, document_id, title, reporter)? {
        PreparedOutcome::Cancelled { elapsed_ms } => Ok(ExportOutcome::Cancelled { elapsed_ms }),
        PreparedOutcome::Prepared(prepared) => compile(prepared, title, reporter),
    }
}

/// What an export produced, including the possibility that it was stopped.
///
/// A separate type rather than a flag on [`ExportResult`], because the two are not the same
/// thing: a cancelled export has no pages, no timings and no bytes, so every field of
/// `ExportResult` would need a "meaningless" value. Matching on the outcome says which.
#[derive(Debug)]
// `Done` is ~224 bytes because it carries the PDF. Boxed, the enum would be a pointer and the
// *cancelled* case -- which allocates nothing today -- would allocate to say so. The bytes are
// already on the heap; the struct around them is not what is expensive.
#[allow(clippy::large_enum_variant)]
pub enum ExportOutcome {
    Done(ExportResult),
    /// Stopped at a phase boundary. `elapsed_ms` is what it had spent getting there, which is
    /// what a UI shows next to "cancelled".
    Cancelled { elapsed_ms: u64 },
}

impl ExportOutcome {
    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Done(result) => result.elapsed_ms as u64,
            Self::Cancelled { elapsed_ms } => *elapsed_ms,
        }
    }
}

/// Export a document with no progress reporting and no way to stop it.
///
/// The shape every test in this crate uses, and the one a caller wants when it is measuring rather
/// than showing. Wrapping the general function rather than duplicating it: a second code path
/// that skips the phase boundaries is a second code path that can skip a cancel check.
pub fn export_pdf_quiet(
    store: &Store,
    document_id: &str,
    title: &str,
) -> Result<ExportResult, Error> {
    let reporter = Reporter::new(
        "quiet",
        Box::new(super::progress::Silent),
        super::progress::not_cancelled(),
    );
    match export_pdf(store, document_id, title, &reporter)? {
        ExportOutcome::Done(result) => Ok(result),
        ExportOutcome::Cancelled { elapsed_ms } => Err(Error::Other(anyhow::anyhow!(
            "an export with no cancel flag set reported itself cancelled after {elapsed_ms}ms, \
             which is a bug in the phase boundaries rather than a cancellation"
        ))),
    }
}

/// Render Typst's errors as something a person can act on.
///
/// # Why this is not `{:?}`
///
/// Because a Typst error is a span into a generated source string, and its useful half is the
/// message plus the line of generated Typst it points at. The `Debug` form gives an id and a
/// byte range into a file the user has never seen, which is a worse diagnostic than the
/// original — the user cannot look at `holonomy.typ` line 47, because there is no
/// `holonomy.typ`.
///
/// So the message is extracted, with the source line where Typst supplies one.
pub(crate) fn render_worker_diagnostics(
    errors: &[typst::diag::SourceDiagnostic],
    title: &str,
) -> String {
    render_diagnostics(errors, title)
}

fn render_diagnostics(errors: &[typst::diag::SourceDiagnostic], title: &str) -> String {
    let mut out = format!("could not typeset \"{title}\":\n");
    let mut lines: Vec<String> = Vec::new();
    for (i, diagnostic) in errors.iter().enumerate() {
        let message = diagnostic.message.to_string();
        // `DiagSpan` is a private type in this version of Typst, so a location cannot be
        // extracted from it. The message alone is reported, plus any hints -- which is still far
        // better than the `Debug` form, whose useful half is a byte range into a `holonomy.typ`
        // the user has never seen. A version of Typst that exposes the span would let this name
        // the line, and the diagnostic would be a great deal more useful; recorded in STATUS.md.
        let mut line = format!("  {i}: {message}");
        for hint in &diagnostic.hints {
            line.push_str(&format!("\n       hint: {}", hint.v));
        }
        lines.push(line);
    }
    out.push_str(&lines.join("\n"));
    out
}

/// The name Typst writes for a document's PDF metadata.
///
/// # Why the title is escaped twice over
///
/// It goes into generated Typst as a string literal, and it came from the database. A title of
/// `x"#show` would otherwise close the literal and start code, which is both a compile error and
/// a small injection surface — the document title is user data, like everything else in the
/// document.
pub fn title_literal(title: &str) -> String {
    format!("\"{}\"", title.replace('\\', "\\\\").replace('"', "\\\""))
}