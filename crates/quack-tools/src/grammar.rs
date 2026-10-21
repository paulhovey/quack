//! The compiled-in tree-sitter grammars and their queries, with overrides from the config
//! layers (docs/plan/05-context-management.md#grammars).
//!
//! Each layer's `grammars/<lang>/` folder may hold `tags.scm` (outlines), `highlights.scm`,
//! `injections.scm` and `locals.scm` (TUI highlighting). A file replaces the query from the
//! layers below it, or adds to it when its first line is `; extends` (the nvim-treesitter
//! convention), so a project can teach the outline a framework's macros without copying
//! the whole query. A query that does not compile is reported and the built-in one is
//! used instead.
//!
//! Grammars are compiled in behind the `lang-common` feature. Without it, nothing parses
//! and callers fall back to plain text.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// Every language with a grammar, by the name its `grammars/<lang>/` folder uses.
pub const LANGUAGES: [&str; 9] = [
    "rust",
    "python",
    "javascript",
    "typescript",
    "tsx",
    "go",
    "c",
    "cpp",
    "bash",
];

/// The query files a language folder may hold.
pub const QUERY_FILES: [&str; 4] = ["tags.scm", "highlights.scm", "injections.scm", "locals.scm"];

/// The highlight names the TUI styles. A capture such as `@function.method` takes the
/// longest of these that prefixes it.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "keyword",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable.builtin",
    "variable.parameter",
];

/// A highlighted run: its byte range in the source and its highlight name, if any.
pub type Highlighted = (Range<usize>, Option<&'static str>);

/// Grammars with the query overrides of a set of `grammars/` folders. Queries compile on
/// first use and are kept.
pub struct Grammars {
    /// `grammars/` folders, lowest layer first.
    dirs: Vec<PathBuf>,
    /// Problems with override files, for `quack doctor` and the log.
    warnings: Mutex<Vec<String>>,
    #[cfg(feature = "lang-common")]
    cache: imp::Cache,
}

impl std::fmt::Debug for Grammars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammars")
            .field("dirs", &self.dirs)
            .finish_non_exhaustive()
    }
}

impl Default for Grammars {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Grammars {
    /// Grammars with overrides from `dirs` (each a `grammars/` folder, lowest layer first).
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            warnings: Mutex::new(Vec::new()),
            #[cfg(feature = "lang-common")]
            cache: imp::Cache::default(),
        }
    }

    /// The `grammars/` folders of the given layer directories, lowest first.
    pub fn from_layers<'a>(layers: impl IntoIterator<Item = &'a Path>) -> Self {
        Self::new(layers.into_iter().map(|d| d.join("grammars")).collect())
    }

    /// One shared set without overrides, so each caller doesn't recompile the queries.
    pub fn builtin() -> Arc<Self> {
        static BUILTIN: LazyLock<Arc<Grammars>> = LazyLock::new(|| Arc::new(Grammars::default()));
        BUILTIN.clone()
    }

    /// Whether this build has any grammars.
    pub fn compiled_in() -> bool {
        cfg!(feature = "lang-common")
    }

    /// Override files that exist, as `(language, file, path)`.
    pub fn overrides(&self) -> Vec<(&'static str, &'static str, PathBuf)> {
        let mut out = Vec::new();
        for dir in &self.dirs {
            for lang in LANGUAGES {
                for file in QUERY_FILES {
                    let p = dir.join(lang).join(file);
                    if p.is_file() {
                        out.push((lang, file, p));
                    }
                }
            }
        }
        out
    }

    /// Compile every overridden query now and return the problems found (also logged).
    /// Folders for languages this build lacks are reported too.
    pub fn check(&self) -> Vec<String> {
        #[cfg(feature = "lang-common")]
        for (lang, file, _) in self.overrides() {
            if file == "tags.scm" {
                let _ = self.tags(lang);
            } else {
                let _ = self.highlight_config(lang);
            }
        }
        let mut out = self.warnings();
        for dir in &self.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() && !LANGUAGES.contains(&name.as_str()) {
                    out.push(format!(
                        "{}: no `{name}` grammar in this build; the folder is ignored",
                        e.path().display()
                    ));
                }
            }
        }
        out
    }

    /// Problems found so far with override files.
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.lock().map(|w| w.clone()).unwrap_or_default()
    }

    /// The query for `lang`'s `file`: the built-in pieces, then each layer's override
    /// (replacing, or appending after `; extends`). The flag says whether a file was used.
    #[cfg_attr(not(feature = "lang-common"), allow(dead_code))]
    fn source(&self, lang: &str, file: &str, builtin: &[&str]) -> (String, bool) {
        let mut src = builtin.concat();
        let mut overridden = false;
        for dir in &self.dirs {
            let path = dir.join(lang).join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            overridden = true;
            if extends(&text) {
                src.push('\n');
                src.push_str(&text);
            } else {
                src = text;
            }
        }
        (src, overridden)
    }

    #[cfg_attr(not(feature = "lang-common"), allow(dead_code))]
    fn warn(&self, message: String) {
        tracing::warn!("{message}");
        if let Ok(mut w) = self.warnings.lock()
            && !w.contains(&message)
        {
            w.push(message);
        }
    }

    /// Whether `lang`'s highlight queries are compiled already, so highlighting it is quick.
    /// The first use compiles them, which takes tens of milliseconds.
    pub fn highlight_ready(&self, lang: &str) -> bool {
        imp::highlight_ready(self, lang)
    }

    /// Syntax highlighting for `code` in `lang` (a name from [`LANGUAGES`]): runs that
    /// cover the code in order. `None` when the language has no grammar in this build.
    pub fn highlight(&self, lang: &str, code: &str) -> Option<Vec<Highlighted>> {
        imp::highlight(self, lang, code)
    }
}

