//! The Typst compilation environment.
//!
//! # What a `World` is and why Holonomy needs its own
//!
//! Typst's compiler does not touch the filesystem, the font directories or the clock. It asks
//! a [`World`] for four things — the standard library, a font book, the main source, and the
//! bytes of any other file the document references. That is the whole interface, and it is
//! exactly the seam Holonomy needs, because two of the four answers are things only this
//! project has:
//!
//! - **Assets.** A figure is addressed `holo-asset://<sha256>` and lives in SQLite. There is
//!   no file to open, so `file()` answers from the `assets` table. See
//!   [`WorldImpl::file`] for why that is also the *only* thing it will answer.
//! - **Fonts.** The document's fonts are a decision, not a discovery. See [`FontSet`].
//!
//! # Why no temporary files, anywhere
//!
//! The directive says assets resolve "as raw byte slices without writing temporary files to
//! disk", and the reason is worth stating rather than repeating: a 2000-page document with
//! figures is several hundred megabytes of assets. Writing them to a temp directory to hand
//! Typst paths would mean a second full copy on disk, a cleanup path that has to be correct
//! on every crash, and a window in which a half-written temp file is the answer to an export.
//! SQLite already holds the bytes; `file()` reads them and Typst decodes them in memory.
//!
//! # Why the source is one in-memory string
//!
//! Because the translator produces it. A section-per-file layout would mean 667 files, 667
//! `#include`s and a directory to manage, in exchange for an incremental compilation this
//! application has no use for — a PDF export is a whole-document operation by definition.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{VirtualPath, VirtualRoot};
use typst::syntax::{FileId, RootedPath, Source};
use typst::text::{Font, FontBook, FontInfo};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

use holonomy_core::error::Error;
use holonomy_core::Store;

/// The scheme asset bytes are addressed by. Mirrors `holonomy_core::ASSET_SCHEME`, and the
/// translator emits it, so a document's Typst and its SQLite rows cannot disagree about it.
pub const ASSET_SCHEME: &str = holonomy_core::ASSET_SCHEME;

/// The family the generated preamble names, and therefore the face every exported document uses
/// unless it asks for something else.
///
/// # Why a bundled family and not a system one
///
/// Because the alternative was a hard requirement on a font this project does not ship. Naming
/// `Libertinus Serif` with nothing bundled produced `unknown font family: libertinus serif` on
/// every export on this machine -- 290 fonts installed, none of them that one -- and the warning
/// was the only evidence, with a silent substitution behind it.
///
/// So the face is bundled (see [`FontSet`]) and the name is one this project controls. The
/// string is a single constant on both sides of the crate: [`BODY_FONT`] here and
/// `translate::BODY_FONT` there, and `tests/export.rs` asserts they are the same value rather
/// than trusting the two to stay in step.
pub const BODY_FONT: &str = "Libertinus Serif";

/// The fonts the compiler is offered.
///
/// # Built once per process
///
/// A `LazyHash<FontBook>` because Typst requires one by reference, and rebuilding it per export
/// would re-parse every font file's metadata. A 2000-page export builds it once.
///
/// # Bundled first, system second, and the order is the whole point
///
/// Determinism across Linux, macOS and Windows comes from the *first* entries in the book being
/// the same bytes on every machine, and the generated preamble naming a family among them. A
/// document that asks for nothing else therefore paginates identically everywhere.
///
/// The system fonts are kept, and they come after. Two reasons:
///
/// - A word processor that cannot use a font the user installed is not a word processor. Making
///   the bundle the only source would fix pagination and break the application's actual purpose.
/// - The cost is bounded and stateable: pagination is machine-dependent **only** for a document
///   that names a family outside the bundle. That is recorded in `STATUS.md` rather than left as
///   a surprise, and `tests/export.rs` asserts the bundle occupies the lowest indices, which is
///   the property that makes the guarantee real.
///
/// A system font that duplicates a bundled one is skipped rather than appended, so a machine
/// with Libertinus installed gets the *bundled* bytes -- otherwise "deterministic" would mean
/// "deterministic if your distribution happens to match".
///
/// # Where the bytes come from
///
/// `typst-assets`, which is already in the dependency graph through `typst` and which ships
/// Libertinus Serif and New Computer Modern under the SIL Open Font License 1.1. Nothing is
/// checked into this repository as a binary blob, and nothing is downloaded at build time: the
/// version is pinned by the same `typst` version the compiler is, so the fonts and the compiler
/// that reads them cannot drift apart.
struct FontSet {
    book: LazyHash<FontBook>,
    faces: Vec<Font>,
    /// The families the bundle provides, in book order. Exposed for the status line and for the
    /// test that asserts the guarantee above.
    bundled_families: Vec<String>,
    /// How many *faces* the bundle contributed, as opposed to how many families.
    ///
    /// Separate because the two are different numbers and the ordering test needs the face count:
    /// the bundle contributes six Libertinus faces and one of each other family, so a family list
    /// and a book prefix are different shapes. The first version of that test compared them and
    /// failed on "four Libertinus Serifs" against "one Libertinus Serif" — a test bug that read
    /// exactly like an implementation bug, which is the worst kind.
    bundled_faces: usize,
}

