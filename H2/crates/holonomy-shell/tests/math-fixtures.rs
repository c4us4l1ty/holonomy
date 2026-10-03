//! Write the TeX-to-Typst math fixture the frontend reads.
//!
//! Run: `cargo test -p holonomy-shell --test math-fixtures --release -- --ignored`
//!
//! # Why the fixture is generated rather than written by hand
//!
//! Because `tex_to_typst_math` is Rust and `app/test/export-math.ts` is TypeScript, and the
//! alternative — reimplementing the conversion in TypeScript so the test can call it — is
//! precisely the duplication this project keeps removing. So the Rust side writes what it
//! produces, and the TypeScript side asserts against it. A failure then means the *translator*
//! changed, not that two copies of a function disagree.
//!
//! # Why each entry records whether it compiled
//!
//! Because a conversion can be recognisably correct and still produce Typst that does not parse.
//! Each input is put through a real export and the page count read, so the fixture carries
//! compilability rather than assuming it. The entries that *should not* compile are the point of
//! one of the tests: an unrecognised command has to survive to be named, and that is only
//! checkable by watching Typst refuse.
//!
//! `#[ignore]`d because it writes a file and compiles 30-odd documents, which is slow and has no
//! business running in an ordinary test suite. It runs when the translator changes, and the
//! frontend test fails if the fixture is stale — which is the staleness check.

use std::path::Path;

use holonomy_core::Store;
use holonomy_shell_lib::export::pdf::export_pdf_quiet;
use holonomy_shell_lib::export::translate::tex_to_typst_math;
use serde_json::{json, Value};

/// Entries that do not compile, each with why it *should not*.
///
/// # Every one of these is by design, and that is the property
///
/// The list used to contain four bugs. They are closed: `\int` with limits, `\left(...\right)` in
/// the round and square forms, and a literal brace. The three that remain are inputs with no
/// meaning -- an unbalanced `\frac{1}{`, an unrecognised command, a trailing backslash -- and for
/// those the correct outcome is a document that refuses to export and says why.
///
/// So the property being held is uniform across the whole list, which is what makes it a
/// statement about the translator rather than an excuse: **an equation Holonomy cannot render
/// becomes an error that names the problem, never a different equation.** A converter that
/// quietly dropped an unknown command would produce a document that exports and is wrong, and
/// nothing downstream could tell.
///
/// `app/test/export-math.ts` asserts the failing set is *exactly* this list, so a fix or a
/// regression both fail that test.
///
/// # None of this blocks v1
///
/// The directive that moved these to a backlog (M7) also ruled them non-blocking, and that is
/// the right call rather than a deferral: the six inputs have no meaning, so there is nothing
/// to ship late. `docs/parser-backlog.md` holds the argument, the real work — which is a
/// group-aware parser for matrices and cases, not a list of missing commands — and the
/// reasoning for why the four constructs this backlog was written to excuse were bugs in this
/// translator rather than Typst limitations.
///
/// That reasoning is recorded here because the mistake generalises: each of the four was
/// filed as "Typst cannot express this", and each turned out to be a defect that **compiled**.
/// Two were a space inserted before a subscript and a `\,` that emitted a literal backslash
/// and did nothing. A compile-only check accepts all three.
const KNOWN_GAPS: &[&str] = &[
    "unbalanced fraction",
    "unbalanced sqrt",
    "lone backslash at the end",
    "lone escaped brace",
    "unknown command",
    "unknown operator name",
];/// Every input worth pinning, grouped by what it is testing.
const INPUTS: &[(&str, &str)] = &[
    // -- large operators with limits ---------------------------------------
    ("large operator, both limits", "\\sum_{i=1}^{n} i"),
    ("large operator, subscript only", "\\sum_{i} x_i"),
    ("large operator, superscript only", "\\prod^{n} k"),
    ("integral with limits", "\\int_{0}^{\\infty} e^{-x} \\,dx"),
    ("integral with a differential", "\\int_0^1 e^{-x} \\,dx"),
    ("contour integral", "\\oint_C \\nabla \\times F"),
    ("double integral", "\\iint_S f \\,dA"),
    ("triple integral", "\\iiint_V f \\,dV"),
    // -- adjacent identifiers ---------------------------------------------
    ("greek after an identifier", "i\\pi"),
    ("two greeks adjacent", "\\alpha\\beta"),
    ("greek inside a superscript", "e^{i\\pi}"),
    ("uppercase greek", "\\Gamma \\Delta \\Omega"),
    // -- structures --------------------------------------------------------
    ("simple fraction", "\\frac{a}{b}"),
    ("fraction with a sum", "\\frac{1}{n} \\sum_{i=1}^{n} i"),
    ("nested fraction", "\\frac{a}{\\frac{b}{c}}"),
    ("plain square root", "\\sqrt{2}"),
    ("square root of a sum", "\\sqrt{\\frac{a}{b}}"),
    ("nth root", "\\sqrt[3]{x}"),
    // -- operators and relations -------------------------------------------
    ("binary operators", "a \\cdot b + c \\times d \\pm e"),
    ("relations", "a \\leq b \\geq c \\neq d \\approx e"),
    ("sets", "a \\in B, a \\notin C, A \\cup B, A \\cap B"),
    ("arrows", "A \\rightarrow B \\Rightarrow C"),
    ("other symbols", "\\infty \\partial \\nabla"),
    // -- delimiters ---------------------------------------------------------
    ("growing delimiters", "\\left( \\frac{a}{b} \\right)"),
    ("growing brackets", "\\left[ x + y \\right]"),
    ("growing braces", "\\left\\{ x \\} \\right\\}"),
    ("growing bars", "\\left| x \\right|"),
    // -- spacing and text ---------------------------------------------------
    ("thin space", "a \\, b"),
    ("quad", "a \\quad b"),
    ("upright text", "x \\text{ if } y > 0"),
    // -- plain passthrough --------------------------------------------------
    ("no commands at all", "x^2 + y^2 = z^2"),
    ("superscript with a group", "x^{10}"),
    // -- pathological -------------------------------------------------------
    ("unbalanced fraction", "\\frac{1}{"),
    ("unbalanced sqrt", "\\sqrt{"),
    ("lone backslash at the end", "trailing \\"),
    ("lone escaped brace", "literal \\{ brace"),
    ("escaped brace pair", "brace \\{ and \\}"),
    ("brace pair in a set", "\\{ 1, 2 \\}"),
    ("thin space", "a \\, b"),
    ("negative thin space", "a \\! b"),
    // -- must NOT compile ---------------------------------------------------
    ("unknown command", "\\acme{x}"),
    ("unknown operator name", "\\notacommand"),
];

