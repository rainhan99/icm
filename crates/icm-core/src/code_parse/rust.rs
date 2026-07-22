//! Rust symbol extraction (F-002). References/calls are added in a later
//! task; this module produces definitions.

use tree_sitter::Node;

use super::{field_text, node_lines, symbol_id};
use crate::code_graph::{CodeLanguage, Ref, Symbol, SymbolKind};

/// Extract Rust symbols from a parsed tree.
pub(super) fn extract(root: Node, source: &str, path: &str) -> (Vec<Symbol>, Vec<Ref>) {
    let mut symbols = Vec::new();
    walk(root, source, path, None, false, &mut symbols);
    (symbols, Vec::new())
}

/// Map a Rust node kind to a symbol kind. `in_impl` turns `fn` into a
/// method. Returns `None` for nodes that are not themselves definitions
/// (we still recurse into them).
fn classify(kind: &str, in_impl: bool) -> Option<SymbolKind> {
    Some(match kind {
        "function_item" | "function_signature_item" => {
            if in_impl {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }
        }
        "struct_item" | "union_item" => SymbolKind::Struct,
        "enum_item" => SymbolKind::Enum,
        "trait_item" => SymbolKind::Trait,
        "type_item" => SymbolKind::TypeAlias,
        "const_item" | "static_item" => SymbolKind::Constant,
        "mod_item" => SymbolKind::Module,
        "macro_definition" => SymbolKind::Function,
        _ => return None,
    })
}

fn walk(
    node: Node,
    source: &str,
    path: &str,
    parent: Option<String>,
    in_impl: bool,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind_str = child.kind();
        // `impl_item` has no name; recurse so its methods become Methods.
        if kind_str == "impl_item" {
            walk(child, source, path, parent.clone(), true, out);
            continue;
        }
        match classify(kind_str, in_impl) {
            Some(sym_kind) => {
                let Some(name) = field_text(&child, "name", source) else {
                    // Unnamed def node — recurse into it anyway.
                    walk(child, source, path, parent.clone(), in_impl, out);
                    continue;
                };
                let (start_line, end_line) = node_lines(&child);
                let id = symbol_id(path, &name, start_line);
                out.push(Symbol {
                    id: id.clone(),
                    file: path.to_string(),
                    name,
                    kind: sym_kind,
                    language: CodeLanguage::Rust,
                    start_line,
                    end_line,
                    parent: parent.clone(),
                });
                // Recurse into the body; nested items get this as parent.
                // Entering an impl is handled above; other containers
                // (mod/trait) keep in_impl for their fn children only when
                // it's a trait (trait methods are methods).
                let child_in_impl = matches!(sym_kind, SymbolKind::Trait);
                walk(child, source, path, Some(id), child_in_impl, out);
            }
            None => walk(child, source, path, parent.clone(), in_impl, out),
        }
    }
}