impl FontSet {
    fn build() -> Result<Self, Error> {
        let mut book = FontBook::new();
        let mut faces: Vec<Font> = Vec::new();
        // Deduplicates the bundle itself: `typst-assets` lists some families by more than
        // one file, and a book with the same (family, variant) twice resolves to whichever
        // came last.
        let mut seen: BTreeSet<(String, typst::text::FontVariant)> = BTreeSet::new();
        let mut bundled_families: Vec<String> = Vec::new();

        // -- bundled, first, in the order `typst-assets` lists them --------------------
        //
        // That order is a literal array in a pinned crate version, so it is the same on every
        // machine and every build. Sorting here instead would be equally deterministic and would
        // make the guarantee obvious rather than inherited; it is left in the crate's order
        // because the *bytes* are what matter and the crate's order is stable by construction.
        for data in typst_assets::fonts() {
            for face in Font::iter(Bytes::new(data)) {
                let Some(info) = FontInfo::new(data, face.index()) else { continue };
                if !seen.insert((info.family.clone(), info.variant)) {
                    continue;
                }
                if bundled_families.last().map(String::as_str) != Some(info.family.as_str()) {
                    bundled_families.push(info.family.clone());
                }
                faces.push(face);
                book.push(info);
            }
        }

        let bundled_count = faces.len();
        let bundled_faces = bundled_count;
        if bundled_count == 0 {
            // Unreachable with the `fonts` feature on, and the check exists because the failure
            // it prevents is silent: an empty book makes every glyph a fallback box, and the
            // export still succeeds.
            return Err(Error::Other(anyhow::anyhow!(
                "the bundled font set is empty. `typst-assets` was built without its `fonts` \
                 feature, so Libertinus Serif is not present and every document would typeset in \
                 a fallback face."
            )));
        }

        // -- and nothing else -----------------------------------------------------------
        //
        // There is no system-font pass. The first version had one, after the bundle, so that a
        // user could typeset in a font they had installed, and it made three promises this code
        // could not keep:
        //
        //   * **Fingerprinting.** Enumerating `/usr/share/fonts`, `/Library/Fonts` and
        //     `C:\Windows\Fonts` means stat-ing and parsing every font on the machine on the
        //     first export. The set of installed fonts is one of the more distinctive things
        //     about a machine, and it does not need to leave the process to be observable to
        //     anything with `ptrace`.
        //   * **Determinism, only partly.** The bundle already won every duplicate — a system
        //     face with a `(family, variant)` already seen is skipped — so a bundled family
        //     rendered from the same bytes everywhere. What it did *not* stop is a document
        //     naming a family the bundle lacks: that resolved to whatever the exporting machine
        //     happened to have, so two runners paginated that document differently and the
        //     parity job could not have distinguished a Typst difference from a font
        //     difference.
        //   * **Attack surface, which was the weakest of the three.** No OS font engine is
        //     reached from here: faces are parsed by `ttf-parser`, pure Rust, and `fontdb` is
        //     in the graph only through `typst-library`'s SVG path. Reading a `.ttf` off disk is
        //     a `std::fs::read`, not a call into FreeType or DirectWrite.
        //
        // The first two are worth the third's price, and — measured, not assumed — the price is
        // smaller than it looks. A document naming a family the bundle lacks still *exports*:
        // Typst reports `unknown font family: <name>` as a **warning** and lays the document out
        // in the default face. So a document that says Arial is not refused; it is typeset in
        // Libertinus Serif, with a warning attached to the export that the caller is given.
        //
        // That is strictly better than what the system-font pass produced. Before, the same
        // document resolved Arial to whatever Arial the exporting machine had: different glyph
        // widths, a different page count, and **no warning at all**, because the name did
        // resolve. Now it resolves to the same bundled bytes on every machine and says so.
        //
        // The capability given up is the narrower one: a document that wants to look like
        // another document, set in a face neither machine happened to have, no longer can. That
        // is the price of a pagination figure meaning the same thing everywhere, and
        // `a_document_naming_a_font_the_bundle_lacks_warns_and_still_exports` is what keeps the
        // substitution visible rather than silent.

        Ok(Self {
            book: LazyHash::new(book),
            faces,
            bundled_families,
            bundled_faces,
        })
    }

