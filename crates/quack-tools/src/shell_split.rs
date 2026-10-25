//! Splitting a command line into the simple commands a shell would run, for permission
//! checks (docs/plan/11-security.md#permission-modes): `git status && rm -rf x` is
//! `git status` and `rm -rf x`.
//!
//! With `lang-common`, the line is parsed with tree-sitter-bash (`ast`): every simple
//! command anywhere in it counts, including those inside subshells, `{ }`, `if`/`for`/
//! `while`/`case` bodies, `$(...)`, backticks, `<(...)`, heredoc bodies and literal
//! `bash -c '...'` / `eval '...'` scripts. A line that does not parse cleanly, or that runs
//! a computed command name (`$cmd -rf x`), returns `None`.
//!
//! Without it, a small hand-written splitter (`split_simple`) understands quotes, escapes,
//! `;` `&&` `||` `|` `&`, newlines, `$(...)` and redirections, which covers what models
//! write. Anything else (backticks, subshells, heredocs, `if`/`for`, process
//! substitution, comments) returns `None`.
//!
//! On `None` the caller must treat the command as opaque: no allow rule applies.

/// The simple commands in a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Split {
    /// Each simple command, whitespace-collapsed, including those inside `$(...)`.
    pub parts: Vec<String>,
    /// Output is redirected to a file (`> out.txt`), so the command writes.
    pub writes: bool,
}

/// Words that start compound commands the hand-written splitter does not follow.
const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "{", "}", "[[", "]]", "!",
];

/// Split a line for the shell that runs it: PowerShell lines are read with the PowerShell
/// grammar when it is compiled in (`ps_split`), everything else as bash.
pub(crate) fn split_for(cmd: &str, powershell: bool) -> Option<Split> {
    #[cfg(feature = "lang-common")]
    if powershell {
        return crate::ps_split::split(cmd);
    }
    #[cfg(not(feature = "lang-common"))]
    let _ = powershell;
    split(cmd)
}

pub(crate) fn split(cmd: &str) -> Option<Split> {
    #[cfg(feature = "lang-common")]
    {
        ast::split(cmd)
    }
    #[cfg(not(feature = "lang-common"))]
    {
        split_simple(cmd)
    }
}

/// The hand-written splitter, for builds without the bash grammar.
#[cfg_attr(all(feature = "lang-common", not(test)), allow(dead_code))]
fn split_simple(cmd: &str) -> Option<Split> {
    let mut out = Split {
        parts: Vec::new(),
        writes: false,
    };
    parse(cmd, &mut out, 0)?;
    Some(out)
}

fn parse(cmd: &str, out: &mut Split, depth: usize) -> Option<()> {
    if depth > 8 {
        return None;
    }
    let chars: Vec<char> = cmd.chars().collect();
    let mut part = String::new();
    // Start of a word: where `#` would start a comment and a redirection target begins.
    let mut word_start = true;
    let mut i = 0;
    let finish = |part: &mut String, out: &mut Split| -> Option<()> {
        let p = part.split_whitespace().collect::<Vec<_>>().join(" ");
        part.clear();
        if p.is_empty() {
            return Some(());
        }
        if KEYWORDS.contains(&p.split(' ').next().unwrap_or_default()) {
            return None;
        }
        out.parts.push(p);
        Some(())
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                let end = chars[i + 1..].iter().position(|&x| x == '\'')? + i + 1;
                part.extend(&chars[i..=end]);
                i = end + 1;
                word_start = false;
                continue;
            }
            '"' => {
                let mut j = i + 1;
                loop {
                    match chars.get(j)? {
                        '\\' => j += 2,
                        '"' => break,
                        '`' => return None,
                        '$' if chars.get(j + 1) == Some(&'(') => {
                            let end = substitution(&chars, j, out, depth)?;
                            j = end + 1;
                        }
                        _ => j += 1,
                    }
                }
                part.extend(&chars[i..=j]);
                i = j + 1;
                word_start = false;
                continue;
            }
            '\\' => {
                match next {
                    // A line continuation.
                    Some('\n') => {}
                    Some(n) => {
                        part.push('\\');
                        part.push(n);
                    }
                    None => return None,
                }
                i += 2;
                word_start = false;
                continue;
            }
            '`' | '(' | ')' => return None,
            '#' if word_start => return None,
            '$' if next == Some('(') => {
                let end = substitution(&chars, i, out, depth)?;
                part.extend(&chars[i..=end]);
                i = end + 1;
                word_start = false;
                continue;
            }
            ';' | '\n' => {
                finish(&mut part, out)?;
                i += 1;
            }
            '&' if next == Some('&') => {
                finish(&mut part, out)?;
                i += 2;
            }
            '|' if next == Some('|') => {
                finish(&mut part, out)?;
                i += 2;
            }
            '|' => {
                finish(&mut part, out)?;
                i += if next == Some('&') { 2 } else { 1 };
            }
            // `&>` and `&>>` redirect both streams.
            '&' if next == Some('>') => {
                i = redirect(&chars, i + 1, &mut part, out)?;
            }
            '&' => {
                finish(&mut part, out)?;
                i += 1;
            }
            '>' => {
                i = redirect(&chars, i, &mut part, out)?;
            }
            '<' => match next {
                // Heredocs, here-strings and process substitution.
                Some('<' | '(') => return None,
                _ => {
                    part.push('<');
                    i += 1;
                }
            },
            c => {
                part.push(c);
                i += 1;
                word_start = c.is_whitespace();
                continue;
            }
        }
        word_start = true;
    }
    finish(&mut part, out)
}