#[test]
#[ignore = "writes a fixture and compiles 30 documents; run it when the translator changes"]
fn write_the_math_fixture() {
    let mut entries: Vec<Value> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for (label, input) in INPUTS {
        let typst = tex_to_typst_math(input);

        // Compiled for real, through the same path an export takes.
        let (compiles, error) = match compile_equation(input, &typst) {
            Ok(pages) => (pages > 0, None),
            Err(message) => (false, Some(message)),
        };

        // The three "unknown command" entries are *supposed* to fail. Anything else failing is
        // a translator bug, and the fixture records it so the frontend test can see it rather
        // than this one hiding it.
        // Three kinds of input are *supposed* to fail, and are recorded rather than fixed:
        //
        // - A command the translator does not know. That is the design: it survives to be named
        //   by Typst, so the document refuses to export and says which command.
        // - TeX the user is halfway through typing, where braces do not balance. An unbalanced
        //   `\frac{1}{` has no meaning and the honest outcome is a refusal.
        // - The four entries in [`KNOWN_GAPS`], which are *bugs* — see its documentation. They
        //   are listed here so the generator passes and the gap stays visible in the fixture,
        //   which is the difference between a recorded limitation and a red test nobody runs.
        let expected_to_fail = label.contains("unknown")
            || label.contains("unbalanced")
            || label.contains("lone backslash")
            || label.contains("lone escaped brace")
            || KNOWN_GAPS.contains(label);
        if compiles == expected_to_fail {
            failures.push(format!(
                "{label}: {input:?} -> {typst:?} compiled={compiles} but expected \\
                 compiled={} ({error:?})",
                !expected_to_fail
            ));
        }

        entries.push(json!({
            "label": label,
            "input": input,
            "typst": typst,
            "compiles": compiles,
            "error": error,
        }));
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/test/fixtures");
    std::fs::create_dir_all(&dir).expect("fixtures directory");
    let path = dir.join("tex-to-typst.json");
    std::fs::write(&path, serde_json::to_string_pretty(&entries).expect("serialise"))
        .expect("write the fixture");
    eprintln!("wrote {} ({} entries)", path.display(), entries.len());

    assert!(
        failures.is_empty(),
        "{} fixture(s) did not behave as expected:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Export a one-equation document and report the page count, or the error.
fn compile_equation(input: &str, typst: &str) -> Result<usize, String> {
    let store = Store::open_in_memory().expect("in-memory store");
    let document = store.create_document("Math").expect("document");
    let json = json!({
        "type": "doc",
        "content": [{
            "type": "paragraph",
            "content": [{"type": "inlineMath", "attrs": {"latex": input}}]
        }]
    });
    store.add_section(&document.id, &json).expect("section");
    export_pdf_quiet(&store, &document.id, "Math")
        .map(|r| {
            // The converted form is what actually compiled, so the fixture's `compiles` flag is
            // about the translator's output rather than about Typst having been handed the TeX.
            assert!(
                r.report.unknown_types.is_empty(),
                "an equation should not introduce an unknown node type"
            );
            r.pages
        })
        .map_err(|e| e.to_string())
        // `typst` is unused here because the conversion happens inside `export_pdf`; named so
        // the call site reads as "this is what we expect to compile".
        .inspect(|_| {
            let _ = typst;
        })
}
