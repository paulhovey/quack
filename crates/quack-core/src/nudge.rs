//! The memory nudge: a one-line reminder, added to a user message that sounds like a
//! correction or a lasting preference, to save it with the memory tool
//! (docs/plan/07-sessions-memory-checkpoints.md#behaviour). It costs no model call, and it
//! goes into the message itself, so the history stays append-only.

/// Phrases that state how to work from now on, anywhere in the message.
const LASTING: &[&str] = &[
    "from now on",
    "going forward",
    "in future",
    "in the future",
    "next time",
    "remember that",
    "remember to",
    "keep in mind",
    "i prefer",
    "i'd prefer",
    "i would prefer",
    "i'd rather",
    "i would rather",
    "we prefer",
    "we always",
    "we never",
    "always use",
    "never use",
    "don't ever",
    "do not ever",
    "stop using",
    "stop doing",
    "please don't",
    "please do not",
    "that's not what i",
    "that is not what i",
    "not like that",
    "i told you",
    "you should have",
    "you shouldn't have",
];

/// Openings of a correction or a standing order.
const OPENINGS: &[&str] = &[
    "no,",
    "no.",
    "no!",
    "no that",
    "nope",
    "wrong",
    "that's wrong",
    "actually,",
    "don't ",
    "do not ",
    "stop ",
    "never ",
    "always ",
];

/// Longer messages are mostly pasted material, where these words mean nothing.
const MAX_LEN: usize = 1_200;

/// Whether `typed` sounds like something worth remembering. Code blocks and quoted lines
/// don't count, and neither do slash commands.
pub(crate) fn lasting(typed: &str) -> bool {
    if typed.trim_start().starts_with('/') {
        return false;
    }
    let mut prose = String::new();
    let mut fenced = false;
    for line in typed.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && !t.starts_with('>') {
            prose.push_str(t);
            prose.push(' ');
        }
    }
    if prose.len() > MAX_LEN {
        return false;
    }
    let prose = prose.to_lowercase().replace('\u{2019}', "'");
    let prose = prose.trim();
    LASTING.iter().any(|p| prose.contains(p)) || OPENINGS.iter().any(|p| prose.starts_with(p))
}

/// The reminder, naming the memory tool as the model knows it.
pub(crate) fn note(tool: &str) -> String {
    format!(
        "[If this message corrects how you work or states a lasting preference, save it with \
         `{tool}` so later sessions follow it: update the note that covers it if there is one, \
         and say why. Skip this if it only matters to the current task.]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues() {
        for yes in [
            "From now on use pnpm",
            "No, that's the wrong file",
            "Always run clippy before you finish",
            "I\u{2019}d prefer tabs here",
            "please don't touch vendor/",
            "Fix the bug.\n\nNext time, run the tests first.",
        ] {
            assert!(lasting(yes), "{yes}");
        }
        for no in [
            "fix the failing test",
            "why does this test always fail?",
            "Notice the error below",
            "/review always",
            "explain this:\n```\n// from now on we cache\n```",
            "> I prefer the old one\nwhat does this comment mean?",
        ] {
            assert!(!lasting(no), "{no}");
        }
        assert!(!lasting(&format!(
            "{} from now on",
            "log line\n".repeat(200)
        )));
    }
}
