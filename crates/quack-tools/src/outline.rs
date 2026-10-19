//! File outlines: definitions with their line ranges, nested, from tree-sitter and each
//! grammar's `tags.scm` (docs/plan/05-context-management.md#index-tool-maki-style-skeleton).
//!
//! Grammars are compiled in behind the `lang-common` feature. Without it, or for a
//! language it lacks, there is no outline and callers fall back to plain reads.

use std::path::Path;

/// One definition: a function, type, module, `impl` block, …
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// The grammar's word for it: `function`, `class`, `method`, `module`, `impl`, …
    pub kind: String,
    /// 1-based, inclusive.
    pub start: usize,
    pub end: usize,
    /// The definition's first line, trimmed and shortened.
    pub signature: String,
    /// How many definitions enclose this one.
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    pub language: &'static str,
    pub lines: usize,
    pub symbols: Vec<Symbol>,
}

impl Outline {
    /// The outline as the model sees it:
    ///
    /// ```text
    /// src/session.rs (412 lines, rust)
    ///   14-31    pub struct Session {
    ///   33-180   impl Session
    ///     35-60    pub fn new(cfg: &Config) -> Result<Self>
    /// ```
    pub fn render(&self, shown: &str) -> String {
        let mut out = format!("{shown} ({} lines, {})\n", self.lines, self.language);
        let width = self.lines.to_string().len() * 2 + 1;
        for s in &self.symbols {
            let indent = "  ".repeat(s.depth + 1);
            let range = format!("{}-{}", s.start, s.end);
            out.push_str(&format!("{indent}{range:<width$}  {}\n", s.signature));
        }
        out
    }

    /// Top-level symbols on one line, for directory listings.
    pub fn summary(&self, max_chars: usize) -> String {
        let mut out = String::new();
        for s in self.symbols.iter().filter(|s| s.depth == 0) {
            let item = format!("{} {}", s.kind_word(), s.name);
            if out.len() + item.len() + 2 > max_chars {
                out.push_str(", …");
                break;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&item);
        }
        out
    }

    /// The symbol a name points at: `name`, or `Parent::name` / `Parent.name` for one
    /// inside another. The innermost match wins.
    pub fn find(&self, query: &str) -> Option<&Symbol> {
        let parts: Vec<&str> = query.split([':', '.']).filter(|p| !p.is_empty()).collect();
        let (last, parents) = parts.split_last()?;
        let candidates = self
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name == *last);
        candidates
            .filter(|(i, s)| {
                // Each named parent must enclose it, innermost last.
                let mut need = parents.iter().rev();
                let mut next = need.next();
                for p in self.symbols[..*i].iter().rev() {
                    let Some(want) = next else { break };
                    if p.start <= s.start && p.end >= s.end && p.name == *want {
                        next = need.next();
                    }
                }
                next.is_none()
            })
            .map(|(_, s)| s)
            .max_by_key(|s| s.depth)
    }
}

impl Symbol {
    fn kind_word(&self) -> &str {
        match self.kind.as_str() {
            "function" | "method" => "fn",
            "class" => "type",
            "interface" => "trait",
            "module" => "mod",
            other => other,
        }
    }
}

/// The language for a path, by extension.
pub fn language_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        _ => return None,
    })
}

/// Whether this build can outline `path` (its language, and grammars compiled in).
pub fn supported(path: &Path) -> bool {
    cfg!(feature = "lang-common") && language_of(path).is_some()
}

/// The outline of `text`, if its language has a grammar in this build.
pub fn outline(path: &Path, text: &str) -> Option<Outline> {
    let language = language_of(path)?;
    let mut symbols = imp::symbols(language, text)?;
    symbols.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    // Several patterns can match one definition.
    symbols.dedup_by(|b, a| a.start == b.start && a.end == b.end && a.name == b.name);
    // Depth: how many earlier symbols enclose each one.
    let mut open: Vec<usize> = Vec::new();
    for s in &mut symbols {
        // Sorted by start, longest first: the top encloses this one unless it ends sooner.
        while open.last().is_some_and(|&end| end < s.end) {
            open.pop();
        }
        s.depth = open.len();
        open.push(s.end);
    }
    Some(Outline {
        language,
        lines: text.lines().count(),
        symbols,
    })
}

#[cfg(feature = "lang-common")]
mod imp {
    use std::sync::OnceLock;

    use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

    use super::Symbol;

    const SIGNATURE_CHARS: usize = 100;

    /// A definition's first line, trimmed and shortened.
    fn signature(text: &str, start_byte: usize) -> String {
        let line = text[start_byte..].lines().next().unwrap_or_default().trim();
        if line.chars().count() > SIGNATURE_CHARS {
            let cut: String = line.chars().take(SIGNATURE_CHARS).collect();
            format!("{cut}…")
        } else {
            line.to_owned()
        }
    }

    /// `impl` blocks are references in the Rust query; they hold the methods, so they are
    /// shown as definitions.
    const RUST_EXTRA: &str = "(impl_item type: (_) @name) @definition.impl\n";

    struct Grammar {
        language: Language,
        query: Query,
    }

    fn build(language: Language, queries: &[&str]) -> Option<Grammar> {
        // A query that does not fit the grammar is dropped, not fatal.
        let src = queries.concat();
        let query = Query::new(&language, &src)
            .or_else(|_| Query::new(&language, queries.last().copied().unwrap_or_default()))
            .map_err(|e| tracing::warn!(error = %e, "tags query does not compile"))
            .ok()?;
        Some(Grammar { language, query })
    }