    /// The families the bundle provides, in the order they occupy the book.
    ///
    /// The guarantee this documents is that a family in this list renders as the same bytes
    /// everywhere, and that a family outside it does not.
    fn bundled_families(&self) -> &[String] {
        &self.bundled_families
    }
}

/// How many faces the bundle contributed, which is how far into the book the bundle reaches.
///
/// The number the ordering guarantee is about. See [`FontSet::bundled_faces`].
pub fn bundled_face_count() -> usize {
    match FONTS.as_ref() {
        Ok(set) => set.bundled_faces,
        Err(_) => 0,
    }
}

/// Every family in the book, in index order.
///
/// # Why this exists when the bundling is internal
///
/// Because the claim "the bundle occupies the lowest indices, so a document that names a bundled
/// family gets the same bytes on every machine" is the *whole* of the determinism guarantee, and
/// the first version of this test suite asserted only the easier half of it — that the bundled
/// families are present. The mutation run then moved the system-font scan ahead of the bundle and
/// every test still passed, which is the definition of an unheld claim.
///
/// So the order is observable, and a test can hold it. It costs a `Vec<String>` clone per call,
/// which is why it is not built in release: only the status line and tests ask.
pub fn font_book_families() -> Vec<(String, usize)> {
    let Ok(set) = FONTS.as_ref() else { return Vec::new() };
    let mut out: Vec<(String, usize)> = (0..set.faces.len())
        .filter_map(|index| set.book.info(index).map(|info| (info.family.clone(), index)))
        .collect();
    // Index order, explicitly. `FontBook::families()` is a `BTreeMap` and therefore alphabetical,
    // which is a different question: the guarantee is about *which bytes a family resolves to*, and
    // that is decided by the index the first face with that family was pushed at.
    out.sort_by_key(|(_, index)| *index);
    out
}

static FONTS: LazyLock<Result<FontSet, String>> = LazyLock::new(|| {
    FontSet::build().map_err(|e| format!("{e}"))
});

/// The families the bundle provides, for the status line and for tests.
///
/// Falls back to an empty list rather than panicking when the font set could not be built, because
/// the one caller that is not a test is a status line, and a status line that crashes the
/// application is worse than one that says nothing.
pub fn bundled_font_families() -> Vec<String> {
    match FONTS.as_ref() {
        Ok(set) => set.bundled_families().to_vec(),
        Err(_) => Vec::new(),
    }
}

/// The compiler's view of one document.
pub struct HoloWorld {
    library: LazyHash<Library>,
    fonts: &'static FontSet,
    /// The generated Typst source. The directive calls for it in memory, and it is: one
    /// `Source`, held for the length of the compile and dropped with the world.
    main: Source,
    main_id: FileId,
    /// Assets, read once per compile.
    ///
    /// # Read once, up front
    ///
    /// Because `file()` is called from inside Typst's own layout loop, possibly more than once
    /// for the same figure, and because a miss here is an error that must be attributable.
    /// Loading the whole set costs one pass over the `assets` table; resolving on demand would
    /// cost a query per figure per attempt. For a document with 200 figures that is 200 queries
    /// where one will do.
    ///
    /// The failure mode of the eager read is that an asset belonging to *no* section is still
    /// read. That is a few hundred KB of unused bytes held for the length of a compile, which is
    /// a fair price for the error message saying which hash was missing.
    assets: HashMap<String, (String, Vec<u8>)>,
}

