//! Splitting a PowerShell line into the commands it runs, with tree-sitter-powershell
//! (docs/plan/11-security.md#powershell-2026-10-25). The output is the same [`Split`] the
//! bash reader gives, so the permission rules don't change.
//!
//! - Every `command` anywhere in the tree is a part, inner ones first: script blocks
//!   (`ForEach-Object { … }`), `( … )` and `$( … )` (also inside strings), and the bodies
//!   of `if`, `foreach`, `while`, `switch`, `try` and functions.
//! - `& 'C:\…\git.exe' status` is read as `C:\…\git.exe status`; `& $cmd` is opaque.
//! - Literal scripts are looked into: `iex '…'` / `Invoke-Expression` and
//!   `pwsh`/`powershell -Command '…'` (any abbreviation of `-Command`).
//! - A .NET method call (`[IO.File]::Delete('x')`, `$f.Delete()`) is a part of its own,
//!   which no read-only rule covers, so it never passes as reading.
//! - **Opaque (`None`)**: a tree with an error, a computed command name, a provider path
//!   written as a variable (`${C:\x.txt} = 1`), and nesting deeper than 8 scripts.
//! - **Writes**: a `>`-style redirection to anything but `$null` or `NUL`.

use tree_sitter::{Node, Parser};

use crate::shell_split::Split;

