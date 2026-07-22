//! Code-graph indexing (F-002, feature `code-graph`).
//!
//! Walks a repository (respecting `.gitignore` via ripgrep's `ignore`
//! walker, plus a hard exclude-list of build dirs), parses each supported
//! file, resolves references across the whole symbol set, and writes the
//! graph to the active [`Store`]. Paths are stored **repository-relative**
//! so the graph is portable across machines (F-002 remote-share key).

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use icm_core::{code_parse, code_resolve, CodeFile, CodeGraphStore, CodeLanguage, Ref, Symbol};
use icm_store::Store;

/// `icm code <...>` subcommands (F-002).
#[derive(Subcommand, Debug)]
pub enum CodeCommand {
    /// Index (or re-index) a repository into the code graph.
    Index {
        /// Repository root to index (default: current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Only re-parse files whose content changed since last index.
        #[arg(long)]
        incremental: bool,
    },
    /// Explore a symbol: definition, callers, callees, blast radius.
    Explore {
        symbol: String,
        #[arg(long, default_value = "3")]
        depth: usize,
    },
    /// List the callers of a symbol.
    Callers { symbol: String },
    /// Show the transitive change blast-radius of a symbol.
    Impact {
        symbol: String,
        #[arg(long, default_value = "5")]
        depth: usize,
    },
    /// Show code-graph statistics.
    Stats,
}

