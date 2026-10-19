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
    /// inside another (a C++ `Parent::name` defined outside its class counts too). The
    /// longest match wins, so a definition beats its prototype, then the innermost.
    pub fn find(&self, query: &str) -> Option<&Symbol> {
        let parts: Vec<&str> = query.split([':', '.']).filter(|p| !p.is_empty()).collect();
        let (last, parents) = parts.split_last()?;
        self.symbols
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                // Its path: the names of what encloses it, outermost first, then its own.
                let mut path: Vec<&str> = self.symbols[..*i]
                    .iter()
                    .filter(|p| p.start <= s.start && p.end >= s.end)
                    .flat_map(|p| p.name.split("::"))
                    .collect();
                path.extend(s.name.split("::"));
                let Some((own, above)) = path.split_last() else {
                    return false;
                };
                // Each named parent must be on the path, in order.
                let mut above = above.iter();
                own == last && parents.iter().all(|p| above.any(|a| a == p))
            })
            .map(|(_, s)| s)
            .max_by_key(|s| (s.end - s.start, s.depth))
    }
}

impl Symbol {
    fn kind_word(&self) -> &str {
        match self.kind.as_str() {
            "function" | "method" => "fn",
            "class" | "struct" | "union" | "enum" => "type",
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
        "c" => "c",
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "tpp" | "inl" => {
            "cpp"
        }
        // C or C++: the C++ grammar reads both.
        "h" => "c/c++",
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

    /// A definition's first line, trimmed and shortened. When the name is not on it (a
    /// return type or macro on its own line, `typedef struct {`), the lines up to the
    /// name's, or the first and the name's line around `…` when the name is further down.
    fn signature(text: &str, start: usize, name: std::ops::Range<usize>) -> String {
        let first = text[start..].lines().next().unwrap_or_default();
        let line = if name.start < start + first.len() {
            first.trim().to_owned()
        } else {
            let line_end = text[name.end..]
                .find('\n')
                .map_or(text.len(), |i| name.end + i);
            let span = &text[start..line_end];
            if span.lines().count() <= 3 {
                span.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                let name_line = span.lines().last().unwrap_or_default();
                format!("{} … {}", first.trim(), name_line.trim())
            }
        };
        if line.chars().count() > SIGNATURE_CHARS {
            let cut: String = line.chars().take(SIGNATURE_CHARS).collect();
            format!("{cut}…")
        } else {
            line
        }
    }

    /// `impl` blocks are references in the Rust query; they hold the methods, so they are
    /// shown as definitions.
    const RUST_EXTRA: &str = "(impl_item type: (_) @name) @definition.impl\n";

    // The C and C++ grammars' own `tags.scm` capture a function's declarator (its name and
    // parameters) rather than the definition, so every function would be one line long;
    // these capture the whole definition, and add namespaces, methods and macros.

    /// Definitions shared by C and C++.
    const C_COMMON: &str = r#"
(struct_specifier name: (type_identifier) @name body: (_)) @definition.struct
(union_specifier name: (type_identifier) @name body: (_)) @definition.union
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
(type_definition declarator: (type_identifier) @name) @definition.type
(preproc_function_def name: (identifier) @name) @definition.macro
"#;

    const C_TAGS: &str = r#"
(function_definition declarator: [
  (function_declarator declarator: (identifier) @name)
  (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))
  (pointer_declarator declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name)))
]) @definition.function
"#;

    /// C++: also used for `.h`, so prototypes count (a header is mostly prototypes).
    /// `Foo::bar` defined outside its class keeps its qualified name.
    const CPP_TAGS: &str = r#"
