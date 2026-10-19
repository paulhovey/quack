//! `ask_user`: the model asks the user about a decision that is theirs to make. The
//! engine shows the questions ([`ToolCtx::ask`]) and the answers come back as the result.

use futures::future::BoxFuture;
use quack_protocol::{Answer, ApprovalRequest, Question, QuestionOption};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput};

const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 6;

/// What the user said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    /// One per question, in order.
    pub answers: Vec<Answer>,
    /// The user wants no more questions.
    pub stop_asking: bool,
}

/// Questions on their way to the user, with the channel for the answers.
pub type AskRequest = (Vec<Question>, oneshot::Sender<Answers>);
pub type Asker = mpsc::UnboundedSender<AskRequest>;

#[derive(Debug)]
pub struct AskUser;

/// A list from an array or an array sent as a JSON string.
fn list(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_array().cloned()),
        _ => None,
    }
}

fn text<'a>(o: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| o.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// An option label with a `(recommended)` mark taken off, and whether it had one.
fn unmark(label: &str) -> (String, bool) {
    let lower = label.to_ascii_lowercase();
    match lower.find("(recommended)") {
        Some(i) => {
            let mut s = label[..i].to_owned();
            s.push_str(&label[i + "(recommended)".len()..]);
            (s.trim().to_owned(), true)
        }
        None => (label.trim().to_owned(), false),
    }
}

fn question(v: &Value) -> Option<Question> {
    let o = match v {
        Value::String(s) if !s.trim().is_empty() => {
            return Some(Question {
                header: String::new(),
                question: s.trim().to_owned(),
                options: Vec::new(),
                recommended: None,
            });
        }
        Value::Object(o) => o,
        _ => return None,
    };
    let q = text(o, &["question", "text", "prompt"])?.to_owned();
    let header = text(o, &["header", "topic", "title"])
        .unwrap_or_default()
        .to_owned();
    let mut options = Vec::new();
    let mut recommended = None;
    let raw = ["options", "choices"]
        .iter()
        .find_map(|k| o.get(*k).and_then(list))
        .unwrap_or_default();
    for opt in raw {
        if options.len() == MAX_OPTIONS {
            break;
        }
        let (label, description, marked) = match &opt {
            Value::String(s) => {
                let (l, m) = unmark(s);
                (l, None, m)
            }
            Value::Object(p) => {
                let Some(l) = text(p, &["label", "text", "value", "option", "title"]) else {
                    continue;
                };
                let (l, m) = unmark(l);
                let d = text(p, &["description", "detail", "details"]).map(str::to_owned);
                let r = matches!(p.get("recommended"), Some(Value::Bool(true)));
                (l, d, m || r)
            }
            _ => continue,
        };
        if label.is_empty() {
            continue;
        }
        if marked && recommended.is_none() {
            recommended = Some(options.len());
        }
        options.push(QuestionOption { label, description });
    }
    match o.get("recommended") {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_u64().and_then(|i| usize::try_from(i).ok()) {
                recommended = Some(i);
            }
        }
        Some(Value::String(s)) => {
            let s = unmark(s).0;
            if let Some(i) = options
                .iter()
                .position(|p| p.label.eq_ignore_ascii_case(&s))
            {
                recommended = Some(i);
            }
        }
        _ => {}
    }
    let recommended = recommended.filter(|i| *i < options.len());
    Some(Question {
        header,
        question: q,
        options,
        recommended,
    })
}

/// Questions from `{"questions": [...]}`, or a single `{"question", "options"}`.
fn questions(args: &Value) -> Result<Vec<Question>, String> {
    let out: Vec<Question> = match ["questions", "items"]
        .iter()
        .find_map(|k| args.get(*k).and_then(list))
    {
        Some(l) => l.iter().filter_map(question).take(MAX_QUESTIONS).collect(),
        None => question(args).into_iter().collect(),
    };
    if out.is_empty() {
        return Err(
            "missing required argument `questions` (an array of {question, options})".into(),
        );
    }
    Ok(out)
}