fn fmt_syms(syms: &[Symbol]) -> String {
    if syms.is_empty() {
        "(none)".to_string()
    } else {
        syms.iter()
            .map(|s| format!("  {} — {}:{}", s.name, s.file, s.start_line))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Read the verbatim source of a symbol from disk (best-effort), relative
/// to `root`. Returns `None` if the file can't be read.
fn read_source(root: &Path, sym: &Symbol) -> Option<String> {
    let text = std::fs::read_to_string(root.join(&sym.file)).ok()?;
    let start = sym.start_line.saturating_sub(1) as usize;
    let end = (sym.end_line as usize).min(text.lines().count());
    Some(
        text.lines()
            .skip(start)
            .take(end - start)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Execute a `code` subcommand against `store` (root = current dir for
/// source slicing).
pub fn run(cmd: &CodeCommand, store: &Store) -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    match cmd {
        CodeCommand::Index { path, incremental } => {
            let report = index_path(store, Path::new(path), *incremental)?;
            println!(
                "code graph: indexed {} file(s), {} symbol(s), skipped {}",
                report.files_indexed, report.symbols, report.files_skipped
            );
        }
        CodeCommand::Explore { symbol, depth } => match store.explore(symbol, *depth)? {
            Some(res) => {
                println!(
                    "{:?} {} @ {}:{}-{}",
                    res.symbol.kind,
                    res.symbol.name,
                    res.symbol.file,
                    res.symbol.start_line,
                    res.symbol.end_line
                );
                println!(
                    "\ncallers ({}):\n{}",
                    res.callers.len(),
                    fmt_syms(&res.callers)
                );
                println!(
                    "\ncallees ({}):\n{}",
                    res.callees.len(),
                    fmt_syms(&res.callees)
                );
                println!(
                    "\nblast radius ({}):\n{}",
                    res.blast_radius.len(),
                    fmt_syms(&res.blast_radius)
                );
                if let Some(src) = read_source(&root, &res.symbol) {
                    println!("\nsource:\n{src}");
                }
            }
            None => println!("no symbol named {symbol:?} — run `icm code index` first"),
        },
        CodeCommand::Callers { symbol } => {
            match store.find_symbols(symbol, 1)?.into_iter().next() {
                Some(s) => {
                    let callers = store.callers(&s.id)?;
                    println!(
                        "callers of {} ({}):\n{}",
                        s.name,
                        callers.len(),
                        fmt_syms(&callers)
                    );
                }
                None => println!("no symbol named {symbol:?}"),
            }
        }
        CodeCommand::Impact { symbol, depth } => match store.explore(symbol, *depth)? {
            Some(res) => println!(
                "impact of {} — {} symbol(s):\n{}",
                res.symbol.name,
                res.blast_radius.len(),
                fmt_syms(&res.blast_radius)
            ),
            None => println!("no symbol named {symbol:?}"),
        },
        CodeCommand::Stats => {
            let s = store.code_stats()?;
            println!(
                "files: {}\nsymbols: {}\nrefs: {}\nstale: {}",
                s.files, s.symbols, s.refs, s.stale_files
            );
            for (lang, n) in &s.by_language {
                println!("  {lang}: {n}");
            }
        }
    }
    Ok(())
}

/// Directories always skipped even if not `.gitignore`d (build artifacts,
/// vendored deps) so indexing stays fast and relevant.
const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    ".git",
];

/// Summary of an index run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub symbols: usize,
}

fn content_hash(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

struct Parsed {
    file: CodeFile,
    symbols: Vec<Symbol>,
    refs: Vec<Ref>,
    /// Whether this file's content changed vs the stored hash (always
    /// true for a full index).
    changed: bool,
}

/// Index all supported files under `root` into `store`. When
/// `incremental` is true, files whose content hash already matches the
/// stored hash are skipped.
pub fn index_path(store: &Store, root: &Path, incremental: bool) -> Result<IndexReport> {
    // Pass 1 — walk + parse. Accumulate every symbol for cross-file
    // resolution, and keep per-file parse results for storage.
    let mut parsed: Vec<Parsed> = Vec::new();
    let mut all_symbols: Vec<Symbol> = Vec::new();
    let mut skipped = 0usize;

    for result in WalkBuilder::new(root)
        .standard_filters(true) // hidden + .gitignore + .ignore
        .require_git(false) // apply .gitignore even outside a git repo
        .build()
    {
        let Ok(entry) = result else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|c| EXCLUDED_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
        {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(language) = CodeLanguage::from_extension(ext) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(source) = std::fs::read_to_string(path) else {
            skipped += 1;
            continue;
        };
        let hash = content_hash(&source);
        // Parse every file (so cross-file resolution stays correct), but
        // remember which ones actually changed so incremental runs only
        // WRITE the changed files.
        let changed = if incremental {
            store.file_hash(&rel).ok().flatten().as_deref() != Some(hash.as_str())
        } else {
            true
        };
        let pf = match code_parse::parse_file(language, &rel, &source) {
            Ok(p) => p,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        all_symbols.extend(pf.symbols.iter().cloned());
        parsed.push(Parsed {
            file: CodeFile {
                path: rel,
                language,
                content_hash: hash,
                stale: false,
            },
            symbols: pf.symbols,
            refs: pf.refs,
            changed,
        });
    }

    // Pass 2 — resolve references against the full symbol set, then store
    // only the changed files (index_file also clears their stale flag).
    let mut symbols = 0usize;
    let mut indexed = 0usize;
    for p in &mut parsed {
        if !p.changed {
            skipped += 1;
            continue;
        }
        code_resolve::resolve_refs(&all_symbols, &mut p.refs);
        store.index_file(&p.file, &p.symbols, &p.refs)?;
        symbols += p.symbols.len();
        indexed += 1;
    }

    Ok(IndexReport {
        files_indexed: indexed,
        files_skipped: skipped,
        symbols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use icm_store::Store;
    use std::fs;

    #[test]
    fn code_index_full_indexes_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn foo(){ bar(); }\nfn bar(){}").unwrap();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/skip.js"), "function nope(){}").unwrap();
        fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(root.join("ignored.rs"), "fn secret(){}").unwrap();

        let store = Store::in_memory().unwrap();
        let report = index_path(&store, root, false).unwrap();

        assert!(
            report.symbols >= 2,
            "indexed foo + bar, got {}",
            report.symbols
        );
        assert_eq!(store.find_symbols("foo", 5).unwrap().len(), 1);
        assert_eq!(store.find_symbols("bar", 5).unwrap().len(), 1);
        // node_modules excluded, .gitignore'd file excluded.
        assert!(
            store.find_symbols("nope", 5).unwrap().is_empty(),
            "node_modules excluded"
        );
        assert!(
            store.find_symbols("secret", 5).unwrap().is_empty(),
            "gitignore excluded"
        );

        // The call foo -> bar resolved to a real edge.
        let bar = store
            .find_symbols("bar", 5)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(store.callers(&bar.id).unwrap().len(), 1, "foo calls bar");
    }

    #[test]
    fn code_cli_subcommands_run() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn foo(){ bar(); }\nfn bar(){}").unwrap();
        let store = Store::in_memory().unwrap();
        run(
            &CodeCommand::Index {
                path: dir.path().to_string_lossy().to_string(),
                incremental: false,
            },
            &store,
        )
        .unwrap();
        // Index populated the graph.
        assert!(store.code_stats().unwrap().symbols >= 2);
        // Every subcommand runs without error.
        run(&CodeCommand::Stats, &store).unwrap();
        run(
            &CodeCommand::Explore {
                symbol: "bar".into(),
                depth: 3,
            },
            &store,
        )
        .unwrap();
        run(
            &CodeCommand::Callers {
                symbol: "bar".into(),
            },
            &store,
        )
        .unwrap();
        run(
            &CodeCommand::Impact {
                symbol: "bar".into(),
                depth: 5,
            },
            &store,
        )
        .unwrap();
    }

    #[test]
    fn incremental_reindexes_changed_only_and_clears_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn foo(){}").unwrap();
        fs::write(root.join("b.rs"), "fn keep(){}").unwrap();
        let store = Store::in_memory().unwrap();
        index_path(&store, root, false).unwrap();

        // Edit only a.rs; mark it stale (simulating the hook).
        fs::write(root.join("a.rs"), "fn qux(){}").unwrap();
        store.mark_stale(&["a.rs".to_string()]).unwrap();
        assert_eq!(store.list_stale().unwrap(), vec!["a.rs".to_string()]);

        let report = index_path(&store, root, true).unwrap();
        assert_eq!(report.files_indexed, 1, "only a.rs re-stored");
        assert_eq!(report.files_skipped, 1, "b.rs unchanged, skipped");
        // a.rs symbols updated; b.rs untouched.
        assert!(
            store.find_symbols("foo", 5).unwrap().is_empty(),
            "old foo gone"
        );
        assert_eq!(store.find_symbols("qux", 5).unwrap().len(), 1);
        assert_eq!(store.find_symbols("keep", 5).unwrap().len(), 1);
        // Re-index cleared a.rs's stale flag.
        assert!(
            store.list_stale().unwrap().is_empty(),
            "stale cleared after reindex"
        );
    }
}
