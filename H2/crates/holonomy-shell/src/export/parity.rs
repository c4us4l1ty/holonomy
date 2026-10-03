//! Cross-platform pagination parity: the fingerprint.
//!
//! # What this is for
//!
//! M6 bundled four font families so that "pagination is identical across Linux, macOS and
//! Windows" stopped being a hope and became a claim that could be *checked*. This module is
//! how.
//!
//! The claim has two halves and they are not equally hard. "The same font bytes are used" is
//! a fact about the binary, provable by inspection. "The same pages come out" is a fact
//! about Typst's line breaking, glyph advances and page-breaking arithmetic on three
//! different operating systems with three different libm implementations — and it cannot be
//! shown on one machine.
//!
//! # Why a fingerprint and not the PDF bytes
//!
//! Because the PDF bytes are not comparable, and comparing them would be the most plausible
//! mistake available here. `typst_pdf::pdf` writes a `/CreationDate` into the trailer, so two
//! runs a second apart produce different bytes from identical layout. There is also no
//! guarantee about object ordering or compression. A byte comparison would fail on whichever
//! runner happened to start a second later than another, and would then be "fixed" by
//! hashing away the header — at which point it is no longer testing the thing it claimed to.
//!
//! So the fingerprint is taken from the *laid-out document*, before any of that: the frames,
//! their items, and where each item sits. That is the object pagination actually is.
//!
//! # Why the coordinates are computed the way `typst-pdf` computes them
//!
//! Because an independent derivation would be an independent implementation, and the whole
//! claim is that the two agree. `handle_group` in `typst-pdf/src/convert.rs` accumulates
//! `acc = group.transform.pre_concat(acc)` and maps a point through the result. This module
//! does the identical thing, so the positions it hashes are the positions the PDF writer
//! writes. Read `typst-pdf`'s `convert.rs` before changing the arithmetic here.
//!
//! # Why positions are quantised
//!
//! Because raw `f64` positions are not expected to be bit-identical across architectures,
//! and a comparison that demands bit-identity will fail on a real difference in the fourth
//! decimal place while passing on a line broken in the wrong place. The quantum is 1/1000 of
//! a point — about 0.35 nanometres — far below anything a reader or a printer resolves and
//! far above the last-bit noise that differently-implemented transcendental functions
//! produce. A line breaking one word early moves a run by a line height: four orders of
//! magnitude above the quantum.

use typst::layout::{Frame, FrameItem, Point, Transform};
use typst_layout::PagedDocument;

/// Positions are rounded to a multiple of this, in points.
///
/// 0.001pt. Chosen by argument rather than by measurement across platforms, because there is
/// no second platform here to measure against — which is the whole reason this module exists.
/// `quantum_is_far_below_any_real_difference` holds the comparison that matters.
pub const QUANTUM: f64 = 0.001;

/// The bytes hashed to make a digest.
///
/// Serialised as 64 hex characters rather than as serde's default array of 32 numbers. The
/// artifacts these fingerprints end up in are read by people debugging a failed cross-platform
/// run, and `[104, 101, 108, ...]` is not read by people. An earlier version had *both* forms
/// in the same file; a test that changed the readable one then failed with "expected an array
/// of length 32" rather than with the page difference it was standing in for, which is exactly
/// what two representations of one fact cost.
pub type Digest = [u8; 32];

/// (De)serialise a [`Digest`] as hex.
mod hex_digest {
    use super::Digest;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(digest: &Digest, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::hex(digest))
    }

    /// Deserialise a list of digests, each one hex.
    ///
    /// Separate from the scalar case because `#[serde(with = ...)]` substitutes *both*
    /// directions of a field, and a `Vec` field needs a sequence, not a string.
    pub mod seq {
        use super::Digest;
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            digests: &[Digest],
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(digests.len()))?;
            for d in digests {
                seq.serialize_element(&super::super::hex(d))?;
            }
            seq.end()
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Vec<Digest>, D::Error> {
            Vec::<String>::deserialize(deserializer)?
                .into_iter()
                .map(|text| {
                    let mut out = [0u8; 32];
                    if text.len() != 64 {
                        return Err(serde::de::Error::custom(format!(
                            "a digest is 64 hex characters, this one is {}: {text:?}",
                            text.len()
                        )));
                    }
                    for (i, byte) in out.iter_mut().enumerate() {
                        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                            .map_err(serde::de::Error::custom)?;
                    }
                    Ok(out)
                })
                .collect()
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Digest, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "a digest is 64 hex characters, this one is {}: {text:?}",
                text.len()
            )));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}