(class_specifier name: (type_identifier) @name body: (_)) @definition.class
(namespace_definition name: [(namespace_identifier) (nested_namespace_specifier)] @name) @definition.module
(function_definition declarator: [
  (function_declarator declarator: [(identifier) (field_identifier) (destructor_name) (operator_name)] @name)
  (pointer_declarator declarator: (function_declarator declarator: [(identifier) (field_identifier)] @name))
  (reference_declarator (function_declarator declarator: [(identifier) (field_identifier) (operator_name)] @name))
]) @definition.function
(function_definition declarator: [
  (function_declarator declarator: (qualified_identifier) @name)
  (pointer_declarator declarator: (function_declarator declarator: (qualified_identifier) @name))
  (reference_declarator (function_declarator declarator: (qualified_identifier) @name))
]) @definition.method
(declaration declarator: [
  (function_declarator declarator: (identifier) @name)
  (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))
]) @definition.function
(field_declaration declarator: [
  (function_declarator declarator: [(field_identifier) (destructor_name) (operator_name)] @name)
  (pointer_declarator declarator: (function_declarator declarator: (field_identifier) @name))
  (reference_declarator (function_declarator declarator: [(field_identifier) (operator_name)] @name))
]) @definition.method
(declaration declarator: (function_declarator declarator: (destructor_name) @name)) @definition.method
"#;

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

    /// Each language with a grammar: its parser and its definition queries.
    const LANGUAGES: [&str; 8] = [
        "rust",
        "python",
        "javascript",
        "typescript",
        "tsx",
        "go",
        "c",
        "cpp",
    ];

    fn sources(name: &str) -> Option<(Language, &'static [&'static str])> {
        Some(match name {
            "rust" => (
                tree_sitter_rust::LANGUAGE.into(),
                &[tree_sitter_rust::TAGS_QUERY, RUST_EXTRA],
            ),
            "python" => (
                tree_sitter_python::LANGUAGE.into(),
                &[tree_sitter_python::TAGS_QUERY],
            ),
            "javascript" => (
                tree_sitter_javascript::LANGUAGE.into(),
                &[tree_sitter_javascript::TAGS_QUERY],
            ),
            "typescript" => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &[
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY,
                ],
            ),
            "tsx" => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &[
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY,
                ],
            ),
            "go" => (
                tree_sitter_go::LANGUAGE.into(),
                &[tree_sitter_go::TAGS_QUERY],
            ),
            "c" => (tree_sitter_c::LANGUAGE.into(), &[C_COMMON, C_TAGS]),
            "cpp" => (tree_sitter_cpp::LANGUAGE.into(), &[C_COMMON, CPP_TAGS]),
            _ => return None,
        })
    }

    fn grammar(name: &str) -> Option<&'static Grammar> {
        static BUILT: [OnceLock<Option<Grammar>>; LANGUAGES.len()] =
            [const { OnceLock::new() }; LANGUAGES.len()];
        let name = if name == "c/c++" { "cpp" } else { name };
        let i = LANGUAGES.iter().position(|l| *l == name)?;
        BUILT[i]
            .get_or_init(|| {
                let (language, queries) = sources(name)?;
                build(language, queries)
            })
            .as_ref()
    }

    /// Each language's queries that do not compile as a whole (`build` would quietly fall
    /// back to the last one).
    #[cfg(test)]
    pub(super) fn query_errors() -> Vec<String> {
        LANGUAGES
            .iter()
            .filter_map(|name| {
                let (language, queries) = sources(name)?;
                Query::new(&language, &queries.concat())
                    .err()
                    .map(|e| format!("{name}: {e}"))
            })
            .collect()
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
        // One symbol per definition and pattern: a C++ parse confused by macros can give a
        // node several names (an initializer list read as declarators); the first is kept.
        // Different patterns may still name one node (Rust's `impl Trait for Type`).
        let mut seen = std::collections::HashSet::new();
        while let Some(m) = matches.next() {
            let mut def = None;
            let mut name = None;
            for c in m.captures() {
                let capture = names[c.index as usize];
                if capture == "name" {
                    name = Some(c.node);
                } else if let Some(kind) = capture
                    .strip_prefix("definition.")
                    .or_else(|| (capture == "reference.implementation").then_some("impl"))
                {
                    def = Some((kind, c.node));
                }
            }
            let (Some((kind, node)), Some(name_node)) = (def, name) else {
                continue;
            };
            let Ok(name) = name_node.utf8_text(text.as_bytes()) else {
                continue;
            };
            if !seen.insert((node.id(), m.pattern_index)) {
                continue;
            }
            let (start, end) = (node.start_position(), node.end_position());
            out.push(Symbol {
                name: name.to_owned(),
                kind: kind.to_owned(),
                start: start.row + 1,
                // A node that takes its line's newline (`#define`) ends on the line before.
                end: if end.column == 0 && end.row > start.row {
                    end.row
                } else {
                    end.row + 1
                },
                signature: signature(text, node.start_byte(), name_node.byte_range()),
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

    #[test]
    fn every_query_compiles() {
        assert_eq!(imp::query_errors(), Vec::<String>::new());
    }

    fn rows(o: &Outline) -> Vec<(usize, usize, &str, &str, usize)> {
        o.symbols
            .iter()
            .map(|s| (s.start, s.end, s.kind.as_str(), s.name.as_str(), s.depth))
            .collect()
    }

    const C: &str = "#include <stdio.h>\n\
\n\
#define MAX(a, b) ((a) > (b) ? (a) : (b))\n\
#define LIMIT 10\n\
\n\
struct point {\n\
    int x, y;\n\
};\n\
\n\
typedef enum { RED, GREEN } color;\n\
\n\
static int add(int a, int b);\n\
\n\
static int add(int a, int b) {\n\
    return a + b;\n\
}\n\
\n\
char *name(struct point *p) {\n\
    (void)p;\n\
    return \"p\";\n\
}\n";

    #[test]
    fn c_outline_spans_whole_definitions() {
        let o = outline(Path::new("src/geo.c"), C).expect("c");
        assert_eq!(o.language, "c");
        assert_eq!(
            rows(&o),
            [
                (3, 3, "macro", "MAX", 0),
                (6, 8, "struct", "point", 0),
                (10, 10, "type", "color", 0),
                (14, 16, "function", "add", 0),
                (18, 21, "function", "name", 0),
            ]
        );
        assert_eq!(o.find("name").map(|s| s.start), Some(18));
        assert_eq!(
            o.summary(200),
            "macro MAX, type point, type color, fn add, fn name"
        );
    }

    const CPP: &str = "namespace geo {\n\
\n\
class Shape {\n\
public:\n\
    virtual ~Shape();\n\
    double area() const;\n\
    int sides() const { return n_; }\n\
private:\n\
    int n_;\n\
};\n\
\n\
double Shape::area() const {\n\
    return 0.0;\n\
}\n\
\n\
}  // namespace geo\n\
\n\
template <typename T>\n\
T twice(T v) {\n\
    return v * 2;\n\
}\n";

    #[test]
    fn cpp_outline_has_namespaces_classes_and_methods() {
        let o = outline(Path::new("src/shape.cpp"), CPP).expect("cpp");
        assert_eq!(o.language, "cpp");
        assert_eq!(
            rows(&o),
            [
                (1, 16, "module", "geo", 0),
                (3, 10, "class", "Shape", 1),
                (5, 5, "method", "~Shape", 2),
                (6, 6, "method", "area", 2),
                (7, 7, "function", "sides", 2),
                (12, 14, "method", "Shape::area", 1),
                (19, 21, "function", "twice", 0),
            ]
        );
        // The out-of-line definition, by its qualified name or inside its namespace;
        // the definition over the declaration in the class.
        assert_eq!(o.find("Shape::area").map(|s| s.start), Some(12));
        assert_eq!(o.find("geo::Shape::area").map(|s| s.start), Some(12));
        assert_eq!(o.find("Shape.sides").map(|s| s.start), Some(7));
        assert_eq!(o.find("area").map(|s| s.start), Some(12));
        assert_eq!(o.find("Other::area"), None);
    }

    #[test]
    fn signatures_show_the_name() {
        let c = "typedef struct {\n    int x;\n    int y;\n    int z;\n} vec3;\n\nstatic int\nadd(int a, int b)\n{\n    return a + b;\n}\n";
        let o = outline(Path::new("v.c"), c).expect("c");
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            ["typedef struct { … } vec3;", "static int add(int a, int b)"]
        );
    }

    #[test]
    fn headers_read_as_c_or_cpp() {
        let h = "#ifndef GEO_H\n#define GEO_H\n\nstruct point { int x; };\nint add(int a, int b);\nchar *name(void);\n\n#endif\n";
        let o = outline(Path::new("include/geo.h"), h).expect("header");
        assert_eq!(o.language, "c/c++");
        let names: Vec<&str> = o.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["point", "add", "name"]);
        assert_eq!(language_of(Path::new("a.HPP")), Some("cpp"));
        assert_eq!(language_of(Path::new("a.cc")), Some("cpp"));
    }
}
