//! Answering what MCP servers ask of quack (docs/plan/09-mcp.md#client-capabilities).
//!
//! A form (elicitation) is shown as questions, one per field, like `ask_user`; skipping a
//! required field or stopping the questions declines it, and interrupting cancels it.
//! A sampling request is a tool-like action, `mcp:<server>/sampling`, so it follows the
//! permission rules: the user approves it unless a rule allows it. It then goes to the
//! session's model with no tools, and counts toward `/stats`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use quack_mcp::{Elicited, FieldKind, Form, SampleRequest, Sampled};
use quack_protocol::{
    Answer, ApprovalDecision, ApprovalRequest, CallId, Event, Question, QuestionOption,
};
use quack_providers::{FinishReason, Message, StreamEvent};
use quack_tools::{Answers, Decision, Permissions, Subject};
use serde_json::{Map, Value};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::session::{self, SharedLog};
use crate::startup::Setup;
use crate::stats::Stats;
use crate::turn::{self, Approvals, Questions};

/// The form as questions: the server's message leads the first one.
pub(crate) fn questions(server: &str, form: &Form) -> Vec<Question> {
    form.fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut ask = f.label.clone();
            if !f.description.is_empty() {
                ask.push_str(&format!(": {}", f.description));
            }
            if matches!(f.kind, FieldKind::Choices(_)) {
                let all: Vec<&str> = match &f.kind {
                    FieldKind::Choices(o) => o.iter().map(|(_, l)| l.as_str()).collect(),
                    _ => Vec::new(),
                };
                ask.push_str(&format!(" (any of {}, comma-separated)", all.join(", ")));
            }
            if !f.required {
                ask.push_str(" (optional)");
            }
            let options = match &f.kind {
                FieldKind::Choice(o) => o.iter().map(|(_, l)| l.as_str()).collect(),
                FieldKind::Boolean => vec!["Yes", "No"],
                _ => Vec::new(),
            };
            Question {
                header: server.to_owned(),
                question: if i == 0 {
                    format!("{}\n{ask}", form.message)
                } else {
                    ask
                },
                options: options
                    .into_iter()
                    .map(|l| QuestionOption {
                        label: l.to_owned(),
                        description: None,
                    })
                    .collect(),
                recommended: None,
            }
        })
        .collect()
}

/// The user's answers as the server's result, and why a form was declined when the user
/// may not expect it.
pub(crate) fn elicited(form: &Form, answers: Option<Answers>) -> (Elicited, Option<String>) {
    let Some(answers) = answers else {
        return (Elicited::Cancel, None);
    };
    let skipped = |a: Option<&Answer>| matches!(a, None | Some(Answer::Skipped));
    if answers.stop_asking || answers.answers.iter().all(|a| skipped(Some(a))) {
        return (Elicited::Decline, None);
    }
    let mut content = Map::new();
    for (i, f) in form.fields.iter().enumerate() {
        let value = match (answers.answers.get(i), &f.kind) {
            (a, _) if skipped(a) => {
                if f.required {
                    return (
                        Elicited::Decline,
                        Some(format!("declined the form: `{}` is required", f.label)),
                    );
                }
                continue;
            }
            (Some(Answer::Picked { index }), FieldKind::Boolean) => Ok(Value::Bool(*index == 0)),
            (Some(Answer::Picked { index }), FieldKind::Choice(o)) => o
                .get(*index)
                .map(|(v, _)| Value::String(v.clone()))
                .ok_or_else(|| "no such option".to_owned()),
            (Some(Answer::Text { text }), _) => f.value(text),
            _ => Err("no such option".to_owned()),
        };
        match value {
            Ok(v) => {
                content.insert(f.name.clone(), v);
            }
            Err(e) => {
                return (
                    Elicited::Decline,
                    Some(format!("declined the form: {}: {e}", f.label)),
                );
            }
        }
    }
    (Elicited::Accept(content), None)
}