pub(crate) fn split(cmd: &str) -> Option<Split> {
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
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
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

fn walk(node: Node<'_>, src: &str, out: &mut Split, depth: usize) -> Option<()> {
    match node.kind() {
        "comment" => Some(()),
        "command" => {
            if let Some(inner) = wrapped_script(node, src) {
                return script(&inner, out, depth + 1);
            }
            let part = part(node, src)?;
            children(node, src, out, depth)?;
            out.parts.push(part);
            Some(())
        }
        "invokation_expression" => {
            children(node, src, out, depth)?;
            out.parts.push(collapse(text(node, src)));
            Some(())
        }
        // `${C:\x.txt}` reads or writes a file through a provider; `${env:X}` is fine.
        "braced_variable" => {
            let t = text(node, src).to_ascii_lowercase();
            (!t.contains(':') || t.starts_with("${env:")).then_some(())
        }
        "redirection" => {
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

/// Whitespace collapsed, with backtick line continuations joined.
fn collapse(s: &str) -> String {
    s.replace("`\r\n", " ")
        .replace("`\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command as the rules see it. `& 'name' args` becomes `name args`; a name the line
/// computes (`& $cmd`, `& (Get-X)`) gives `None`.
fn part(node: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let operator = node
        .children(&mut cursor)
        .find(|c| c.kind() == "command_invokation_operator")
        .map(|c| text(c, src));
    let Some(operator) = operator else {
        return Some(collapse(text(node, src)));
    };
    let name = node.child_by_field_name("command_name")?;
    let program = match name.named_child(0) {
        Some(n) if n.kind() == "command_name" => text(n, src).to_owned(),
        Some(n) if n.kind() == "string_literal" => literal(n, src)?,
        _ => return None,
    };
    let rest = node
        .child_by_field_name("command_elements")
        .map(|e| text(e, src))
        .unwrap_or_default();
    // Dot-sourcing keeps its `.`, so it never reads as the script's own name.
    let head = if operator == "." {
        format!(". {program}")
    } else {
        program
    };
    Some(collapse(&format!("{head} {rest}")))
}

/// The command's name, unquoted when it is a literal.
fn name(node: Node<'_>, src: &str) -> Option<String> {
    let name = node.child_by_field_name("command_name")?;
    match name.kind() {
        "command_name" => Some(text(name, src).to_owned()),
        _ => match name.named_child(0) {
            Some(n) if n.kind() == "command_name" => Some(text(n, src).to_owned()),
            Some(n) if n.kind() == "string_literal" => literal(n, src),
            _ => None,
        },
    }
}

/// The arguments and parameters of a command, without the separators between them.
fn elements<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let Some(e) = node.child_by_field_name("command_elements") else {
        return Vec::new();
    };
    let mut cursor = e.walk();
    e.named_children(&mut cursor)
        .filter(|c| c.kind() != "command_argument_sep")
        .collect()
}

/// The script of `iex '<literal>'` or `pwsh -Command '<literal>'`, so its commands are
/// checked instead of the wrapper. Anything else gives `None`, and the wrapper is then an
/// ordinary command, which no read-only rule covers.
fn wrapped_script(node: Node<'_>, src: &str) -> Option<String> {
    let name = name(node, src)?.to_ascii_lowercase();
    let program = name.rsplit(['/', '\\']).next().unwrap_or(&name);
    let program = program.strip_suffix(".exe").unwrap_or(program);
    let args = elements(node);
    match program {
        "iex" | "invoke-expression" => {
            let args = match args.split_first() {
                Some((first, rest)) if is_param(*first, src, "-command") => rest,
                _ => &args[..],
            };
            match args {
                [only] => literal(*only, src),
                _ => None,
            }
        }
        "pwsh" | "powershell" => {
            // Any options, then `-Command <script>` as the last two elements.
            let at = args.iter().position(|a| is_param(*a, src, "-command"))?;
            match &args[at + 1..] {
                [only] => literal(*only, src),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A parameter that abbreviates `full` (`-c`, `-Co`, `-Command`), ignoring case.
fn is_param(node: Node<'_>, src: &str, full: &str) -> bool {
    let t = text(node, src).to_ascii_lowercase();
    node.kind() == "command_parameter" && t.len() >= 2 && full.starts_with(t.as_str())
}

/// A word or quoted string with no expansion in it, unquoted.
fn literal(node: Node<'_>, src: &str) -> Option<String> {
    // Arguments come wrapped in expression nodes with one child each.
    let mut node = node;
    while matches!(node.kind(), "array_literal_expression" | "unary_expression")
        && node.named_child_count() == 1
    {
        node = node.named_child(0)?;
    }
    let t = text(node, src);
    match node.kind() {
        "generic_token" if !t.contains(['$', '`', '@']) => Some(t.to_owned()),
        "string_literal" => {
            let inner = node.named_child(0)?;
            match inner.kind() {
                "verbatim_string_characters" if t.starts_with('\'') => {
                    Some(t.get(1..t.len() - 1)?.replace("''", "'"))
                }
                "expandable_string_literal" if t.starts_with('"') && !t.contains(['$', '`']) => {
                    Some(t.get(1..t.len() - 1)?.replace("\"\"", "\""))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether a redirection writes a file: not `$null`, `NUL`, or a stream (`2>&1`).
fn writes(node: Node<'_>, src: &str) -> bool {
    let mut cursor = node.walk();
    let mut to_file = false;
    let mut target = String::new();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "file_redirection_operator" => to_file = true,
            "redirected_file_name" => target = text(c, src).trim().to_ascii_lowercase(),
            _ => {}
        }
    }
    to_file && !matches!(target.as_str(), "$null" | "nul" | "/dev/null")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(cmd: &str) -> Vec<String> {
        split(cmd).expect("splits").parts
    }

    #[test]
    fn statements_and_pipelines_split() {
        assert_eq!(
            parts("git status; Remove-Item -Recurse x"),
            ["git status", "Remove-Item -Recurse x"]
        );
        assert_eq!(
            parts("git log --oneline -5 && cargo build"),
            ["git log --oneline -5", "cargo build"]
        );
        assert_eq!(
            parts("cargo test 2>&1 | Select-Object -Last 20"),
            ["cargo test 2>&1", "Select-Object -Last 20"]
        );
        assert_eq!(parts("echo 'a; rm b'"), ["echo 'a; rm b'"]);
        assert_eq!(
            parts("Get-ChildItem `\n  -Recurse"),
            ["Get-ChildItem -Recurse"]
        );
    }

    #[test]
    fn nested_commands_are_found() {
        assert_eq!(
            parts("Get-ChildItem -Recurse | ForEach-Object { Remove-Item $_ }"),
            [
                "Get-ChildItem -Recurse",
                "Remove-Item $_",
                "ForEach-Object { Remove-Item $_ }"
            ]
        );
        assert_eq!(
            parts("Get-Content (Remove-Item x)"),
            ["Remove-Item x", "Get-Content (Remove-Item x)"]
        );
        assert_eq!(
            parts("Write-Output \"$(Remove-Item x)\""),
            ["Remove-Item x", "Write-Output \"$(Remove-Item x)\""]
        );
        assert_eq!(
            parts("if (Test-Path x) { Remove-Item x }"),
            ["Test-Path x", "Remove-Item x"]
        );
        assert_eq!(
            parts("foreach ($f in Get-ChildItem) { Remove-Item $f }"),
            ["Get-ChildItem", "Remove-Item $f"]
        );
        assert_eq!(
            parts("$x = Get-Content a; Set-Content b $x"),
            ["Get-Content a", "Set-Content b $x"]
        );
        assert_eq!(parts("Get-Content x # rm y"), ["Get-Content x"]);
    }

    #[test]
    fn method_calls_are_parts() {
        assert_eq!(parts("[IO.File]::Delete('x')"), ["[IO.File]::Delete('x')"]);
        assert_eq!(
            parts("(Get-Item x).Delete()"),
            ["Get-Item x", "(Get-Item x).Delete()"]
        );
        assert_eq!(parts("$a.Delete()"), ["$a.Delete()"]);
        assert!(parts("\"hi\"").is_empty());
    }

    #[test]
    fn call_operators_and_literal_scripts() {
        assert_eq!(parts("& \"git\" status"), ["git status"]);
        assert_eq!(
            parts("&'C:\\Program Files\\Git\\bin\\git.exe' status"),
            ["C:\\Program Files\\Git\\bin\\git.exe status"]
        );
        assert_eq!(parts(". ./a.ps1"), [". ./a.ps1"]);
        assert_eq!(parts("iex 'git status; rm x'"), ["git status", "rm x"]);
        assert_eq!(
            parts("Invoke-Expression -Command \"git status\""),
            ["git status"]
        );
        assert_eq!(
            parts("pwsh -NoProfile -c 'git status; rm x'"),
            ["git status", "rm x"]
        );
        assert_eq!(
            parts("powershell.exe -Command 'Remove-Item ''a b'''"),
            ["Remove-Item 'a b'"]
        );
        // A computed script stays the wrapper.
        assert_eq!(parts("iex $cmd"), ["iex $cmd"]);
        assert_eq!(
            parts("iex (Get-Content x)"),
            ["Get-Content x", "iex (Get-Content x)"]
        );
        assert_eq!(parts("iex \"rm $x\""), ["iex \"rm $x\""]);
    }

    #[test]
    fn redirections_note_writes() {
        let w = |c: &str| split(c).map(|s| s.writes);
        assert_eq!(w("Get-Content a.txt > b.txt"), Some(true));
        assert_eq!(w("cargo test 2> err.txt"), Some(true));
        assert_eq!(w("cargo test 2>&1"), Some(false));
        assert_eq!(w("Get-ChildItem > $null"), Some(false));
        assert_eq!(w("Get-ChildItem > NUL"), Some(false));
    }

    #[test]
    fn unsure_forms_are_opaque() {
        for cmd in [
            "& $cmd",
            "& (Get-Command rm) x",
            "${C:\\x.txt} = 1",
            "Get-Content 'unterminated",
            "if (x) {",
        ] {
            assert_eq!(split(cmd), None, "{cmd}");
        }
        assert_eq!(parts("${env:FOO}"), Vec::<String>::new());
    }
}