/// A query file that adds to the one below instead of replacing it.
fn extends(text: &str) -> bool {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| {
            l.trim_start()
                .strip_prefix(';')
                .is_some_and(|rest| rest.trim_start_matches(';').trim() == "extends")
        })
}

/// The language a code fence's info string names (`rust`, `py`, `sh`, `rust,ignore`).
pub fn language_for_tag(tag: &str) -> Option<&'static str> {
    let tag = tag
        .split([',', ' ', '{'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Some(match tag.as_str() {
        "rust" => "rust",
        "python" | "python3" => "python",
        "javascript" | "node" => "javascript",
        "typescript" => "typescript",
        "golang" => "go",
        "c++" => "cpp",
        "bash" | "shell" | "sh" | "zsh" | "shellscript" => "bash",
        "" => return None,
        ext => match crate::outline::language_of(Path::new(&format!("x.{ext}")))? {
            "c/c++" => "cpp",
            lang => lang,
        },
    })
}

#[cfg(feature = "lang-common")]
mod imp {
    use std::sync::OnceLock;

    use tree_sitter::{Language, Query};
    use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

    use super::{Grammars, HIGHLIGHT_NAMES, Highlighted, LANGUAGES};

    type Slots<T> = [OnceLock<Option<T>>; LANGUAGES.len()];

    pub(super) struct Cache {
        tags: Slots<(Language, Query)>,
        highlights: Slots<HighlightConfiguration>,
    }

    impl Default for Cache {
        fn default() -> Self {
            Self {
                tags: [const { OnceLock::new() }; LANGUAGES.len()],
                highlights: [const { OnceLock::new() }; LANGUAGES.len()],
            }
        }
    }

    pub(crate) fn language(name: &str) -> Option<Language> {
        Some(match name {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "javascript" => tree_sitter_javascript::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "go" => tree_sitter_go::LANGUAGE.into(),
            "c" => tree_sitter_c::LANGUAGE.into(),
            "cpp" => tree_sitter_cpp::LANGUAGE.into(),
            "bash" => tree_sitter_bash::LANGUAGE.into(),
            _ => return None,
        })
    }

    /// The built-in `(highlights, injections, locals)` pieces. TypeScript builds on the
    /// JavaScript queries and C++ on C's; the more specific pieces come first, since an
    /// earlier pattern wins.
    pub(crate) fn builtin_highlights(
        name: &str,
    ) -> Option<(
        &'static [&'static str],
        &'static str,
        &'static [&'static str],
    )> {
        use tree_sitter_javascript as js;
        use tree_sitter_typescript as ts;
        Some(match name {
            "rust" => (
                &[tree_sitter_rust::HIGHLIGHTS_QUERY],
                tree_sitter_rust::INJECTIONS_QUERY,
                &[],
            ),
            "python" => (&[tree_sitter_python::HIGHLIGHTS_QUERY], "", &[]),
            "javascript" => (
                &[js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY],
                js::INJECTIONS_QUERY,
                &[js::LOCALS_QUERY],
            ),
            "typescript" => (
                &[ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY],
                js::INJECTIONS_QUERY,
                &[ts::LOCALS_QUERY, js::LOCALS_QUERY],
            ),
            "tsx" => (
                &[
                    ts::HIGHLIGHTS_QUERY,
                    js::JSX_HIGHLIGHT_QUERY,
                    js::HIGHLIGHT_QUERY,
                ],
                js::INJECTIONS_QUERY,
                &[ts::LOCALS_QUERY, js::LOCALS_QUERY],
            ),
            "go" => (&[tree_sitter_go::HIGHLIGHTS_QUERY], "", &[]),
            "c" => (&[tree_sitter_c::HIGHLIGHT_QUERY], "", &[]),
            "cpp" => (
                &[
                    tree_sitter_cpp::HIGHLIGHT_QUERY,
                    tree_sitter_c::HIGHLIGHT_QUERY,
                ],
                "",
                &[],
            ),
            "bash" => (&[tree_sitter_bash::HIGHLIGHT_QUERY], "", &[]),
            _ => return None,
        })
    }

    fn slot(name: &str) -> Option<usize> {
        let name = if name == "c/c++" { "cpp" } else { name };
        LANGUAGES.iter().position(|l| *l == name)
    }

    impl Grammars {
        /// The outline query for `name`, with its overrides.
        pub(crate) fn tags(&self, name: &str) -> Option<&(Language, Query)> {
            let i = slot(name)?;
            let name = LANGUAGES[i];
            self.cache.tags[i]
                .get_or_init(|| {
                    let language = language(name)?;
                    let builtin = crate::outline::builtin_tags(name)?;
                    let (src, overridden) = self.source(name, "tags.scm", builtin);
                    if overridden {
                        match Query::new(&language, &src) {
                            Ok(q) => return Some((language, q)),
                            Err(e) => self.warn(format!(
                                "grammars/{name}/tags.scm does not compile, so the built-in \
                                 query is used · {e}"
                            )),
                        }
                    }
                    // A built-in piece that does not fit the grammar is dropped, not fatal.
                    let query = Query::new(&language, &builtin.concat())
                        .or_else(|_| {
                            Query::new(&language, builtin.last().copied().unwrap_or_default())
                        })
                        .map_err(|e| tracing::warn!(error = %e, "tags query does not compile"))
                        .ok()?;
                    Some((language, query))
                })
                .as_ref()
        }

        pub(super) fn highlight_config(&self, name: &str) -> Option<&HighlightConfiguration> {
            let i = slot(name)?;
            let name = LANGUAGES[i];
            self.cache.highlights[i]
                .get_or_init(|| {
                    let (highlights, injections, locals) = builtin_highlights(name)?;
                    let files = [
                        self.source(name, "highlights.scm", highlights),
                        self.source(name, "injections.scm", &[injections]),
                        self.source(name, "locals.scm", locals),
                    ];
                    let build = |h: &str, i: &str, l: &str| {
                        let mut c = HighlightConfiguration::new(language(name)?, name, h, i, l)
                            .map_err(|e| e.to_string());
                        if let Ok(c) = &mut c {
                            c.configure(HIGHLIGHT_NAMES);
                        }
                        Some(c)
                    };
                    if files.iter().any(|(_, o)| *o) {
                        match build(&files[0].0, &files[1].0, &files[2].0)? {
                            Ok(c) => return Some(c),
                            Err(e) => self.warn(format!(
                                "the highlight queries in grammars/{name}/ do not compile, so \
                                 the built-in ones are used · {e}"
                            )),
                        }
                    }
                    // A built-in piece that does not fit the grammar: the first highlights only.
                    let config = match build(&highlights.concat(), injections, &locals.concat())? {
                        Ok(c) => Ok(c),
                        Err(_) => build(highlights.first().copied().unwrap_or_default(), "", "")?,
                    };
                    config
                        .map_err(|e| tracing::warn!(error = %e, "highlight query does not compile"))
                        .ok()
                })
                .as_ref()
        }
    }

    pub(super) fn highlight_ready(g: &Grammars, lang: &str) -> bool {
        slot(lang).is_some_and(|i| g.cache.highlights[i].get().is_some())
    }

    pub(super) fn highlight(g: &Grammars, lang: &str, code: &str) -> Option<Vec<Highlighted>> {
        let config = g.highlight_config(lang)?;
        let mut highlighter = Highlighter::new();
        let events = highlighter
            .highlight(config, code.as_bytes(), None, None, |injected| {
                super::language_for_tag(injected).and_then(|l| g.highlight_config(l))
            })
            .ok()?;
        let mut out: Vec<Highlighted> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for event in events {
            match event.ok()? {
                HighlightEvent::Source { start, end } => {
                    let kind = stack.last().map(|&i| HIGHLIGHT_NAMES[i]);
                    match out.last_mut() {
                        Some((r, k)) if *k == kind && r.end == start => r.end = end,
                        _ => out.push((start..end, kind)),
                    }
                }
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
            }
        }
        Some(out)
    }

    /// Each language's built-in highlight queries that do not compile as a whole.
    #[cfg(test)]
    pub(super) fn highlight_errors() -> Vec<String> {
        LANGUAGES
            .iter()
            .filter_map(|name| {
                let (h, i, l) = builtin_highlights(name)?;
                HighlightConfiguration::new(language(name)?, *name, &h.concat(), i, &l.concat())
                    .err()
                    .map(|e| format!("{name}: {e}"))
            })
            .collect()
    }
}