/// What a sampling request needs from the engine.
pub(crate) struct Sampler {
    pub tx: broadcast::Sender<Event>,
    pub approvals: Approvals,
    pub perms: Arc<Mutex<Permissions>>,
    pub setup: Arc<Setup>,
    pub stats: Arc<Mutex<Stats>>,
    pub log: Option<SharedLog>,
    /// The running turn's, so an interrupt stops the request too.
    pub cancel: CancellationToken,
    pub call_id: CallId,
}

impl Sampler {
    /// Approve `request` if the rules don't decide it, then send it to the model.
    pub async fn run(self, server: &str, request: SampleRequest) -> Result<Sampled, String> {
        let subject = Subject::Mcp {
            server: server.to_owned(),
            tool: "sampling".into(),
            // It changes nothing, so it may run while planning; it spends tokens, so it asks.
            read_only: true,
            auto_approve: false,
        };
        let decision = self
            .perms
            .lock()
            .map(|p| p.check(&subject))
            .unwrap_or(Decision::Ask);
        match decision {
            Decision::Allow => {}
            Decision::Deny(reason) => return Err(format!("permission denied: {reason}")),
            Decision::Ask => match self.approve(server, &request).await {
                Some(ApprovalDecision::AllowOnce) => {}
                Some(ApprovalDecision::AllowSession | ApprovalDecision::AlwaysAllow { .. }) => {
                    if let Ok(mut p) = self.perms.lock() {
                        p.allow_session(subject);
                    }
                }
                Some(ApprovalDecision::Deny | ApprovalDecision::DenyWithFeedback { .. }) => {
                    return Err("the user declined the request".into());
                }
                None => return Err("the user interrupted the request".into()),
            },
        }
        self.complete(request).await
    }

