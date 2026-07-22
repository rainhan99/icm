//! Code-graph types and store trait (F-002, feature `code-graph`).
//!
//! The code graph is a persistent, incrementally-maintained index of
//! symbols (functions/types/…) and the references/calls between them,
//! extracted by the tree-sitter kernel (`code_parse`) and resolved by
//! `code_resolve`. Agents query it (via `icm_code_explore`) to answer
//! structural questions in one call instead of grep/read crawling.
//!
//! Pure data types + a store trait — no I/O here. The SQLite
//! implementation lives in `icm-store`; parsing lives in `code_parse`.

use serde::{Deserialize, Serialize};

use crate::error::IcmResult;

/// Languages the parser supports (one tree-sitter grammar each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeLanguage {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
}

impl CodeLanguage {
    /// Detect language from a file extension (lowercase, no dot).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }
}

/// Kind of a code symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Module,
    Constant,
    TypeAlias,
    Field,
    Variable,
}

/// A definition extracted from a source file. `id` is a stable content
/// key `{relpath}#{kind}:{name}@{start_line}` so re-indexing a file
/// yields the same ids for unchanged symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: String,
    /// Repository-**relative** path (never absolute — remote-share key).
    pub file: String,
    pub name: String,
    pub kind: SymbolKind,
    pub language: CodeLanguage,
    pub start_line: u32,
    pub end_line: u32,
    /// Enclosing symbol id (e.g. a method's class), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Kind of a reference edge between code locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Call,
    Import,
    Inherit,
    TypeUse,
}

/// A reference/call edge. `target_symbol` is `Some` once resolved by
/// `code_resolve`; it stays `None` for unresolvable targets (dynamic
/// dispatch, external crates) — an authorized limitation, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// Symbol id the reference originates from (the caller).
    pub from_symbol: String,
    pub target_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_symbol: Option<String>,
    pub kind: RefKind,
    pub line: u32,
}

/// An indexed source file (for incremental staleness via content hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeFile {
    /// Repository-relative path.
    pub path: String,
    pub language: CodeLanguage,
    /// Content hash (hex) used to skip unchanged files on re-index.
    pub content_hash: String,
    #[serde(default)]
    pub stale: bool,
}

/// One-call structural answer for a symbol: its definition + verbatim
/// source + who calls it + what it calls + transitive blast radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreResult {
    pub symbol: Symbol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub callers: Vec<Symbol>,
    pub callees: Vec<Symbol>,
    /// Transitively-affected symbols (bounded BFS over callers).
    pub blast_radius: Vec<Symbol>,
}

/// Aggregate index statistics for observability (`icm code stats`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeStats {
    pub files: usize,
    pub symbols: usize,
    pub refs: usize,
    pub stale_files: usize,
    /// (language, symbol count), for a quick language breakdown.
    pub by_language: Vec<(String, usize)>,
}

/// Persistent code-graph store. Implemented by the SQLite backend and,
/// over JSON-RPC, by the remote client (F-002 SC-9).
pub trait CodeGraphStore {
    /// Replace all rows for `file.path` with the given symbols/refs
    /// (transactional: delete-then-insert). Used by full + incremental
    /// indexing; safe to call repeatedly (idempotent per file).
    fn index_file(&self, file: &CodeFile, symbols: &[Symbol], refs: &[Ref]) -> IcmResult<()>;

    /// Remove a file and all its symbols/refs from the graph.
    fn delete_file(&self, path: &str) -> IcmResult<()>;

    /// Stored content hash for a path, if indexed (incremental skip).
    fn file_hash(&self, path: &str) -> IcmResult<Option<String>>;

    fn get_symbol(&self, id: &str) -> IcmResult<Option<Symbol>>;

    /// FTS symbol-name search.
    fn find_symbols(&self, name: &str, limit: usize) -> IcmResult<Vec<Symbol>>;

    /// Symbols that reference (call) `symbol_id`.
    fn callers(&self, symbol_id: &str) -> IcmResult<Vec<Symbol>>;

    /// Symbols referenced (called) by `symbol_id`.
    fn callees(&self, symbol_id: &str) -> IcmResult<Vec<Symbol>>;

    /// Single-call structural answer for the best match of `name`,
    /// computing blast radius up to `max_depth`. Default implementation
    /// does a local BFS over callers — correct for the SQLite backend.
    /// The remote client OVERRIDES this with one server-side RPC so a
    /// thin client never does BFS across the network.
    ///
    /// `source` is left `None` here (the store has no file access); the
    /// CLI/MCP layer fills it from disk.
    fn explore(&self, name: &str, max_depth: usize) -> IcmResult<Option<ExploreResult>> {
        let Some(symbol) = self.find_symbols(name, 1)?.into_iter().next() else {
            return Ok(None);
        };
        let callers = self.callers(&symbol.id)?;
        let callees = self.callees(&symbol.id)?;
        // Bounded BFS over transitive callers for the blast radius.
        let mut seen = std::collections::HashSet::new();
        seen.insert(symbol.id.clone());
        let mut blast: Vec<Symbol> = Vec::new();
        let mut frontier: Vec<Symbol> = callers.clone();
        let mut depth = 0;
        while !frontier.is_empty() && depth < max_depth {
            let mut next = Vec::new();
            for s in frontier.drain(..) {
                if seen.insert(s.id.clone()) {
                    next.extend(self.callers(&s.id)?);
                    blast.push(s);
                }
            }
            frontier = next;
            depth += 1;
        }
        Ok(Some(ExploreResult {
            symbol,
            source: None,
            callers,
            callees,
            blast_radius: blast,
        }))
    }

    /// Paths currently flagged stale (edited but not re-indexed).
    fn list_stale(&self) -> IcmResult<Vec<String>>;

    /// Flag the given (repo-relative) paths as stale — edited but not yet
    /// re-indexed. No-op for paths not in the graph. Cleared by
    /// `index_file`. Used by the PostToolUse hook (cheap; the actual
    /// re-parse happens on the next `icm code index --incremental`).
    fn mark_stale(&self, paths: &[String]) -> IcmResult<()>;

    fn code_stats(&self) -> IcmResult<CodeStats>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_roundtrips() {
        let s = Symbol {
            id: "src/a.rs#function:foo@1".into(),
            file: "src/a.rs".into(),
            name: "foo".into(),
            kind: SymbolKind::Function,
            language: CodeLanguage::Rust,
            start_line: 1,
            end_line: 3,
            parent: None,
        };
        let bytes = serde_json::to_vec(&s).unwrap();
        let back: Symbol = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn language_detection() {
        assert_eq!(CodeLanguage::from_extension("rs"), Some(CodeLanguage::Rust));
        assert_eq!(
            CodeLanguage::from_extension("tsx"),
            Some(CodeLanguage::TypeScript)
        );
        assert_eq!(CodeLanguage::from_extension("go"), Some(CodeLanguage::Go));
        assert_eq!(CodeLanguage::from_extension("txt"), None);
    }
}
