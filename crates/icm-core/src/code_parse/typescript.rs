//! TypeScript / JavaScript symbol extraction (F-002). One grammar family
//! (tree-sitter-typescript, TSX superset) serves both. References/calls
//! are added in a later task.

use tree_sitter::Node;

use super::{field_text, node_lines, symbol_id};
use crate::code_graph::{CodeLanguage, Ref, Symbol, SymbolKind};

pub(super) fn extract(
    root: Node,
    source: &str,
    path: &str,
    language: CodeLanguage,
) -> (Vec<Symbol>, Vec<Ref>) {
    let mut symbols = Vec::new();
    walk(root, source, path, language, None, &mut symbols);
    (symbols, Vec::new())
}

fn classify(kind: &str) -> Option<SymbolKind> {
    Some(match kind {
        "function_declaration" | "generator_function_declaration" => SymbolKind::Function,
        "class_declaration" | "abstract_class_declaration" => SymbolKind::Class,
        "method_definition" => SymbolKind::Method,
        "interface_declaration" => SymbolKind::Interface,
        "enum_declaration" => SymbolKind::Enum,
        "type_alias_declaration" => SymbolKind::TypeAlias,
        _ => return None,
    })
}

fn push_symbol(
    node: &Node,
    name: String,
    kind: SymbolKind,
    source_path: &str,
    language: CodeLanguage,
    parent: &Option<String>,
    out: &mut Vec<Symbol>,
) -> String {
    let (start_line, end_line) = node_lines(node);
    let id = symbol_id(source_path, &name, start_line);
    out.push(Symbol {
        id: id.clone(),
        file: source_path.to_string(),
        name,
        kind,
        language,
        start_line,
        end_line,
        parent: parent.clone(),
    });
    id
}

fn walk(
    node: Node,
    source: &str,
    path: &str,
    language: CodeLanguage,
    parent: Option<String>,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // `const foo = () => {}` / `const foo = function(){}` → Function.
        if child.kind() == "lexical_declaration" || child.kind() == "variable_declaration" {
            let mut vc = child.walk();
            for decl in child.children(&mut vc) {
                if decl.kind() == "variable_declarator" {
                    let is_fn = decl
                        .child_by_field_name("value")
                        .map(|v| {
                            matches!(
                                v.kind(),
                                "arrow_function" | "function" | "function_expression"
                            )
                        })
                        .unwrap_or(false);
                    if is_fn {
                        if let Some(name) = field_text(&decl, "name", source) {
                            push_symbol(
                                &decl,
                                name,
                                SymbolKind::Function,
                                path,
                                language,
                                &parent,
                                out,
                            );
                        }
                    }
                }
            }
            continue;
        }
        match classify(child.kind()) {
            Some(kind) => {
                let Some(name) = field_text(&child, "name", source) else {
                    walk(child, source, path, language, parent.clone(), out);
                    continue;
                };
                let id = push_symbol(&child, name, kind, path, language, &parent, out);
                walk(child, source, path, language, Some(id), out);
            }
            None => walk(child, source, path, language, parent.clone(), out),
        }
    }
}