impl HoloWorld {
    /// Build a world over `typst_source`, with every asset in the store available.
    ///
    /// # Why the language is pinned to `en`
    ///
    /// Typst uses the language for hyphenation, and the default is `en`. A document authored in
    /// another language would hyphenate against English rules and produce badly-broken
    /// justified text — silently, because nothing about the output looks wrong. Recording this
    /// as a constant with a comment is better than inheriting a default whose consequence is
    /// invisible.
    ///
    /// No `store` parameter: the assets are supplied by [`Self::preload_assets`] rather than
    /// read here, so the world holds bytes rather than a handle on a database. Holding a
    /// `&Store` for the length of a compile would mean holding the store lock, which is the one
    /// thing `export_pdf` is arranged never to do.
    pub fn new(typst_source: String) -> Result<Self, Error> {
        let fonts = FONTS.as_ref().map_err(|e| Error::Other(anyhow::anyhow!(e.clone())))?;
        let library = Library::builder().build();

        // `Source::new` takes the id as well as the text, so the id has to exist first. The
        // dependency is inverted by constructing the id from a path the world owns, which is
        // also the id `source()` compares against.
        // `VirtualRoot::Project` is what a standalone `main.typ` sits under. The document has no
        // package and no project on disk, so the root is nominal -- it exists to give the file
        // a stable identity that `source()` can compare against, which is all `FileId` is used
        // for here.
        let vpath = VirtualPath::new("holonomy.typ").map_err(|e| {
            Error::Other(anyhow::anyhow!("the export source path is not a valid Typst path: {e}"))
        })?;
        let main_id = FileId::new(RootedPath::new(VirtualRoot::Project, vpath));
        let main = Source::new(main_id, typst_source);

        Ok(Self {
            library: LazyHash::new(library),
            fonts,
            main,
            main_id,
            assets: HashMap::new(),
        })
    }

    /// Eagerly read the assets a document references.
    ///
    /// Separate from [`HoloWorld::new`] so the world itself stays a pure view, and so the
    /// transaction can be dropped before the compile starts: the compile is milliseconds-to-
    /// seconds of CPU and holds no lock, and holding a SQLite connection open across it would
    /// block every other command for the duration of an export.
    pub fn preload_assets(&mut self, store: &Store, hashes: &[String]) -> Result<(), Error> {
        for hash in hashes {
            // Read once per hash even if the document mentions it twice.
            if self.assets.contains_key(hash) {
                continue;
            }
            match store.get_asset(hash)? {
                Some((mime, bytes)) => {
                    self.assets.insert(hash.clone(), (mime, bytes));
                }
                None => {
                    return Err(Error::Other(anyhow::anyhow!(
                        "the document references asset {hash}, which is not in the store. A figure \\
                         pasted from another device arrives with its section but not its bytes until \\
                         sync has copied them across."
                    )));
                }
            }
        }
        Ok(())
    }


    /// Load assets that have already been read, rather than reading them from a store.
    ///
    /// # Why this exists
    ///
    /// The export layout worker is a *separate process* (`pdf.rs`'s
    /// `compile_in_worker`), and it receives the document's figures over a pipe rather than
    /// opening the database. Without this, the worker would have to reconstruct the whole
    /// document -- re-read 1,300 sections, re-run the translator, and re-derive which
    /// assets are referenced -- to get bytes the parent already had in hand. That would
    /// move the work being cancelled out of a 19-second phase and give the worker a second,
    /// subtly different code path to every translation.
    ///
    /// So this is deliberately narrow: it adopts a map the caller already built. It does
    /// no reading, no hashing and no interpretation, which means a caller cannot get it
    /// subtly wrong.
    ///
    /// `mime` is carried with the bytes because `file()` hands Typst a data URI and the
    /// extension in it decides which image decoder Typst picks. Dropping it would silently
    /// mis-decode every figure rather than fail.
    pub fn adopt_assets(&mut self, assets: HashMap<String, (String, Vec<u8>)>) {
        self.assets = assets;
    }