    async fn approve(&self, server: &str, request: &SampleRequest) -> Option<ApprovalDecision> {
        let (otx, orx) = oneshot::channel();
        let id = self.call_id.0.clone();
        if let Ok(mut pending) = self.approvals.lock() {
            pending.insert(id.clone(), otx);
        }
        let last = request
            .messages
            .last()
            .map(|m| m.text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let last: String = if last.chars().count() > 300 {
            last.chars().take(300).chain(['…']).collect()
        } else {
            last
        };
        let _ = self.tx.send(Event::ApprovalRequest {
            call_id: self.call_id.clone(),
            request: ApprovalRequest::Tool {
                name: format!("mcp:{server}/sampling"),
                arguments: format!(
                    "{server} asks {} for a reply of up to {} tokens to: {last}",
                    self.setup.model_ref, request.max_tokens
                ),
            },
            always: Vec::new(),
        });
        let asked = Instant::now();
        let answer = tokio::select! {
            () = self.cancel.cancelled() => None,
            a = orx => a.ok(),
        };
        if let Ok(mut st) = self.stats.lock() {
            st.waited(asked.elapsed());
        }
        if let Ok(mut pending) = self.approvals.lock() {
            pending.remove(&id);
        }
        session::record(
            self.log.as_ref(),
            "approval",
            serde_json::json!({ "call_id": id, "decision": answer }),
        );
        answer
    }

    /// Send the request to the session's model, with no tools.
    async fn complete(self, request: SampleRequest) -> Result<Sampled, String> {
        let s = &self.setup;
        let mut messages: Vec<Message> = request
            .system
            .map(|content| Message::System { content })
            .into_iter()
            .collect();
        messages.extend(request.messages.into_iter().map(|m| {
            if m.assistant {
                Message::Assistant {
                    content: m.text,
                    reasoning: None,
                    tool_calls: Vec::new(),
                    sealed: None,
                }
            } else {
                Message::User {
                    content: m.text,
                    images: Vec::new(),
                }
            }
        }));
        let mut req = turn::request(s, messages, false);
        req.max_tokens = Some(
            s.model
                .max_output
                .map_or(request.max_tokens, |m| m.min(request.max_tokens)),
        );
        if request.temperature.is_some() {
            req.sampling.temperature = request.temperature;
        }
        let sent = Instant::now();
        let mut rx = s
            .provider
            .stream(req, turn::reasoning_format(s), self.cancel.child_token());
        let (mut text, mut usage, mut truncated) = (String::new(), None, false);
        loop {
            let item = tokio::select! {
                () = self.cancel.cancelled() => return Err("interrupted by the user".into()),
                item = rx.recv() => item,
            };
            match item {
                None => break,
                Some(Ok(StreamEvent::TextDelta(t))) => text.push_str(&t),
                Some(Ok(StreamEvent::Usage(u))) => usage = Some(u),
                Some(Ok(StreamEvent::Finish(FinishReason::Length))) => truncated = true,
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(format!("{}: {e}", s.provider.id)),
            }
        }
        let cost = usage.as_ref().and_then(|u| {
            let c = s.model.cost.as_ref()?;
            Some(c.of(
                u.prompt_tokens,
                u.cached_prompt_tokens.unwrap_or(0),
                u.cache_write_tokens.unwrap_or(0),
                u.completion_tokens,
            ))
        });
        session::record_usage(self.log.as_ref(), usage.as_ref(), cost);
        if let Ok(mut st) = self.stats.lock() {
            st.request(usage.as_ref(), cost, sent.elapsed());
        }
        Ok(Sampled {
            model: s.model_ref.clone(),
            text: text.trim().to_owned(),
            truncated,
        })
    }
}

/// Wait for the answers to a form's questions; `None` when the turn is interrupted or
/// the server stops waiting.
pub(crate) async fn answers(
    questions: &Questions,
    id: &str,
    rx: oneshot::Receiver<Answers>,
    cancel: &CancellationToken,
    reply: &mut oneshot::Sender<Elicited>,
) -> Option<Answers> {
    let answer = tokio::select! {
        a = rx => a.ok(),
        () = cancel.cancelled() => None,
        () = reply.closed() => None,
    };
    if let Ok(mut pending) = questions.lock() {
        pending.remove(id);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form() -> Form {
        Form::from_schema(
            "Deploy where?".into(),
            &json!({"type": "object", "required": ["env"], "properties": {
                "env": {"type": "string", "enum": ["dev", "prod"]},
                "force": {"type": "boolean"},
                "note": {"type": "string", "description": "Why"}
            }}),
        )
    }

    fn answered(answers: Vec<Answer>, stop_asking: bool) -> (Elicited, Option<String>) {
        elicited(
            &form(),
            Some(Answers {
                answers,
                stop_asking,
            }),
        )
    }

    #[test]
    fn form_fields_become_questions() {
        let q = questions("deployer", &form());
        assert_eq!(q.len(), 3);
        assert_eq!(q[0].header, "deployer");
        assert!(q[0].question.starts_with("Deploy where?\nenv"));
        assert_eq!(q[0].options.len(), 2);
        assert_eq!(q[1].options[0].label, "Yes");
        assert_eq!(q[2].question, "note: Why (optional)");
    }

    #[test]
    fn answers_become_the_result() {
        let (r, _) = answered(
            vec![
                Answer::Picked { index: 1 },
                Answer::Picked { index: 1 },
                Answer::Skipped,
            ],
            false,
        );
        let want: Map<String, Value> = [
            ("env".into(), json!("prod")),
            ("force".into(), json!(false)),
        ]
        .into_iter()
        .collect();
        assert_eq!(r, Elicited::Accept(want));
        let (r, why) = answered(vec![Answer::Skipped, Answer::Picked { index: 0 }], false);
        assert_eq!(r, Elicited::Decline);
        assert!(why.is_some_and(|w| w.contains("`env` is required")));
        assert_eq!(answered(Vec::new(), true).0, Elicited::Decline);
        assert_eq!(elicited(&form(), None).0, Elicited::Cancel);
        let typed = answered(
            vec![Answer::Text {
                text: "staging".into(),
            }],
            false,
        );
        assert_eq!(typed.0, Elicited::Decline);
    }
}
