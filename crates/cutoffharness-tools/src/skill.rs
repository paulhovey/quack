//! `skill`: load a skill's instructions (docs/plan/08-instructions-skills-commands.md#progressive-disclosure).

use cutoffharness_instructions::skills::Skill;
use cutoffharness_protocol::ApprovalRequest;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str};

#[derive(Debug)]
pub struct LoadSkill;

/// The text a skill expands to, for the tool and for `/<name>` invocations alike.
pub fn expand(skill: &Skill) -> Result<String, String> {
    Ok(format!(
        "<skill name=\"{}\" directory=\"{}\">\nPaths in this skill are relative to its directory.\n\n{}\n</skill>",
        skill.name,
        skill.dir.display(),
        skill.body()?
    ))
}

impl Tool for LoadSkill {
    fn name(&self) -> &str {
        "skill"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "Load a skill's instructions by name. The available skills are listed in the system \
         prompt; load one when the task matches its description, then follow it."
            .into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name from the list" }
            },
            "required": ["name"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        find(args, ctx)?;
        Ok(Subject::Skill)
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        format!(
            "skill {}",
            arg_str(args, &["name", "skill"]).unwrap_or_default()
        )
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "skill".into(),
            arguments: find(args, ctx)?.name.clone(),
        })
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        !ctx.skills.is_empty()
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match find(&args, ctx).and_then(|s| Ok((s, expand(s)?))) {
                Ok((s, text)) => ToolOutput::ok(text, format!("loaded skill {}", s.name)),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

fn find<'a>(args: &Value, ctx: &'a ToolCtx) -> Result<&'a Skill, String> {
    let name = arg_str(args, &["name", "skill"])?;
    let name = name.trim().trim_start_matches('/');
    ctx.skills.iter().find(|s| s.name == name).ok_or_else(|| {
        let names: Vec<&str> = ctx.skills.iter().map(|s| s.name.as_str()).collect();
        format!("no skill named `{name}`; available: {}", names.join(", "))
    })
}
