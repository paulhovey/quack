//! The planner/editor handoff (`[model] planner`): when a plan is accepted, the messages
//! the planner added are replaced by one user message for the session's model. That model
//! never sees the planner's tool calls, so its cached prefix (everything before plan mode)
//! stays valid and the only new tokens are the brief (docs/plan/04-agent-loop-and-tools.md).

use quack_providers::Message;

/// The brief that replaces the planner's messages: the user's requests, their answers to
/// the planner's questions, and the planner's last reply (its summary of the plan).
/// `plan_note` is the plan-mode request appended to each user message, taken off again;
/// `ask` is the name the planner's profile gives `ask_user`.
pub(crate) fn brief(
    segment: &[Message],
    plan_note: &str,
    ask: Option<&str>,
    planner: &str,
) -> String {
    let suffix = format!("\n\n{plan_note}");
    let mut requests = Vec::new();
    let mut answers = Vec::new();
    let mut summary = "";
    let mut asks: Vec<&str> = Vec::new();
    for m in segment {
        match m {
            Message::User { content } => {
                let text = content.strip_suffix(&suffix).unwrap_or(content).trim();
                if !text.is_empty() {
                    requests.push(text);
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.trim().is_empty() {
                    summary = content.trim();
                }
                asks.extend(
                    tool_calls
                        .iter()
                        .filter(|c| Some(c.name.as_str()) == ask)
                        .map(|c| c.id.as_str()),
                );
            }
            Message::Tool { call_id, content } if asks.contains(&call_id.as_str()) => {
                answers.push(content.trim());
            }
            _ => {}
        }
    }
    let mut out = format!(
        "[Plan handoff] {planner} explored the code with read-only tools and wrote the plan \
         below; you carry it out. Its exploration is not repeated here, so read a file before \
         you change it.\n\nRequest:\n{}",
        requests.join("\n\n")
    );
    if !answers.is_empty() {
        out.push_str(&format!(
            "\n\nThe user's answers to its questions:\n{}",
            answers.join("\n\n")
        ));
    }
    if !summary.is_empty() {
        out.push_str(&format!("\n\nPlan:\n{summary}"));
    }
    out.push_str("\n\nCarry out the plan, and keep the todo list up to date as you go.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_providers::ToolCall;

    #[test]
    fn the_brief_keeps_requests_answers_and_the_summary() {
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        };
        let segment = [
            Message::User {
                content: "Add retries\n\n[Plan mode] plan first".into(),
            },
            Message::Assistant {
                content: "Looking around.".into(),
                reasoning: None,
                tool_calls: vec![call("r", "Read"), call("q", "AskUserQuestion")],
                sealed: None,
            },
            Message::Tool {
                call_id: "r".into(),
                content: "fn fetch() {}".into(),
            },
            Message::Tool {
                call_id: "q".into(),
                content: "Q: How many?\nA: 3".into(),
            },
            Message::Assistant {
                content: "1. Wrap fetch in a loop.".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                sealed: None,
            },
        ];
        let b = brief(
            &segment,
            "[Plan mode] plan first",
            Some("AskUserQuestion"),
            "big/model",
        );
        assert!(b.starts_with("[Plan handoff] big/model"), "{b}");
        assert!(
            b.contains("Request:\nAdd retries\n\n"),
            "the plan request is taken off: {b}"
        );
        assert!(
            b.contains("answers to its questions:\nQ: How many?\nA: 3"),
            "{b}"
        );
        assert!(
            b.contains("Plan:\n1. Wrap fetch in a loop."),
            "the last reply is the plan: {b}"
        );
        assert!(!b.contains("fn fetch"), "tool output stays behind: {b}");
        assert!(!b.contains("Looking around"), "{b}");
    }
}