/// `$(...)` starting at `start` (the `$`): its commands are added to `out`. Returns the
/// index of the closing `)`.
fn substitution(chars: &[char], start: usize, out: &mut Split, depth: usize) -> Option<usize> {
    // `$((...))` is arithmetic.
    if chars.get(start + 2) == Some(&'(') {
        return None;
    }
    let mut level = 0usize;
    let mut j = start + 2;
    let mut quote: Option<char> = None;
    while let Some(&c) = chars.get(j) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => j += 1,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => j += 1,
            (None, '(') => level += 1,
            (None, ')') if level == 0 => {
                let inner: String = chars[start + 2..j].iter().collect();
                parse(&inner, out, depth + 1)?;
                return Some(j);
            }
            (None, ')') => level -= 1,
            _ => {}
        }
        j += 1;
    }
    None
}

/// A `>` redirection at `i`: kept in the part's text, and noted as a write unless it goes
/// to `/dev/null`, `NUL` or another stream (`2>&1`). Returns the index after the target.
fn redirect(chars: &[char], i: usize, part: &mut String, out: &mut Split) -> Option<usize> {
    let mut j = i + 1;
    while matches!(chars.get(j), Some('>' | '|')) {
        j += 1;
    }
    part.extend(&chars[i..j]);
    if chars.get(j) == Some(&'&') {
        part.push('&');
        j += 1;
    }
    let op_end = j;
    while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
        j += 1;
    }
    let start = j;
    if start > op_end {
        part.push(' ');
    }
    while let Some(&c) = chars.get(j) {
        if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')') {
            break;
        }
        if matches!(c, '\'' | '"' | '`' | '$' | '\\') {
            // A quoted or computed target: assume a file, and let the caller read the
            // target as ordinary text (it may hold a `$(...)`).
            out.writes = true;
            return Some(start);
        }
        j += 1;
    }
    let target: String = chars[start..j].iter().collect();
    let to_stream = part.ends_with('&') && target.chars().all(|c| c.is_ascii_digit() || c == '-');
    let null = matches!(target.as_str(), "/dev/null" | "NUL" | "nul" | "$null");
    if target.is_empty() {
        return None;
    }
    if !to_stream && !null {
        out.writes = true;
    }
    part.push_str(&target);
    Some(j)
}

#[cfg(feature = "lang-common")]
mod ast {
    use tree_sitter::{Node, Parser};

    use super::Split;

    /// Shells whose `-c` script is read like the line itself.
    const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

    pub(super) fn split(cmd: &str) -> Option<Split> {
        let mut out = Split {
            parts: Vec::new(),
            writes: false,
        };
        script(cmd, &mut out, 0)?;
        Some(out)
    }

    fn script(src: &str, out: &mut Split, depth: usize) -> Option<()> {
        if depth > 8 {
            return None;
        }
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .ok()?;
        let tree = parser.parse(src, None)?;
        let root = tree.root_node();
        if root.has_error() {
            return None;
        }
        walk(root, src, out, depth)
    }

    fn text<'s>(node: Node<'_>, src: &'s str) -> &'s str {
        node.utf8_text(src.as_bytes()).unwrap_or_default()
    }

