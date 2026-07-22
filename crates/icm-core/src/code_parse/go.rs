//! Go symbol extraction (F-002). References/calls added later.

use tree_sitter::Node;

use super::{field_text, node_lines, symbol_id};
use crate::code_graph::{CodeLanguage, Ref, Symbol, SymbolKind};

pub(super) fn extract(root: Node, source: &str, path: &str) -> (Vec<Symbol>, Vec<Ref>) {
    let mut symbols = Vec::new();
    walk(root, source, path, &mut symbols);
    (symbols, Vec::new())
}

fn push(node: &Node, name: String, kind: SymbolKind, path: &str, out: &mut Vec<Symbol>) {
    let (start_line, end_line) = node_lines(node);
    out.push(Symbol {
        id: symbol_id(path, &name, start_line),
        file: path.to_string(),
        name,
        kind,
        language: CodeLanguage::Go,
        start_line,
        end_line,
        parent: None,
    });
}

fn walk(node: Node, source: &str, path: &str, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name) = field_text(&child, "name", source) {
                    push(&child, name, SymbolKind::Function, path, out);
                }
            }
            "method_declaration" => {
                if let Some(name) = field_text(&child, "name", source) {
                    push(&child, name, SymbolKind::Method, path, out);
                }
            }
            // `type_declaration` wraps one or more `type_spec` nodes.
            "type_declaration" => {
                let mut tc = child.walk();
                for spec in child.children(&mut tc) {
                    if spec.kind() == "type_spec" {
                        if let Some(name) = field_text(&spec, "name", source) {
                            let kind = match spec.child_by_field_name("type").map(|t| t.kind()) {
                                Some("struct_type") => SymbolKind::Struct,
                                Some("interface_type") => SymbolKind::Interface,
                                _ => SymbolKind::TypeAlias,
                            };
                            push(&spec, name, kind, path, out);
                        }
                    }
                }
            }
            _ => walk(child, source, path, out),
        }
    }
}