/// The answers as the model reads them.
fn report(qs: &[Question], a: &Answers) -> String {
    let mut out = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        let answer = match a.answers.get(i) {
            Some(Answer::Picked { index }) => match q.options.get(*index) {
                Some(o) if q.recommended == Some(*index) => {
                    format!("{} (your recommendation)", o.label)
                }
                Some(o) => o.label.clone(),
                None => skipped(),
            },
            Some(Answer::Text { text }) if !text.trim().is_empty() => text.trim().to_owned(),
            _ => skipped(),
        };
        out.push(format!("Q: {}\nA: {answer}", q.question));
    }
    if a.stop_asking {
        out.push(
            "The user wants no more questions; go on with your best judgement and say what \
             you assumed."
                .into(),
        );
    }
    out.join("\n\n")
}

fn skipped() -> String {
    "(skipped) decide yourself and say what you assumed".into()
}

impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Ask the user about decisions that are theirs to make and would change the work in a \
         big way: scope, approach, a trade-off, an unclear requirement. Don't ask what you \
         can find out from the code or docs. Put related questions in one call (up to 4), \
         each with 2-4 short options and your recommended option first. The user can also \
         answer in their own words or skip."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": { "type": "string", "description": "A word or two naming the topic" },
                            "question": { "type": "string" },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "recommended": { "type": "integer", "description": "Index of the option you recommend" }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    fn subject(&self, _args: &Value, _ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Plan)
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        match questions(args).as_deref() {
            Ok([q]) if !q.header.is_empty() => format!("ask · {}", q.header),
            Ok([_]) => "ask 1 question".into(),
            Ok(qs) => format!("ask {} questions", qs.len()),
            Err(_) => "ask".into(),
        }
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "ask_user".into(),
            arguments: self.summary(args, ctx),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let qs = match questions(&args) {
                Ok(q) => q,
                Err(e) => return ToolOutput::error(e),
            };
            let (tx, rx) = oneshot::channel();
            // No one to ask (headless): the model decides.
            let sent = ctx
                .ask
                .as_ref()
                .is_some_and(|a| a.send((qs.clone(), tx)).is_ok());
            let answers = if sent {
                tokio::select! {
                    () = ctx.cancel.cancelled() => None,
                    a = rx => a.ok(),
                }
            } else {
                Some(Answers {
                    answers: Vec::new(),
                    stop_asking: true,
                })
            };
            let Some(answers) = answers else {
                return ToolOutput::error("interrupted by the user");
            };
            let text = report(&qs, &answers);
            let answered = answers
                .answers
                .iter()
                .filter(|a| !matches!(a, Answer::Skipped))
                .count();
            let summary = if answered == 0 {
                "questions skipped".to_owned()
            } else {
                format!("{answered}/{} answered", qs.len())
            };
            ToolOutput::ok(text.clone(), summary).with_detail(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_shapes_models_send() {
        let a = json!({"questions": [{
            "header": "Storage",
            "question": "Where should tokens live?",
            "options": [
                {"label": "OS keyring", "description": "safest"},
                "A dotfile (Recommended)"
            ]
        }]});
        let q = questions(&a).expect("questions");
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].header, "Storage");
        assert_eq!(q[0].options[1].label, "A dotfile");
        assert_eq!(q[0].options[0].description.as_deref(), Some("safest"));
        assert_eq!(q[0].recommended, Some(1));

        let single = json!({"question": "Which?", "choices": "[\"a\", \"b\"]", "recommended": "b"});
        let q = questions(&single).expect("single");
        assert_eq!(q[0].options.len(), 2);
        assert_eq!(q[0].recommended, Some(1));

        let out_of_range = json!({"question": "Which?", "options": ["a"], "recommended": 5});
        assert_eq!(questions(&out_of_range).expect("q")[0].recommended, None);
        assert!(questions(&json!({})).is_err());
    }

    #[test]
    fn reports_answers_for_the_model() {
        let qs = questions(&json!({"questions": [
            {"question": "Where?", "options": ["keyring", "file"], "recommended": 0},
            {"question": "Which db?"},
            {"question": "Tests?"}
        ]}))
        .expect("questions");
        let a = Answers {
            answers: vec![
                Answer::Picked { index: 0 },
                Answer::Text {
                    text: "sqlite".into(),
                },
                Answer::Skipped,
            ],
            stop_asking: true,
        };
        let r = report(&qs, &a);
        assert!(
            r.contains("Q: Where?\nA: keyring (your recommendation)"),
            "{r}"
        );
        assert!(r.contains("Q: Which db?\nA: sqlite"), "{r}");
        assert!(r.contains("Q: Tests?\nA: (skipped)"), "{r}");
        assert!(r.ends_with("say what you assumed."), "{r}");
    }
}