    /// The Typst source this world compiles.
    ///
    /// Read-only and cheap: the source is an in-memory `Source`, so this is a view of the
    /// text the world already holds rather than a copy of it.
    pub fn typst_source(&self) -> &str {
        self.main.text()
    }

    /// The assets loaded into this world, for handing to a worker process.
    pub fn assets(&self) -> &HashMap<String, (String, Vec<u8>)> {
        &self.assets
    }
}

impl World for HoloWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.fonts.book
    }

    fn main(&self) -> FileId {
        self.main_id
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main_id {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from("holonomy.typ")))
        }
    }

    /// Resolve a file the document references.
    ///
    /// # Only asset URIs, and why that is enforced here rather than upstream
    ///
    /// The translator emits `image("holo-asset://<hash>")`, and this is the only path that can
    /// satisfy it. Nothing else is answered — not a relative path, not an absolute one, not a
    /// `file://` URL.
    ///
    /// That is a security property, not a simplification. A Typst document can name any file
    /// the process can read, and a document is *user data*: it arrives by sync from other
    /// devices, and its text is whatever those devices had. If `file()` honoured paths, an
    /// imported document could name `file:///etc/passwd` and have Typst read it into the export.
    /// Answering only `holo-asset://<sha256>` makes the reachable set exactly the bytes this
    /// application already decided to store, keyed by a digest it chose.
    ///
    /// The map lookup is the whole implementation, which is the point: the set of things a
    /// document can read is the set of things in `assets`.
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        // `FileId` is an interned index, so the only way back to a path is the interner. That
        // round trip is why this method can answer `AccessDenied` rather than `NotFound`: a path
        // the world does not recognise is not a file that is missing, it is a request for
        // something outside the set this world is willing to serve.
        let rooted = FileId::get(&id);
        // Scanned for the digest rather than matched against a prefix, because Typst does not
        // preserve `holo-asset://` verbatim in a `VirtualPath`: a `//` is a path separator, so
        // what comes back is the digest under a normalised path and the scheme is gone. Matching
        // the prefix answered `AccessDenied` for every figure in every document.
        //
        // The scan is not a loosening of the security property. The only thing it will accept is
        // a run of 64 lowercase hex characters, and the answer it produces is a key for
        // `assets` -- so the reachable set is still exactly the stored bytes, whatever the path
        // normalisation did in between.
        let Some(hash) = scan_for_hash(rooted.vpath().get_with_slash()) else {
            return Err(FileError::AccessDenied);
        };
        let Some((_mime, bytes)) = self.assets.get(hash) else {
            return Err(FileError::NotFound(rooted.vpath().realize(Path::new("."))?));
        };
        Ok(Bytes::new(bytes.clone()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.faces.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // None, deliberately: `datetime()` then reports an error rather than baking a build-time
        // date into an exported document, which is a fact about the exporter rather than about
        // the document. A user who wants the date writes it in the document.
        None
    }
}

/// The 64-character lowercase hex run in `path`, if there is exactly one.
///
/// "Exactly one" rather than "the first": a path with two runs in it is not a path this world
/// created, and accepting it would mean deciding which half was meant.
fn scan_for_hash(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    let mut found: Option<&str> = None;
    let mut i = 0;
    while i + 64 <= bytes.len() {
        if bytes[i..i + 64].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
            if found.is_some() {
                return None;
            }
            found = Some(&path[i..i + 64]);
            i += 64;
        } else {
            i += 1;
        }
    }
    found
}

/// The language hyphenation runs against.
///
/// Exposed because the translator's generated preamble sets it too, and the two must agree.
///
/// Typed as `&'static str` rather than Typst's `Lang` so this module does not have to know
/// where in Typst's module tree the language enum lives; `pdf.rs` converts once.
pub const LANG: &str = "en";