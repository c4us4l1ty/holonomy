//! PDF export.
//!
//! Three pieces, and the order they matter in:
//!
//! 1. [`translate`] turns section JSON into a Typst source string. It is pure and it is where
//!    almost every interesting question lives: what does a table become, what does an equation
//!    become, and what happens to a node type nobody thought of.
//! 2. [`world`] supplies the environment Typst compiles in — fonts, and the assets the
//!    document names.
//! 3. [`pdf`] drives the two and reports what came out, including how long it took, through
//!    [`progress`] so a caller can watch a 54-second export and stop one that is going wrong.
//!
//! The split is deliberate: the translator is a string builder and can be tested by comparing
//! strings, which is a far better failure message than "the PDF had a syntax error somewhere",
//! and the world is a lookup table, which can be tested by asking it for things.

pub mod parity;
pub mod pdf;
pub mod progress;
pub mod translate;
pub mod worker;
pub mod world;
