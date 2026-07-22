//! SQLite implementation of [`CodeGraphStore`] (F-002).
//!
//! `explore` uses the trait's default (local BFS). All other methods are
//! direct SQL against the `cg_*` tables. `index_file` is transactional
//! (delete-then-insert per file) so re-indexing is idempotent; the FTS
//! triggers keep `cg_symbols_fts` in sync on the delete + insert.

use rusqlite::{params, Connection};

use icm_core::{
    CodeFile, CodeGraphStore, CodeLanguage, CodeStats, IcmError, IcmResult, Ref, RefKind, Symbol,
    SymbolKind,
};

use crate::store::SqliteStore;

fn db_err(e: rusqlite::Error) -> IcmError {
    IcmError::Database(e.to_string())
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Module => "module",
        SymbolKind::Constant => "constant",
        SymbolKind::TypeAlias => "type_alias",
        SymbolKind::Field => "field",
        SymbolKind::Variable => "variable",
    }
}

fn parse_kind(s: &str) -> SymbolKind {
    match s {
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "module" => SymbolKind::Module,
        "constant" => SymbolKind::Constant,
        "type_alias" => SymbolKind::TypeAlias,
        "field" => SymbolKind::Field,
        "variable" => SymbolKind::Variable,
        _ => SymbolKind::Function,
    }
}

fn parse_lang(s: &str) -> CodeLanguage {
    match s {
        "typescript" => CodeLanguage::TypeScript,
        "javascript" => CodeLanguage::JavaScript,
        "python" => CodeLanguage::Python,
        "go" => CodeLanguage::Go,
        _ => CodeLanguage::Rust,
    }
}

fn ref_kind_str(k: RefKind) -> &'static str {
    match k {
        RefKind::Call => "call",
        RefKind::Import => "import",
        RefKind::Inherit => "inherit",
        RefKind::TypeUse => "type_use",
    }
}

/// SELECT column list for `cg_symbols`, in `row_to_symbol` order.
const SYM_COLS: &str = "id, file, name, kind, language, start_line, end_line, parent";

fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<Symbol> {
    Ok(Symbol {
        id: row.get(0)?,
        file: row.get(1)?,
        name: row.get(2)?,
        kind: parse_kind(&row.get::<_, String>(3)?),
        language: parse_lang(&row.get::<_, String>(4)?),
        start_line: row.get::<_, i64>(5)? as u32,
        end_line: row.get::<_, i64>(6)? as u32,
        parent: row.get(7)?,
    })
}

