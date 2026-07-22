//! Cross-file reference resolution (F-002, feature `code-graph`).
//!
//! Links unresolved call/reference edges to their target definitions
//! using a global name index plus lexical-scope heuristics. Precision
//! boundary (authorized, see plan Manifest): scope + uniqueness + a light
//! enclosing-type preference for methods; NO full type inference, so
//! dynamic dispatch / duck typing on dynamic languages stays
//! `unresolved` rather than being mis-linked.
//!
//! Resolution is a graph-level pass over all indexed symbols (call it
//! after parsing every file, or a working set of files).

use std::collections::HashMap;

use crate::code_graph::{Ref, Symbol};

/// Set `target_symbol` on each ref where it can be resolved against
/// `all_symbols`. Rules, in order:
/// 1. exactly one symbol with the target name → resolve;
/// 2. otherwise prefer a candidate in the **same file** as the caller;
/// 3. otherwise prefer a candidate sharing the caller's enclosing type
///    (light type-context heuristic for methods);
/// 4. otherwise leave `None` (ambiguous / external / dynamic dispatch).
pub fn resolve_refs(all_symbols: &[Symbol], refs: &mut [Ref]) {
    let mut by_name: HashMap<&str, Vec<&Symbol>> = HashMap::new();
    for s in all_symbols {
        by_name.entry(s.name.as_str()).or_default().push(s);
    }
    let by_id: HashMap<&str, &Symbol> =
        all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

    for r in refs.iter_mut() {
        let Some(cands) = by_name.get(r.target_name.as_str()) else {
            continue; // no definition known → unresolved
        };
        if cands.len() == 1 {
            r.target_symbol = Some(cands[0].id.clone());
            continue;
        }
        let caller = by_id.get(r.from_symbol.as_str());
        let caller_file = caller.map(|s| s.file.as_str());
        let caller_parent = caller.and_then(|s| s.parent.as_deref());

        // (2) same-file preference.
        let same_file: Vec<&&Symbol> = cands
            .iter()
            .filter(|c| Some(c.file.as_str()) == caller_file)
            .collect();
        if same_file.len() == 1 {
            r.target_symbol = Some(same_file[0].id.clone());
            continue;
        }

        // (3) same enclosing-type preference (methods).
        if let Some(parent) = caller_parent {
            let same_parent: Vec<&&Symbol> = cands
                .iter()
                .filter(|c| c.parent.as_deref() == Some(parent))
                .collect();
            if same_parent.len() == 1 {
                r.target_symbol = Some(same_parent[0].id.clone());
                continue;
            }
        }
        // (4) ambiguous → leave unresolved.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_parse::parse_file;
    use crate::code_graph::CodeLanguage;

    #[test]
    fn cross_file_call_resolves_to_unique_definition() {
        let a = parse_file(CodeLanguage::Rust, "a.rs", "fn a(){ helper(); }").unwrap();
        let b = parse_file(CodeLanguage::Rust, "b.rs", "fn helper(){}").unwrap();
        let mut all = a.symbols.clone();
        all.extend(b.symbols.clone());
        let mut refs = a.refs.clone();
        resolve_refs(&all, &mut refs);
        let helper_id = b.symbols.iter().find(|s| s.name == "helper").unwrap().id.clone();
        let call = refs.iter().find(|r| r.target_name == "helper").unwrap();
        assert_eq!(call.target_symbol.as_deref(), Some(helper_id.as_str()));
    }

    #[test]
    fn dynamic_dispatch_stays_unresolved() {
        // `obj.m()` with no definition of `m` → unresolved (no mis-link).
        let p = parse_file(CodeLanguage::Python, "p.py", "def f():\n    obj.m()\n").unwrap();
        let mut refs = p.refs.clone();
        resolve_refs(&p.symbols, &mut refs);
        let m = refs.iter().find(|r| r.target_name == "m").unwrap();
        assert!(m.target_symbol.is_none());
    }
}
