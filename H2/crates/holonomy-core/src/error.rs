//! Errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("corrupt section blob for section {section_id}: {reason}")]
    Corrupt { section_id: String, reason: String },

    #[error("document {0} not found")]
    DocumentNotFound(String),

    #[error("section {0} not found")]
    SectionNotFound(String),

    #[error("schema version mismatch: found {found}, expected {expected}")]
    SchemaVersion { found: i64, expected: i64 },

    /// A migration failed while upgrading an older file in place.
    ///
    /// Distinct from [`Error::SchemaVersion`] because the two demand opposite responses
    /// from a user: a version mismatch means "this build is not the one for this file",
    /// which is not actionable, while this means the file *was* this build's to read and
    /// the upgrade could not complete. Reporting the latter as a version number would
    /// tell a user their document came from a future Holonomy, which is the one thing
    /// it is not.
    #[error("migrating this document to schema version {version} failed: {source}")]
    Migration {
        version: usize,
        #[source]
        source: rusqlite::Error,
    },

    /// The path was not a Holonomy document.
    ///
    /// One variant for both "not a database" and "a database, not a document", because the
    /// action is the same either way — open a different file — and the two cases are told
    /// apart by [`crate::holo::probe`] if a caller wants to say more. The message names the
    /// path rather than the reason because the reason is the part the user cannot act on:
    /// they cannot make their file into a document, but they can pick another one.
    #[error("{path} is not a Holonomy document")]
    NotADocument { path: String },

    #[error("section {section_id} cannot be split: {reason}")]
    Unsplittable { section_id: String, reason: String },

    #[error("undo stack is empty for document {0}")]
    UndoEmpty(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