    fn grammar(name: &str) -> Option<&'static Grammar> {
        macro_rules! cached {
            ($build:expr) => {{
                static G: OnceLock<Option<Grammar>> = OnceLock::new();
                G.get_or_init(|| $build).as_ref()
            }};
        }
        match name {
            "rust" => cached!(build(
                tree_sitter_rust::LANGUAGE.into(),
                &[tree_sitter_rust::TAGS_QUERY, RUST_EXTRA]
            )),
            "python" => cached!(build(
                tree_sitter_python::LANGUAGE.into(),
                &[tree_sitter_python::TAGS_QUERY]
            )),
            "javascript" => cached!(build(
                tree_sitter_javascript::LANGUAGE.into(),
                &[tree_sitter_javascript::TAGS_QUERY]
            )),
            "typescript" => cached!(build(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &[
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                ]
            )),
            "tsx" => cached!(build(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &[
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                ]
            )),
            "go" => cached!(build(
                tree_sitter_go::LANGUAGE.into(),
                &[tree_sitter_go::TAGS_QUERY]
            )),
            _ => None,
        }
    }

    pub(super) fn symbols(language: &str, text: &str) -> Option<Vec<Symbol>> {
        let g = grammar(language)?;
        let mut parser = Parser::new();
        parser.set_language(&g.language).ok()?;
        let tree = parser.parse(text, None)?;
        let names = g.query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&g.query, tree.root_node(), text.as_bytes());
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            let mut def = None;
            let mut name = None;
            for c in m.captures() {
                let capture = names[c.index as usize];
                if capture == "name" {
                    name = c.node.utf8_text(text.as_bytes()).ok();
                } else if let Some(kind) = capture
                    .strip_prefix("definition.")
                    .or_else(|| (capture == "reference.implementation").then_some("impl"))
                {
                    def = Some((kind, c.node));
                }
            }
            let (Some((kind, node)), Some(name)) = (def, name) else {
                continue;
            };
            out.push(Symbol {
                name: name.to_owned(),
                kind: kind.to_owned(),
                start: node.start_position().row + 1,
                end: node.end_position().row + 1,
                signature: signature(text, node.start_byte()),
                depth: 0,
            });
        }
        Some(out)
    }
}

#[cfg(not(feature = "lang-common"))]
mod imp {
    use super::Symbol;

    pub(super) fn symbols(_language: &str, _text: &str) -> Option<Vec<Symbol>> {
        None
    }
}

#[cfg(all(test, feature = "lang-common"))]
mod tests {
    use super::*;

    const RUST: &str = "use std::io;\n\npub struct Session {\n    id: u32,\n}\n\nimpl Session {\n    pub fn new() -> Self {\n        Self { id: 0 }\n    }\n\n    fn run(&self) {}\n}\n\nmod tests {\n    fn t() {}\n}\n";

    #[test]
    fn rust_outline_nests_methods_in_impls() {
        let o = outline(Path::new("src/s.rs"), RUST).expect("outline");
        let rows: Vec<(usize, usize, &str, usize)> = o
            .symbols
            .iter()
            .map(|s| (s.start, s.end, s.name.as_str(), s.depth))
            .collect();
        assert_eq!(
            rows,
            [
                (3, 5, "Session", 0),
                (7, 13, "Session", 0),
                (8, 10, "new", 1),
                (12, 12, "run", 1),
                (15, 17, "tests", 0),
                (16, 16, "t", 1),
            ]
        );
        let text = o.render("src/s.rs");
        assert!(text.starts_with("src/s.rs (17 lines, rust)\n"), "{text}");
        assert!(text.contains("    8-10   pub fn new() -> Self {"), "{text}");
        assert_eq!(o.find("Session::run").map(|s| s.start), Some(12));
        assert_eq!(o.find("tests.t").map(|s| s.start), Some(16));
        assert_eq!(o.find("nope"), None);
        assert_eq!(o.summary(200), "type Session, impl Session, mod tests");
    }

    #[test]
    fn other_languages_parse() {
        let py = "class A:\n    def f(self):\n        pass\n\ndef g():\n    return 1\n";
        let o = outline(Path::new("a.py"), py).expect("python");
        assert_eq!(o.find("A.f").map(|s| (s.start, s.depth)), Some((2, 1)));
        let go = "package main\n\nfunc main() {\n}\n\ntype T struct{}\n\nfunc (t T) M() {}\n";
        let o = outline(Path::new("a.go"), go).expect("go");
        assert!(o.find("main").is_some() && o.find("M").is_some(), "{o:?}");
        let ts =
            "export class C {\n  m(): void {}\n}\ninterface I { x: number }\nfunction f() {}\n";
        let o = outline(Path::new("a.ts"), ts).expect("ts");
        assert!(o.find("C.m").is_some() && o.find("f").is_some(), "{o:?}");
        let js = "function a() {}\nconst b = () => 1;\nclass K { z() {} }\n";
        let o = outline(Path::new("a.js"), js).expect("js");
        assert!(o.find("a").is_some() && o.find("K.z").is_some(), "{o:?}");
        assert!(outline(Path::new("a.txt"), "x").is_none());
    }
}