/// One glyph's contribution, so shaping differences are caught.
///
/// # Why glyph ids and not just the text
///
/// Because the text is the same by construction — it is the same source — while the *glyphs*
/// are where shaping happens. A runner with a different font, or the same font shaped by a
/// different version, produces different glyph ids for the same characters, and that is a
/// fidelity failure a text-only fingerprint would report as success.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GlyphRun {
    pub ids: Vec<u16>,
    /// Sum of the glyph advances, quantised like everything else.
    pub advance: i64,
}

/// One text run, positioned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextRunFingerprint {
    /// Font family, as Typst reports it.
    pub font: String,
    /// Size in points, quantised.
    pub size: i64,
    /// Position within the page, in points, quantised.
    pub x: i64,
    pub y: i64,
    pub width: i64,
    /// The text, hashed.
    ///
    /// Hashed rather than stored because a fifty-page fixture's text is most of the
    /// document, and the useful comparison is "same text", not "which text".
    pub text_digest: String,
    pub glyphs: GlyphRun,
}

/// What one page contributes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageFingerprint {
    /// Laid-out items on the page, groups included.
    ///
    /// Separately recorded because it is the cheapest single indicator of a pagination
    /// difference, and because a page whose item count matches while its positions differ is
    /// a *different* failure from one whose count does not: the first is a font-metric
    /// difference, the second a line-breaking one. A single digest cannot tell them apart,
    /// and whoever debugs a failed run will want to.
    pub items: usize,
    pub text_runs: usize,
    pub runs: Vec<TextRunFingerprint>,
    /// How far down the page the last item reaches, quantised.
    pub lowest: i64,
}

/// The whole document's fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaginationFingerprint {
    pub pages: usize,
    /// SHA-256 over the per-page digests, in order.
    ///
    /// **Computed, not stored.** The first version built this from the pages and
    /// `page_digests` from the pages, by two separate traversals, and every comparison used
    /// `page_digests` — so nothing ever compared `digest`, and a mutation that made it cover
    /// only the first page passed all fourteen tests. Two encodings of one fact, only one of
    /// them tested, is exactly the arrangement a redundant field produces.
    ///
    /// So it is derived from `page_digests` on demand, and there is now only one place the
    /// document's identity is computed. It remains because a single number is what a CI log
    /// line wants to show three runners agreeing on.
    #[serde(with = "hex_digest")]
    pub digest: Digest,
    /// Per-page digests, so a failure can be localised to the page that diverged.
    ///
    /// Without this, a mismatch in one paragraph among fifty pages produces one number that
    /// differs and no way to find where. With it, the check prints the first page whose
    /// digest disagrees and the difference is one screen instead of a bisect.
    #[serde(with = "hex_digest::seq")]
    pub page_digests: Vec<Digest>,
    /// Typst version, which is part of the claim: a different Typst paginates differently,
    /// and a mismatch between runners on different versions is a version difference rather
    /// than a platform one. Recording it is what lets the two be told apart.
    pub typst_version: String,
    /// The bundled font families this fingerprint was produced with.
    ///
    /// Same reasoning. If two runners disagree about their fonts, that *is* the finding, and
    /// comparing digests without recording the fonts would present a font difference as a
    /// mysterious platform one.
    pub fonts: Vec<String>,
    /// The Preamble version the translator emitted.
    ///
    /// Also part of the claim: a translator change moves every position on every page, and
    /// without this a translator regression reads as a platform difference.
    pub preamble: String,
}