#[cfg(not(feature = "lang-common"))]
mod imp {
    use super::{Grammars, Highlighted};

    pub(super) fn highlight(_g: &Grammars, _lang: &str, _code: &str) -> Option<Vec<Highlighted>> {
        None
    }

    pub(super) fn highlight_ready(_g: &Grammars, _lang: &str) -> bool {
        false
    }
}

#[cfg(feature = "lang-common")]
pub(crate) use imp::language;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_marker() {
        assert!(extends("; extends\n(x) @y"));
        assert!(extends("\n;; extends\n"));
        assert!(!extends("; a comment\n; extends"));
        assert!(!extends("(x) @y"));
    }

    #[test]
    fn fence_tags_name_languages() {
        assert_eq!(language_for_tag("rust,ignore"), Some("rust"));
        assert_eq!(language_for_tag("py"), Some("python"));
        assert_eq!(language_for_tag("sh"), Some("bash"));
        assert_eq!(language_for_tag("h"), Some("cpp"));
        assert_eq!(language_for_tag("TSX"), Some("tsx"));
        assert_eq!(language_for_tag("text"), None);
        assert_eq!(language_for_tag(""), None);
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn builtin_highlight_queries_compile() {
        assert_eq!(imp::highlight_errors(), Vec::<String>::new());
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn highlights_cover_the_code() {
        let g = Grammars::default();
        for lang in LANGUAGES {
            let code = "fn main() { let x = \"hi\"; } // done\n";
            let runs = g.highlight(lang, code).expect(lang);
            let covered: usize = runs.iter().map(|(r, _)| r.len()).sum();
            assert_eq!(covered, code.len(), "{lang}");
        }
        let runs = g.highlight("rust", "let s = \"hi\"; // c\n").expect("rust");
        let kind_of = |text: &str| {
            let code = "let s = \"hi\"; // c\n";
            let at = code.find(text).expect("present");
            runs.iter()
                .find(|(r, _)| r.contains(&at))
                .and_then(|(_, k)| *k)
        };
        assert_eq!(kind_of("let"), Some("keyword"));
        assert_eq!(kind_of("\"hi\""), Some("string"));
        assert_eq!(kind_of("// c"), Some("comment"));
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn overrides_replace_or_extend_and_bad_ones_fall_back() {
        let user = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        let write = |root: &Path, lang: &str, file: &str, text: &str| {
            let dir = root.join(lang);
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join(file), text).expect("write");
        };
        // The user layer adds `macro_rules!` invocations of `route!` as definitions; the
        // project layer replaces Python's query outright.
        write(
            user.path(),
            "rust",
            "tags.scm",
            "; extends\n(macro_invocation macro: (identifier) @m (#eq? @m \"route\") \
             (token_tree (identifier) @name)) @definition.route\n",
        );
        write(
            project.path(),
            "python",
            "tags.scm",
            "(class_definition name: (identifier) @name) @definition.class\n",
        );
        write(
            project.path(),
            "go",
            "tags.scm",
            "(not_a_node) @definition.x\n",
        );
        write(project.path(), "cobol", "tags.scm", "");
        let g = Grammars::new(vec![
            user.path().to_path_buf(),
            project.path().to_path_buf(),
        ]);

        let rs = "route!(index);\n\nfn handler() {}\n";
        let o = g.outline(Path::new("a.rs"), rs).expect("rust");
        let names: Vec<&str> = o.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["index", "handler"]);

        let py = "class A:\n    def f(self):\n        pass\n";
        let o = g.outline(Path::new("a.py"), py).expect("python");
        let names: Vec<&str> = o.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A"]);

        // A broken override: the built-in query still outlines, with a warning.
        let o = g
            .outline(Path::new("a.go"), "package m\n\nfunc F() {}\n")
            .expect("go");
        assert!(o.find("F").is_some());
        let problems = g.check();
        assert!(
            problems.iter().any(|p| p.contains("grammars/go/tags.scm")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("`cobol`")),
            "{problems:?}"
        );
        assert_eq!(g.overrides().len(), 3);
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn highlight_overrides_extend() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("bash")).expect("mkdir");
        std::fs::write(
            dir.path().join("bash").join("highlights.scm"),
            "; extends\n((word) @constant (#eq? @constant \"QUACK\"))\n",
        )
        .expect("write");
        let g = Grammars::new(vec![dir.path().to_path_buf()]);
        let runs = g.highlight("bash", "echo QUACK\n").expect("bash");
        assert!(
            runs.iter()
                .any(|(r, k)| r.start == 5 && *k == Some("constant")),
            "{runs:?}"
        );
        assert!(g.check().is_empty());
    }
}
