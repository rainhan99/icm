//! Code-graph SQLite schema (F-002, feature `code-graph`).
//!
//! Three tables + a symbol-name FTS index, created idempotently alongside
//! the memory schema. Refs carry their source `file` so a file's rows can
//! be dropped wholesale on re-index. `target_symbol` is nullable
//! (unresolved edges).

use rusqlite::Connection;

use icm_core::{IcmError, IcmResult};

/// Create the `cg_*` tables + FTS index if absent.
pub fn init_code_graph(conn: &Connection) -> IcmResult<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS cg_files (
            path         TEXT PRIMARY KEY,
            language     TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            stale        INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS cg_symbols (
            id         TEXT PRIMARY KEY,
            file       TEXT NOT NULL,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            language   TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line   INTEGER NOT NULL,
            parent     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_cg_symbols_file ON cg_symbols(file);
        CREATE INDEX IF NOT EXISTS idx_cg_symbols_name ON cg_symbols(name);

        CREATE TABLE IF NOT EXISTS cg_refs (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            file          TEXT NOT NULL,
            from_symbol   TEXT NOT NULL,
            target_name   TEXT NOT NULL,
            target_symbol TEXT,
            kind          TEXT NOT NULL,
            line          INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cg_refs_file ON cg_refs(file);
        CREATE INDEX IF NOT EXISTS idx_cg_refs_from ON cg_refs(from_symbol);
        CREATE INDEX IF NOT EXISTS idx_cg_refs_target ON cg_refs(target_symbol);
        CREATE INDEX IF NOT EXISTS idx_cg_refs_name ON cg_refs(target_name);

        CREATE VIRTUAL TABLE IF NOT EXISTS cg_symbols_fts USING fts5(
            name,
            symbol_id UNINDEXED
        );
        ",
    )
    .map_err(|e| IcmError::Database(format!("code-graph schema init: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE name = ?1",
            [name],
            |_| Ok(()),
        )
        .is_ok()
    }

    #[test]
    fn creates_all_code_graph_objects() {
        let conn = Connection::open_in_memory().unwrap();
        init_code_graph(&conn).unwrap();
        for t in ["cg_files", "cg_symbols", "cg_refs", "cg_symbols_fts"] {
            assert!(table_exists(&conn, t), "missing table {t}");
        }
        // Idempotent.
        init_code_graph(&conn).unwrap();
    }
}