impl PaginationFingerprint {
    /// A one-line description for a CI log, or for a failure message.
    pub fn summary(&self) -> String {
        format!(
            "{} pages, digest {}, typst {}, preamble {}, fonts [{}]",
            self.pages,
            hex(&self.digest[..8]),
            self.typst_version,
            self.preamble,
            self.fonts.join(", ")
        )
    }

    /// Compare against another, naming the first thing that disagrees.
    ///
    /// # Why this is a function and not `==`
    ///
    /// Because `PartialEq` on two fingerprints that differ says only *that* they differ, and
    /// the first question anyone asks is *where*. Answering it needs the per-page digests,
    /// which are in the struct but not in the comparison.
    pub fn first_divergence(&self, other: &PaginationFingerprint) -> Option<Divergence> {
        // Self-consistency before anything cross-runner. `digest` is derived from
        // `page_digests`, so a mismatch means the artefact is corrupt rather than different,
        // and every answer computed from it — including "these agree" — would be unfounded.
        // Checked first for that reason; see `Divergence::Malformed`.
        if self.digest != digest_of(&self.page_digests) {
            return Some(Divergence::Malformed {
                label: "left".to_string(),
            });
        }
        if other.digest != digest_of(&other.page_digests) {
            return Some(Divergence::Malformed {
                label: "right".to_string(),
            });
        }
        // Versions and fonts before content. A runner on a different Typst is expected to
        // disagree, and reporting "page 0 is laid out differently" for that would send
        // whoever reads it looking for a font problem they do not have.
        if self.typst_version != other.typst_version {
            return Some(Divergence::TypstVersion {
                left: self.typst_version.clone(),
                right: other.typst_version.clone(),
            });
        }
        if self.fonts != other.fonts {
            return Some(Divergence::Fonts {
                left: self.fonts.join(", "),
                right: other.fonts.join(", "),
            });
        }
        if self.preamble != other.preamble {
            return Some(Divergence::Preamble {
                left: self.preamble.clone(),
                right: other.preamble.clone(),
            });
        }
        if self.pages != other.pages {
            return Some(Divergence::PageCount {
                left: self.pages,
                right: other.pages,
            });
        }
        for (i, (a, b)) in self.page_digests.iter().zip(&other.page_digests).enumerate() {
            if a != b {
                return Some(Divergence::PageContent { page: i });
            }
        }
        None
    }

    /// Check one runner's fingerprint against a reference, returning a message or `Ok`.
    ///
    /// The shape CI uses, so the thing the three-way comparison depends on is a function
    /// with tests rather than shell arithmetic.
    ///
    /// # Why the reference is labelled too
    ///
    /// Because the first version named only the failing side, and the test asserting the
    /// message names both is what noticed. "macos does not match the reference" followed by
    /// two unlabelled summaries is a message that makes the reader guess which line is the
    /// reference — which is the first thing they need to know, and the one thing a diff of
    /// three runners makes ambiguous.
    pub fn check_against(
        &self,
        reference: &PaginationFingerprint,
        ref_label: &str,
        label: &str,
    ) -> Result<(), String> {
        match self.first_divergence(reference) {
            None => Ok(()),
            // `first_divergence` cannot know the runner names — it is handed two fingerprints
            // and nothing else — so a self-inconsistency comes back labelled `left`/`right`.
            // Substituted here, because the one message that must not be vague is the one
            // saying a runner's artefact is corrupt: "left's digest" sends the reader looking
            // for a difference between runners when there is none.
            Some(Divergence::Malformed { .. }) => {
                let bad = if self.digest != digest_of(&self.page_digests) {
                    label
                } else {
                    ref_label
                };
                Err(format!(
                    "{bad}'s fingerprint is corrupt: its summary digest does not match its \
                     own page digests, so it cannot be compared against anything. Re-emit it \
                     on that runner.\n  {bad}: {}",
                    if bad == label { self.summary() } else { reference.summary() },
                ))
            }
            Some(d) => Err(format!(
                "{label} does not match the reference ({ref_label})\n  \
                 reference {ref_label}: {}\n  this {label}: {}\n  reason: {d}",
                reference.summary(),
                self.summary(),
            )),
        }
    }
}

