//! Test helpers.

use crate::schema::{MIGRATIONS, SCHEMA_VERSION};
use rusqlite::Connection;

/// An in-memory database with the full schema applied.
#[cfg(test)]
pub fn temp_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    for m in MIGRATIONS {
        conn.execute_batch(m).unwrap_or_else(|e| panic!("migration failed: {e}"));
    }
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )
    .unwrap();
    conn
}
