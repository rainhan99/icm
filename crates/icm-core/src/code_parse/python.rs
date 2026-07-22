//! Python symbol extraction (F-002). References/calls added later.

use tree_sitter::Node;

use super::{field_text, node_lines, symbol_id};
use crate::code_graph::{CodeLanguage, Ref, Symbol, SymbolKind};

pub(super) fn extract(root: Node, source: &str, path: &str) -> (Vec<Symbol>, Vec<Ref>) {
    let mut symbols = Vec::new();
    walk(root, source, path, None, false, &mut symbols);
    (symbols, Vec::new())
}

fn walk(
    node: Node,
    source: &str,
    path: &str,
    parent: Option<String>,
    in_class: bool,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = match child.kind() {
            "function_definition" => Some(if in_class {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }),
            "class_definition" => Some(SymbolKind::Class),
            _ => None,
        };
        match kind {
            Some(sym_kind) => {
                let Some(name) = field_text(&child, "name", source) else {
                    walk(child, source, path, parent.clone(), in_class, out);
                    continue;
                };
                let (start_line, end_line) = node_lines(&child);
                let id = symbol_id(path, &name, start_line);
                out.push(Symbol {
                    id: id.clone(),
                    file: path.to_string(),
                    name,
                    kind: sym_kind,
                    language: CodeLanguage::Python,
                    start_line,
                    end_line,
                    parent: parent.clone(),
                });
                // Descend; functions nested in a class body are methods.
                let child_in_class = matches!(sym_kind, SymbolKind::Class);
                walk(child, source, path, Some(id), child_in_class, out);
            }
            None => walk(child, source, path, parent.clone(), in_class, out),
        }
    }
}