fn query_symbols(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> IcmResult<Vec<Symbol>> {
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let rows = stmt
        .query_map(args, row_to_symbol)
        .map_err(db_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_err)?;
    Ok(rows)
}

impl CodeGraphStore for SqliteStore {
    fn index_file(&self, file: &CodeFile, symbols: &[Symbol], refs: &[Ref]) -> IcmResult<()> {
        let conn = self.cg_conn();
        conn.execute_batch("BEGIN IMMEDIATE;").map_err(db_err)?;
        let result = (|| -> IcmResult<()> {
            conn.execute("DELETE FROM cg_symbols WHERE file = ?1", params![file.path])
                .map_err(db_err)?;
            conn.execute("DELETE FROM cg_refs WHERE file = ?1", params![file.path])
                .map_err(db_err)?;
            conn.execute(
                "INSERT OR REPLACE INTO cg_files (path, language, content_hash, stale)
                 VALUES (?1, ?2, ?3, 0)",
                params![file.path, file.language.as_str(), file.content_hash],
            )
            .map_err(db_err)?;
            for s in symbols {
                conn.execute(
                    "INSERT OR REPLACE INTO cg_symbols
                     (id, file, name, kind, language, start_line, end_line, parent)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        s.id,
                        s.file,
                        s.name,
                        kind_str(s.kind),
                        s.language.as_str(),
                        s.start_line as i64,
                        s.end_line as i64,
                        s.parent,
                    ],
                )
                .map_err(db_err)?;
            }
            for r in refs {
                conn.execute(
                    "INSERT INTO cg_refs (file, from_symbol, target_name, target_symbol, kind, line)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        file.path,
                        r.from_symbol,
                        r.target_name,
                        r.target_symbol,
                        ref_kind_str(r.kind),
                        r.line as i64,
                    ],
                )
                .map_err(db_err)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;").map_err(db_err)?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    fn delete_file(&self, path: &str) -> IcmResult<()> {
        let conn = self.cg_conn();
        conn.execute("DELETE FROM cg_symbols WHERE file = ?1", params![path])
            .map_err(db_err)?;
        conn.execute("DELETE FROM cg_refs WHERE file = ?1", params![path])
            .map_err(db_err)?;
        conn.execute("DELETE FROM cg_files WHERE path = ?1", params![path])
            .map_err(db_err)?;
        Ok(())
    }

    fn file_hash(&self, path: &str) -> IcmResult<Option<String>> {
        self.cg_conn()
            .query_row(
                "SELECT content_hash FROM cg_files WHERE path = ?1",
                params![path],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err(other)),
            })
    }

    fn get_symbol(&self, id: &str) -> IcmResult<Option<Symbol>> {
        let sql = format!("SELECT {SYM_COLS} FROM cg_symbols WHERE id = ?1");
        Ok(query_symbols(self.cg_conn(), &sql, &[&id])?.into_iter().next())
    }

    fn find_symbols(&self, name: &str, limit: usize) -> IcmResult<Vec<Symbol>> {
        // FTS match on symbol name; join back to cg_symbols by rowid.
        let sql = format!(
            "SELECT {} FROM cg_symbols s
             JOIN cg_symbols_fts f ON s.rowid = f.rowid
             WHERE cg_symbols_fts MATCH ?1
             LIMIT ?2",
            SYM_COLS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", ")
        );
        // Quote the term so punctuation/keywords are treated literally.
        let term = format!("\"{}\"", name.replace('"', "\"\""));
        query_symbols(self.cg_conn(), &sql, &[&term, &(limit as i64)])
    }

    fn callers(&self, symbol_id: &str) -> IcmResult<Vec<Symbol>> {
        let sql = format!(
            "SELECT DISTINCT {} FROM cg_symbols s
             JOIN cg_refs r ON r.from_symbol = s.id
             WHERE r.target_symbol = ?1",
            SYM_COLS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", ")
        );
        query_symbols(self.cg_conn(), &sql, &[&symbol_id])
    }

    fn callees(&self, symbol_id: &str) -> IcmResult<Vec<Symbol>> {
        let sql = format!(
            "SELECT DISTINCT {} FROM cg_symbols s
             JOIN cg_refs r ON r.target_symbol = s.id
             WHERE r.from_symbol = ?1",
            SYM_COLS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", ")
        );
        query_symbols(self.cg_conn(), &sql, &[&symbol_id])
    }

    fn list_stale(&self) -> IcmResult<Vec<String>> {
        let conn = self.cg_conn();
        let mut stmt = conn
            .prepare("SELECT path FROM cg_files WHERE stale = 1")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    fn code_stats(&self) -> IcmResult<CodeStats> {
        let conn = self.cg_conn();
        let count = |sql: &str| -> IcmResult<usize> {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|n| n as usize)
                .map_err(db_err)
        };
        let files = count("SELECT COUNT(*) FROM cg_files")?;
        let symbols = count("SELECT COUNT(*) FROM cg_symbols")?;
        let refs = count("SELECT COUNT(*) FROM cg_refs")?;
        let stale_files = count("SELECT COUNT(*) FROM cg_files WHERE stale = 1")?;
        let mut stmt = conn
            .prepare("SELECT language, COUNT(*) FROM cg_symbols GROUP BY language ORDER BY 2 DESC")
            .map_err(db_err)?;
        let by_language = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(CodeStats {
            files,
            symbols,
            refs,
            stale_files,
            by_language,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteStore {
        SqliteStore::in_memory_with_dims(384).unwrap()
    }

    fn sym(id: &str, name: &str, kind: SymbolKind, file: &str, l: u32) -> Symbol {
        Symbol {
            id: id.into(),
            file: file.into(),
            name: name.into(),
            kind,
            language: CodeLanguage::Rust,
            start_line: l,
            end_line: l,
            parent: None,
        }
    }

    #[test]
    fn index_query_callers_callees_roundtrip() {
        let s = store();
        let a = sym("a.rs#a@1", "a", SymbolKind::Function, "a.rs", 1);
        let b = sym("a.rs#b@2", "b", SymbolKind::Function, "a.rs", 2);
        let call = Ref {
            from_symbol: a.id.clone(),
            target_name: "b".into(),
            target_symbol: Some(b.id.clone()),
            kind: RefKind::Call,
            line: 1,
        };
        let file = CodeFile {
            path: "a.rs".into(),
            language: CodeLanguage::Rust,
            content_hash: "h1".into(),
            stale: false,
        };
        s.index_file(&file, &[a.clone(), b.clone()], &[call]).unwrap();

        assert_eq!(s.get_symbol(&a.id).unwrap().unwrap().name, "a");
        assert_eq!(s.file_hash("a.rs").unwrap().as_deref(), Some("h1"));
        assert_eq!(s.find_symbols("b", 5).unwrap().len(), 1);
        // b is called by a.
        let callers = s.callers(&b.id).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].id, a.id);
        // a calls b.
        let callees = s.callees(&a.id).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].id, b.id);

        let st = s.code_stats().unwrap();
        assert_eq!(st.files, 1);
        assert_eq!(st.symbols, 2);
        assert_eq!(st.refs, 1);
    }

    #[test]
    fn reindex_replaces_file_rows() {
        let s = store();
        let file = CodeFile {
            path: "a.rs".into(),
            language: CodeLanguage::Rust,
            content_hash: "h1".into(),
            stale: false,
        };
        s.index_file(&file, &[sym("a.rs#a@1", "a", SymbolKind::Function, "a.rs", 1)], &[])
            .unwrap();
        // Re-index same file with a different symbol set.
        let file2 = CodeFile {
            content_hash: "h2".into(),
            ..file.clone()
        };
        s.index_file(&file2, &[sym("a.rs#z@1", "z", SymbolKind::Function, "a.rs", 1)], &[])
            .unwrap();
        assert!(s.find_symbols("a", 5).unwrap().is_empty(), "old symbol gone");
        assert_eq!(s.find_symbols("z", 5).unwrap().len(), 1);
        assert_eq!(s.file_hash("a.rs").unwrap().as_deref(), Some("h2"));
        assert_eq!(s.code_stats().unwrap().symbols, 1);
    }

    #[test]
    fn delete_file_clears_rows() {
        let s = store();
        let file = CodeFile {
            path: "a.rs".into(),
            language: CodeLanguage::Rust,
            content_hash: "h1".into(),
            stale: false,
        };
        s.index_file(&file, &[sym("a.rs#a@1", "a", SymbolKind::Function, "a.rs", 1)], &[])
            .unwrap();
        s.delete_file("a.rs").unwrap();
        assert!(s.find_symbols("a", 5).unwrap().is_empty());
        assert_eq!(s.code_stats().unwrap().files, 0);
    }
}
