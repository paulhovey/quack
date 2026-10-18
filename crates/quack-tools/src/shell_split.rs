//! Splitting a command line into the simple commands a shell would run, for permission
//! checks (docs/plan/11-security.md#permission-modes): `git status && rm -rf x` is
//! `git status` and `rm -rf x`.
//!
//! This is not a shell parser. It understands quotes, escapes, `;` `&&` `||` `|` `&`,
//! newlines, `$(...)` and redirections, which covers what models write. Anything else
//! (backticks, subshells, heredocs, `if`/`for`, process substitution, comments) returns
//! `None`, and the caller must then treat the command as opaque: no allow rule applies.

/// The simple commands in a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Split {
    /// Each simple command, whitespace-collapsed, including those inside `$(...)`.
    pub parts: Vec<String>,
    /// Output is redirected to a file (`> out.txt`), so the command writes.
    pub writes: bool,
}

/// Words that start compound commands the splitter does not follow.
const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "{", "}", "[[", "]]", "!",
];

pub(crate) fn split(cmd: &str) -> Option<Split> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(cmd: &str) -> Vec<String> {
        split(cmd).expect("splits").parts
    }

    #[test]
    fn operators_split_outside_quotes() {
        assert_eq!(parts("git status && rm -rf x"), ["git status", "rm -rf x"]);
        assert_eq!(
            parts("a; b || c | d |& e & f\ng"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(parts("ls 'a && b' \"c; d\""), ["ls 'a && b' \"c; d\""]);
        assert_eq!(parts("echo a\\;b"), ["echo a\\;b"]);
        assert_eq!(parts("cargo test \\\n  --all"), ["cargo test --all"]);
    }

    #[test]
    fn substitutions_are_commands_too() {
        assert_eq!(parts("echo $(rm x)"), ["rm x", "echo $(rm x)"]);
        assert_eq!(parts("echo \"$(id -u)\""), ["id -u", "echo \"$(id -u)\""]);
        assert_eq!(split("echo `rm x`"), None);
        assert_eq!(split("echo \"`rm x`\""), None);
        assert_eq!(split("echo $((1 + 2))"), None);
    }

    #[test]
    fn redirections_note_writes() {
        let s = split("cargo test 2>&1 | tail -50").expect("splits");
        assert_eq!(s.parts, ["cargo test 2>&1", "tail -50"]);
        assert!(!s.writes);
        assert!(!split("rg x 2>/dev/null").expect("splits").writes);
        assert!(split("echo hi > out.txt").expect("splits").writes);
        assert!(split("echo hi >>out.txt; ls").expect("splits").writes);
        assert!(split("make &> build.log").expect("splits").writes);
        assert!(split("echo hi > \"$f\"").expect("splits").writes);
        assert_eq!(parts("sort < in.txt"), ["sort < in.txt"]);
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
            assert_eq!(split(cmd), None, "{cmd}");
        }
        // `#` inside a word is not a comment.
        assert_eq!(parts("git log --format=%h#x"), ["git log --format=%h#x"]);
    }
}