/// Where two fingerprints stop agreeing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    TypstVersion { left: String, right: String },
    Fonts { left: String, right: String },
    Preamble { left: String, right: String },
    PageCount { left: usize, right: usize },
    PageContent { page: usize },
    /// A fingerprint whose own summary digest does not match its own page digests.
    ///
    /// # Why this is not the same as a platform difference
    ///
    /// The summary line and the page digests are redundant by construction — `digest` is
    /// `digest_of(&page_digests)` — so a runner cannot normally emit a pair that disagrees.
    /// But the file on disk is an artefact, artefacts get truncated, hand-edited or carried
    /// between runners, and `check_against` is handed whatever it finds.
    ///
    /// Without this check that shows up as the most confusing possible CI output: a failure
    /// whose two summary lines print the *same* digest, because the stored `digest` field is
    /// stale while the pages genuinely differ. The reader concludes the comparison is broken,
    /// or that parity held. Observed while verifying this harness by hand-editing one page
    /// digest, which is exactly how a corrupted artefact would present.
    ///
    /// Reported before any cross-runner comparison, because a self-inconsistent fingerprint
    /// makes every other answer from it untrustworthy — including "they agree".
    Malformed { label: String },
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Divergence::TypstVersion { left, right } => {
                write!(f, "different Typst versions: {left} against {right}")
            }
            Divergence::Fonts { left, right } => {
                write!(f, "different bundled fonts: [{left}] against [{right}]")
            }
            Divergence::Preamble { left, right } => {
                write!(f, "different preambles: {left} against {right}")
            }
            Divergence::PageCount { left, right } => {
                write!(f, "page count differs: {left} against {right}")
            }
            Divergence::PageContent { page } => {
                write!(f, "page {page} is laid out differently")
            }
            Divergence::Malformed { label } => write!(
                f,
                "{label}'s summary digest does not match its own page digests, so the \
                 fingerprint is corrupt and cannot be compared"
            ),
        }
    }
}