    /// Every node is visited, so a command nested anywhere (a subshell, a loop body, a
    /// substitution inside a string or a heredoc) is found. Inner commands come first.
    fn walk(node: Node<'_>, src: &str, out: &mut Split, depth: usize) -> Option<()> {
        match node.kind() {
            "comment" => Some(()),
            "command" => {
                let name = node.child_by_field_name("name")?;
                if computed(name) {
                    return None;
                }
                if let Some(inner) = wrapped_script(node, src) {
                    return script(&inner, out, depth + 1);
                }
                children(node, src, out, depth)?;
                out.parts.push(part(node, src));
                Some(())
            }
            "declaration_command" | "unset_command" | "test_command" => {
                children(node, src, out, depth)?;
                out.parts.push(part(node, src));
                Some(())
            }
            // A bare `x=1`; one in front of a command (`FOO=1 make`) is part of it.
            "variable_assignment" | "variable_assignments"
                if !node.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "command" | "declaration_command" | "variable_assignments"
                    )
                }) =>
            {
                children(node, src, out, depth)?;
                out.parts.push(part(node, src));
                Some(())
            }
            "file_redirect" => {
                if writes(node, src) {
                    out.writes = true;
                }
                children(node, src, out, depth)
            }
            _ => children(node, src, out, depth),
        }
    }

    fn children(node: Node<'_>, src: &str, out: &mut Split, depth: usize) -> Option<()> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, src, out, depth)?;
        }
        Some(())
    }

    /// A command name the line computes (`$cmd`, `$(which x)`): what runs is unknown.
    fn computed(name: Node<'_>) -> bool {
        let mut cursor = name.walk();
        name.children(&mut cursor).any(|c| {
            matches!(
                c.kind(),
                "simple_expansion" | "expansion" | "command_substitution" | "concatenation"
            ) || (c.kind() == "string" && computed(c))
        })
    }

    /// A command's text, whitespace-collapsed, with the redirections of the statement
    /// around it (`cargo test 2>&1`). A heredoc shows as its `<<EOF` only.
    fn part(node: Node<'_>, src: &str) -> String {
        let mut s = text(node, src).to_owned();
        if let Some(p) = node.parent().filter(|p| {
            p.kind() == "redirected_statement" && p.child_by_field_name("body") == Some(node)
        }) {
            let mut cursor = p.walk();
            for r in p.children_by_field_name("redirect", &mut cursor) {
                s.push(' ');
                if r.kind() == "heredoc_redirect" {
                    let mut c = r.walk();
                    let start = r.children(&mut c).find(|n| n.kind() == "heredoc_start");
                    s.push_str("<<");
                    s.push_str(start.map(|n| text(n, src)).unwrap_or_default());
                } else {
                    s.push_str(text(r, src));
                }
            }
        }
        s.replace("\\\n", " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether a `>`-style redirection writes a file: not `/dev/null`, `NUL`, or another
    /// stream (`2>&1`, `>&-`).
    fn writes(node: Node<'_>, src: &str) -> bool {
        let mut cursor = node.walk();
        let Some(op) = node
            .children(&mut cursor)
            .find(|c| !c.is_named())
            .map(|c| c.kind())
        else {
            return true;
        };
        if !op.contains('>') {
            return false;
        }
        let mut cursor = node.walk();
        let target: String = node
            .children_by_field_name("destination", &mut cursor)
            .map(|c| text(c, src))
            .collect();
        let to_stream = op.ends_with("&-")
            || (op.ends_with('&') && target.chars().all(|c| c.is_ascii_digit() || c == '-'));
        let null = matches!(target.as_str(), "/dev/null" | "NUL" | "nul" | "$null");
        !to_stream && !null
    }

    /// The script of `bash -c '<literal>'` or `eval '<literal>' ...`, so the commands in it
    /// are checked instead of the wrapper. A script that is not a literal gives `None`, and
    /// the wrapper is then an ordinary command.
    fn wrapped_script(node: Node<'_>, src: &str) -> Option<String> {
        let name = text(node.child_by_field_name("name")?, src);
        let program = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let mut cursor = node.walk();
        let args: Vec<Node<'_>> = node
            .children_by_field_name("argument", &mut cursor)
            .collect();
        if program == "eval" {
            let words = args
                .iter()
                .map(|a| literal(*a, src))
                .collect::<Option<Vec<_>>>()?;
            return (!words.is_empty()).then(|| words.join(" "));
        }
        if !SHELLS.contains(&program) {
            return None;
        }
        // Flags up to the one that holds `c` (`-c`, `-lc`, `-ec`); then the script.
        let mut i = 0;
        loop {
            let flag = text(*args.get(i)?, src);
            if !flag.starts_with('-') || flag.starts_with("--") || flag.len() < 2 {
                return None;
            }
            if !flag[1..].chars().all(|c| c.is_ascii_alphabetic()) {
                return None;
            }
            i += 1;
            if flag.contains('c') {
                break;
            }
        }
        let script = literal(*args.get(i)?, src)?;
        // Anything after the script fills `$0`, `$1`, …; it must be literal too.
        args[i + 1..]
            .iter()
            .all(|a| literal(*a, src).is_some())
            .then_some(script)
    }

    /// A word or quoted string with no expansion in it, unquoted.
    fn literal(node: Node<'_>, src: &str) -> Option<String> {
        let t = text(node, src);
        match node.kind() {
            "raw_string" => Some(t.get(1..t.len() - 1)?.to_owned()),
            "string" => {
                let mut cursor = node.walk();
                if node
                    .named_children(&mut cursor)
                    .any(|c| c.kind() != "string_content")
                {
                    return None;
                }
                let inner = t.get(1..t.len() - 1)?;
                let mut s = String::new();
                let mut chars = inner.chars();
                while let Some(c) = chars.next() {
                    match (c, chars.clone().next()) {
                        ('\\', Some(n @ ('"' | '\\' | '$' | '`'))) => {
                            s.push(n);
                            chars.next();
                        }
                        ('\\', Some('\n')) => {
                            chars.next();
                        }
                        _ => s.push(c),
                    }
                }
                Some(s)
            }
            "word" | "number" if !t.contains(['\\', '$', '`']) => Some(t.to_owned()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple(cmd: &str) -> Vec<String> {
        split_simple(cmd).expect("splits").parts
    }

    // The hand-written splitter, used without `lang-common`.

    #[test]
    fn operators_split_outside_quotes() {
        assert_eq!(simple("git status && rm -rf x"), ["git status", "rm -rf x"]);
        assert_eq!(
            simple("a; b || c | d |& e & f\ng"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(simple("ls 'a && b' \"c; d\""), ["ls 'a && b' \"c; d\""]);
        assert_eq!(simple("echo a\\;b"), ["echo a\\;b"]);
        assert_eq!(simple("cargo test \\\n  --all"), ["cargo test --all"]);
    }

    #[test]
    fn substitutions_are_commands_too() {
        assert_eq!(simple("echo $(rm x)"), ["rm x", "echo $(rm x)"]);
        assert_eq!(simple("echo \"$(id -u)\""), ["id -u", "echo \"$(id -u)\""]);
        assert_eq!(split_simple("echo `rm x`"), None);
        assert_eq!(split_simple("echo \"`rm x`\""), None);
        assert_eq!(split_simple("echo $((1 + 2))"), None);
    }

    #[test]
    fn redirections_note_writes() {
        let s = split_simple("cargo test 2>&1 | tail -50").expect("splits");
        assert_eq!(s.parts, ["cargo test 2>&1", "tail -50"]);
        assert!(!s.writes);
        assert!(!split_simple("rg x 2>/dev/null").expect("splits").writes);
        assert!(split_simple("echo hi > out.txt").expect("splits").writes);
        assert!(
            split_simple("echo hi >>out.txt; ls")
                .expect("splits")
                .writes
        );
        assert!(split_simple("make &> build.log").expect("splits").writes);
        assert!(split_simple("echo hi > \"$f\"").expect("splits").writes);
        assert_eq!(simple("sort < in.txt"), ["sort < in.txt"]);
    }

    #[test]
    fn unsure_forms_are_opaque() {
        for cmd in [
            "(cd x && rm y)",
            "if true; then rm x; fi",
            "for f in *; do rm $f; done",
            "cat <<EOF\nx\nEOF",
            "diff <(ls a) <(ls b)",
            "ls # comment",
            "echo 'unterminated",
            "{ rm x; }",
        ] {
            assert_eq!(split_simple(cmd), None, "{cmd}");
        }
        // `#` inside a word is not a comment.
        assert_eq!(simple("git log --format=%h#x"), ["git log --format=%h#x"]);
    }

    // The tree-sitter-bash splitter.

    #[cfg(feature = "lang-common")]
    fn ast(cmd: &str) -> Vec<String> {
        ast::split(cmd).expect("splits").parts
    }

    #[cfg(feature = "lang-common")]
    fn ast_set(cmd: &str) -> Vec<String> {
        let mut p = ast(cmd);
        p.sort();
        p
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn ast_agrees_with_the_simple_splitter() {
        for cmd in [
            "git status && rm -rf x",
            "a; b || c | d |& e & f\ng",
            "ls 'a && b' \"c; d\"",
            "cargo test \\\n  --all",
            "echo $(rm x)",
            "echo \"$(id -u)\"",
            "cargo test 2>&1 | tail -50",
            "sort < in.txt",
            "git log --format=%h#x",
            "FOO=1 make -j4",
        ] {
            let mut want = simple(cmd);
            want.sort();
            assert_eq!(ast_set(cmd), want, "{cmd}");
        }
        for (cmd, writes) in [
            ("cargo test 2>&1 | tail -50", false),
            ("rg x 2>/dev/null", false),
            ("echo hi > out.txt", true),
            ("echo hi >>out.txt; ls", true),
            ("make &> build.log", true),
            ("echo hi > \"$f\"", true),
            ("ls >&-", false),
        ] {
            assert_eq!(ast::split(cmd).map(|s| s.writes), Some(writes), "{cmd}");
        }
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn ast_follows_compound_commands() {
        assert_eq!(ast("(cd x && rm y)"), ["cd x", "rm y"]);
        assert_eq!(ast("{ rm x; }"), ["rm x"]);
        assert_eq!(ast("if true; then rm x; fi"), ["true", "rm x"]);
        assert_eq!(
            ast_set("for f in $(ls); do rm \"$f\"; done"),
            ["ls", "rm \"$f\""]
        );
        assert_eq!(ast("while read l; do echo $l; done"), ["read l", "echo $l"]);
        assert_eq!(ast("case $x in a) rm a;; esac"), ["rm a"]);
        assert_eq!(ast("echo `rm x`"), ["rm x", "echo `rm x`"]);
        assert_eq!(
            ast_set("diff <(ls a) <(ls b)"),
            ["diff <(ls a) <(ls b)", "ls a", "ls b"]
        );
        assert_eq!(ast("echo $((1 + 2))"), ["echo $((1 + 2))"]);
        assert_eq!(ast("ls # rm -rf x"), ["ls"]);
        assert_eq!(ast("! grep -q x f"), ["grep -q x f"]);
        assert_eq!(ast("x=$(rm y)"), ["rm y", "x=$(rm y)"]);
        assert_eq!(ast("[[ -f a ]] && cat a"), ["[[ -f a ]]", "cat a"]);
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn ast_reads_heredocs() {
        assert_eq!(ast("cat <<EOF\nrm -rf x\nEOF"), ["cat <<EOF"]);
        assert_eq!(ast_set("cat <<EOF\n$(rm x)\nEOF"), ["cat <<EOF", "rm x"]);
        assert_eq!(
            ast_set("cat <<'EOF' | sh\nrm x\nEOF"),
            ["cat <<'EOF'", "sh"]
        );
        let s = ast::split("cat > out.txt <<EOF\nhi\nEOF").expect("splits");
        assert!(s.writes);
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn ast_looks_inside_literal_scripts() {
        assert_eq!(
            ast("bash -c 'git status && rm -rf x'"),
            ["git status", "rm -rf x"]
        );
        assert_eq!(ast("sh -lc \"cargo test\""), ["cargo test"]);
        assert_eq!(ast("/bin/bash -c 'ls' arg0"), ["ls"]);
        assert_eq!(ast("eval 'rm x'"), ["rm x"]);
        assert_eq!(
            ast("bash -c \"echo \\\"hi\\\"; rm x\""),
            ["echo \"hi\"", "rm x"]
        );
        // A computed script stays the wrapper, which no read-only rule covers.
        assert_eq!(ast("bash -c \"$CMD\""), ["bash -c \"$CMD\""]);
        assert_eq!(ast("eval $x"), ["eval $x"]);
        assert_eq!(ast("bash script.sh"), ["bash script.sh"]);
    }

    #[cfg(feature = "lang-common")]
    #[test]
    fn ast_unsure_forms_are_opaque() {
        for cmd in [
            "$cmd -rf x",
            "$(which rm) x",
            "\"$X\" y",
            "echo 'unterminated",
            "if true; then",
            "echo $(",
            "bash -c 'rm $(' ",
        ] {
            assert_eq!(ast::split(cmd), None, "{cmd}");
        }
    }
}
