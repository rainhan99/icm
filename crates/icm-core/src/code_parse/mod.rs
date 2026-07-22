//! Tree-sitter parsing kernel (F-002, feature `code-graph`).
//!
//! `parse_file` turns source text into a [`ParsedFile`] (symbols + raw,
//! unresolved reference edges). Per-language extraction lives in the
//! submodules; this module owns the tree-sitter `Parser` setup and the
//! shared symbol-id scheme.
//!
//! We walk the syntax tree manually (via `TreeCursor`) rather than using
//! `tree_sitter::Query`, which in 0.25 returns a `StreamingIterator` and
//! would pull an extra dependency. Manual walking keeps deps minimal and
//! gives per-language control over nesting (impl methods, receivers, …).

use tree_sitter::{Node, Parser};

use crate::code_graph::{CodeLanguage, Ref, Symbol};
use crate::error::{IcmError, IcmResult};

mod rust;
mod typescript;

/// Result of parsing one file: definitions + (unresolved) reference edges.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedFile {
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Ref>,
}

/// Resolve a [`CodeLanguage`] to its tree-sitter grammar.
fn ts_language(language: CodeLanguage) -> tree_sitter::Language {
    match language {
        CodeLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        // The TSX grammar is a superset that also parses plain JS/JSX.
        CodeLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        CodeLanguage::JavaScript => tree_sitter_typescript::LANGUAGE_TSX.into(),
        CodeLanguage::Python => tree_sitter_python::LANGUAGE.into(),
        CodeLanguage::Go => tree_sitter_go::LANGUAGE.into(),
    }
}

/// Parse `source` of `language` at repository-relative `rel_path`.
pub fn parse_file(language: CodeLanguage, rel_path: &str, source: &str) -> IcmResult<ParsedFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language(language))
        .map_err(|e| IcmError::CodeGraph(format!("set_language({}): {e}", language.as_str())))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| IcmError::CodeGraph(format!("parse failed for {rel_path}")))?;
    let root = tree.root_node();

    let (symbols, refs) = match language {
        CodeLanguage::Rust => rust::extract(root, source, rel_path),
        CodeLanguage::TypeScript | CodeLanguage::JavaScript => {
            typescript::extract(root, source, rel_path, language)
        }
        // Remaining languages land in later tasks; empty until then.
        _ => (Vec::new(), Vec::new()),
    };
    Ok(ParsedFile { symbols, refs })
}

// --- shared helpers for the per-language extractors ---------------------

/// Stable symbol id: `{relpath}#{name}@{1-based-start-line}`. Unchanged
/// symbols re-index to the same id (name+line are unique within a file).
pub(crate) fn symbol_id(path: &str, name: &str, start_line: u32) -> String {
    format!("{path}#{name}@{start_line}")
}

/// 1-based start/end line of a node.
pub(crate) fn node_lines(node: &Node) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

/// Text of a named child field, if present and valid UTF-8.
pub(crate) fn field_text(node: &Node, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_graph::SymbolKind;

    #[test]
    fn rust_extracts_function_and_struct() {
        let parsed = parse_file(CodeLanguage::Rust, "src/a.rs", "pub fn foo(){} struct Bar;")
            .expect("parse");
        let names: Vec<_> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "symbols: {names:?}");
        assert!(names.contains(&"Bar"), "symbols: {names:?}");
        let foo = parsed.symbols.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(foo.kind, SymbolKind::Function);
        let bar = parsed.symbols.iter().find(|s| s.name == "Bar").unwrap();
        assert_eq!(bar.kind, SymbolKind::Struct);
    }

    #[test]
    fn typescript_extracts_function_and_class() {
        let parsed = parse_file(
            CodeLanguage::TypeScript,
            "src/a.ts",
            "export function foo(){} class Bar{ m(){} }",
        )
        .expect("parse");
        let get = |n: &str| parsed.symbols.iter().find(|s| s.name == n).cloned();
        assert_eq!(get("foo").map(|s| s.kind), Some(SymbolKind::Function));
        assert_eq!(get("Bar").map(|s| s.kind), Some(SymbolKind::Class));
        assert_eq!(get("m").map(|s| s.kind), Some(SymbolKind::Method));
    }

    #[test]
    fn javascript_arrow_const_is_function() {
        let parsed = parse_file(CodeLanguage::JavaScript, "src/a.js", "const foo = () => {};")
            .expect("parse");
        assert_eq!(
            parsed.symbols.iter().find(|s| s.name == "foo").map(|s| s.kind),
            Some(SymbolKind::Function)
        );
    }
}