/// Quantise a point value to an integer.
///
/// Rounded half-away-from-zero so the mapping is symmetric about zero: `x` and `-x` get
/// indices of equal magnitude and opposite sign. "Half up" would give `-0.0005` and `+0.0005`
/// the same index, which is a mapping that folds the two halves of the page onto each other.
///
/// A non-finite value gets a distinct sentinel rather than being clamped. Two documents that
/// both produce NaN then agree, and one that produces NaN where another produces a number
/// does not — which is the honest outcome for what would be a Typst bug.
pub fn quantise(value: f64) -> i64 {
    if value.is_nan() {
        return i64::MIN;
    }
    if value == f64::INFINITY {
        return i64::MAX;
    }
    if value == f64::NEG_INFINITY {
        return i64::MIN + 1;
    }
    (value / QUANTUM).round() as i64
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The summary digest for a set of page digests.
///
/// Public so that a *caller* building a synthetic fingerprint — a test, or a migration — can
/// keep the summary consistent with the pages it just changed. Without it, the only way to
/// make a fingerprint with fewer pages is to produce one from a shorter document, and the
/// shortcut of popping a page and leaving `digest` alone now trips `Divergence::Malformed`.
/// That is the check working, not an obstacle: the fixture should be well-formed, because
/// the thing under test is a *comparison*, not a corrupt-input handler.
///
/// Length-prefixed by construction, since every element is a fixed 32 bytes, so pages `[ab,
/// c]` and `[a, bc]` cannot hash alike — the failure mode a bare concatenation would have.
pub fn digest_of(page_digests: &[Digest]) -> Digest {
    let mut input = Vec::with_capacity(page_digests.len() * 32);
    for d in page_digests {
        input.extend_from_slice(d);
    }
    sha256(&input)
}

/// Hash bytes with SHA-256.
fn sha256(bytes: &[u8]) -> Digest {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Fingerprint a laid-out document.
///
/// # Why the traversal order is trusted rather than sorted
///
/// Because the digest depends on the order items are visited, and that order *is* the
/// layout. Sorting the traversal would make the fingerprint insensitive to emission order,
/// which is a change worth noticing. Typst's order is stable for a given input — that
/// stability is precisely the property under test — so it is used as given.
pub fn fingerprint(
    document: &PagedDocument,
    fonts: Vec<String>,
    preamble: &str,
) -> PaginationFingerprint {
    let pages: Vec<PageFingerprint> = document
        .pages()
        .iter()
        .map(|page| {
            let mut page_out = PageFingerprint {
                items: 0,
                text_runs: 0,
                runs: Vec::new(),
                lowest: 0,
            };
            let mut lowest = f64::NEG_INFINITY;
            walk(&page.frame, Point::zero(), Transform::identity(), &mut page_out, &mut lowest);
            page_out.lowest = if lowest.is_finite() { quantise(lowest) } else { 0 };
            page_out
        })
        .collect();

    let page_digests: Vec<Digest> = pages.iter().map(page_digest).collect();

    PaginationFingerprint {
        pages: pages.len(),
        // Derived from the one list that everything else compares, so it cannot drift from
        // it. See the field's note.
        digest: digest_of(&page_digests),
        page_digests,
        typst_version: typst_version(),
        fonts,
        preamble: preamble.to_string(),
    }
}

fn page_digest(page: &PageFingerprint) -> Digest {
    let mut input = Vec::new();
    input.extend_from_slice(&page.items.to_le_bytes());
    input.extend_from_slice(&page.text_runs.to_le_bytes());
    input.extend_from_slice(&page.lowest.to_le_bytes());
    for run in &page.runs {
        input.extend_from_slice(run.font.as_bytes());
        input.push(0);
        input.extend_from_slice(&run.size.to_le_bytes());
        input.extend_from_slice(&run.x.to_le_bytes());
        input.extend_from_slice(&run.y.to_le_bytes());
        input.extend_from_slice(&run.width.to_le_bytes());
        input.extend_from_slice(run.text_digest.as_bytes());
        input.push(0);
        input.extend_from_slice(&run.glyphs.advance.to_le_bytes());
        for id in &run.glyphs.ids {
            input.extend_from_slice(&id.to_le_bytes());
        }
    }
    sha256(&input)
}

/// Walk one frame, accumulating an absolute offset.
///
/// # Why the transform is carried rather than ignored
///
/// Because a group with a non-identity transform puts its children somewhere other than
/// where their raw coordinates say. Ignoring it produces a fingerprint that is stable across
/// platforms and wrong.
///
/// # Why the offset is read out of the transform rather than summed separately
///
/// Because this is copied from `typst-pdf`'s `convert.rs`, which is the authority on where
/// things end up:
///
/// ```ignore
/// for (point, item) in frame.items() {
///     fc.push();
///     fc.state_mut().pre_concat(Transform::translate(point.x, point.y));
///     match item {
///         FrameItem::Group(g) => handle_group(..),   // which pre_concats g.transform
/// ```
///
/// So the offset is whatever `acc.tx`/`acc.ty` say after the same sequence of `pre_concat`
/// calls, and the absolute point is the frame origin plus that. Summing `position` values by
/// hand would be a second implementation of an arithmetic rule that already exists, and the
/// two would agree until a group applied a scale — at which point the hand-summed version
/// would be confidently wrong.
fn walk(
    frame: &Frame,
    origin: Point,
    acc: Transform,
    out: &mut PageFingerprint,
    lowest: &mut f64,
) {
    for (position, item) in frame.items() {
        let moved = Transform::translate(position.x, position.y).pre_concat(acc);
        let at = Point::new(origin.x + moved.tx, origin.y + moved.ty);
        // In points, once. Converting per use would be three chances to forget one and
        // would leave the values as absolute lengths until the last moment.
        let (ax, ay) = (at.x.to_pt(), at.y.to_pt());
        match item {
            FrameItem::Group(group) => {
                out.items += 1;
                // A group has no size of its own to record, so a transform is observable
                // only through the positions of what is inside it. An empty group therefore
                // contributes nothing — correct, since an empty box draws nothing.
                walk(
                    &group.frame,
                    origin,
                    group.transform.pre_concat(moved),
                    out,
                    lowest,
                );
            }
            FrameItem::Text(text) => {
                out.items += 1;
                out.text_runs += 1;
                let width = text.width();
                *lowest = lowest.max(ay);
                out.runs.push(TextRunFingerprint {
                    // `FontInstance` is a newtype; the family is on the `Font` inside it
                    // and the size is on the metrics. Reaching for `text.font.info()` (which
                    // is how a `Font` behaves) does not compile, and the error says so.
                    font: text.font.font().info().family.clone(),
                    size: quantise(text.size.to_pt()),
                    x: quantise(ax),
                    y: quantise(ay),
                    width: quantise(width.to_pt()),
                    text_digest: hex(&sha256(text.text.as_bytes())),
                    glyphs: GlyphRun {
                        ids: text.glyphs.iter().map(|g| g.id).collect(),
                        // An advance is in ems and means nothing without the size it is
                        // relative to: summing them raw would report `4.2` for a run whose
                        // rendered width is 10.5pt. `text.width()` is the same quantity
                        // computed by Typst, and is recorded separately as `width` so the
                        // two can be compared against each other.
                        advance: quantise(
                            text.glyphs
                                .iter()
                                .map(|g| g.x_advance.at(text.size).to_pt())
                                .sum::<f64>()
                                * text.size.to_pt(),
                        ),
                    },
                });
            }
            FrameItem::Image(_, size, _) => {
                out.items += 1;
                *lowest = lowest.max(ay);
                // An image contributes its size and nothing else. That is the case that
                // catches a missing-asset difference: the page count can match while the
                // content does not.
                out.runs.push(TextRunFingerprint {
                    font: "#image".to_string(),
                    size: quantise(size.y.to_pt()),
                    x: quantise(ax),
                    y: quantise(ay),
                    width: quantise(size.x.to_pt()),
                    text_digest: String::new(),
                    glyphs: GlyphRun { ids: Vec::new(), advance: 0 },
                });
            }
            FrameItem::Link(_, size) => {
                out.items += 1;
                *lowest = lowest.max(ay);
                out.runs.push(TextRunFingerprint {
                    font: "#link".to_string(),
                    size: quantise(size.y.to_pt()),
                    x: quantise(ax),
                    y: quantise(ay),
                    width: quantise(size.x.to_pt()),
                    text_digest: String::new(),
                    glyphs: GlyphRun { ids: Vec::new(), advance: 0 },
                });
            }
            FrameItem::Shape(shape, _) => {
                out.items += 1;
                // A shape's own size is the meaningful part; recording the position too
                // means a rule that moved by one point is noticed.
                // `Geometry` is a tree of paths and has no `bounding_rect`; the size comes
                // from `Shape::bbox`, which is what `typst-pdf` asks the same question of.
                // The stroke is excluded because a hairline's width is a rendering detail,
                // and including it would make the fingerprint sensitive to it.
                let rect = shape.bbox(false);
                *lowest = lowest.max(ay);
                out.runs.push(TextRunFingerprint {
                    font: "#shape".to_string(),
                    size: quantise(rect.size().y.to_pt()),
                    x: quantise(ax),
                    y: quantise(ay),
                    width: quantise(rect.size().x.to_pt()),
                    text_digest: String::new(),
                    glyphs: GlyphRun { ids: Vec::new(), advance: 0 },
                });
            }
            // Tags carry no geometry. They are layout-introspection markers, and a document
            // that emitted different ones has not changed its pagination.
            FrameItem::Tag(_) => {}
        }
    }
}

/// The Typst version this build links, for the record.
///
/// The workspace pins `typst = "0.15"`, so this is Holonomy's own version string and not
/// Typst's — which is *why* it is recorded. Two runners built from the same Holonomy commit
/// agree here; two from different commits do not, and a translator change then shows up as a
/// version difference instead of a mysterious content difference.
fn typst_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Per-page digests, shortened, for a failure report.
pub fn page_table(fingerprint: &PaginationFingerprint) -> Vec<(usize, String)> {
    fingerprint
        .page_digests
        .iter()
        .enumerate()
        .map(|(i, d)| (i, hex(&d[..6])))
        .collect()
}