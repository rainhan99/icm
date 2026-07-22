//! Call/reference edge extraction (F-002, unresolved stage).
//!
//! Collects call sites across the tree, then assigns each to the
//! enclosing symbol (deepest span containing the call line). Targets stay
//! unresolved (`target_symbol: None`) here — `code_resolve` links them to
//! definitions later.

use tree_sitter::Node;

use super::field_text;
use crate::code_graph::{CodeLanguage, Ref, RefKind, Symbol};

/// Build unresolved call edges for a file, attributed to enclosing symbols.
pub(super) fn extract_refs(
    root: Node,
    source: &str,
    language: CodeLanguage,
    symbols: &[Symbol],
) -> Vec<Ref> {
    let mut calls: Vec<(String, u32)> = Vec::new();
    collect(root, source, language, &mut calls);
    calls
        .into_iter()
        .filter_map(|(name, line)| {
            enclosing(symbols, line).map(|from| Ref {
                from_symbol: from,
                target_name: name,
                target_symbol: None,
                kind: RefKind::Call,
                line,
            })
        })
        .collect()
}

/// Deepest symbol whose line span contains `line`.
fn enclosing(symbols: &[Symbol], line: u32) -> Option<String> {
    symbols
        .iter()
        .filter(|s| s.start_line <= line && line <= s.end_line)
        .min_by_key(|s| s.end_line - s.start_line)
        .map(|s| s.id.clone())
}

fn collect(node: Node, source: &str, language: CodeLanguage, out: &mut Vec<(String, u32)>) {
    if let Some(name) = callee(&node, source, language) {
        out.push((name, node.start_position().row as u32 + 1));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, language, out);
    }
}

/// If `node` is a call in `language`, return the callee's leaf name.
fn callee(node: &Node, source: &str, language: CodeLanguage) -> Option<String> {
    match language {
        CodeLanguage::Rust => match node.kind() {
            "call_expression" => name_from(node.child_by_field_name("function"), source),
            "macro_invocation" => field_text(node, "macro", source),
            _ => None,
        },
        CodeLanguage::TypeScript | CodeLanguage::JavaScript | CodeLanguage::Go => {
            if node.kind() == "call_expression" {
                name_from(node.child_by_field_name("function"), source)
            } else {
                None
            }
        }
        CodeLanguage::Python => {
            if node.kind() == "call" {
                name_from(node.child_by_field_name("function"), source)
            } else {
                None
            }
        }
    }
}

/// Extract the leaf identifier of a (possibly qualified) callee node.
fn name_from(node: Option<Node>, source: &str) -> Option<String> {
    let n = node?;
    match n.kind() {
        "identifier" | "type_identifier" | "field_identifier" | "property_identifier" => {
            n.utf8_text(source.as_bytes()).ok().map(str::to_string)
        }
        // Qualified callees: take the trailing segment.
        "field_expression" => field_text(&n, "field", source),
        "scoped_identifier" => field_text(&n, "name", source),
        "member_expression" => field_text(&n, "property", source),
        "attribute" => field_text(&n, "attribute", source),
        "selector_expression" => field_text(&n, "field", source),
        _ => None,
    }
}
