//! Maps internal tools to the model-facing tool surface, with parameter renames and transforms.

use std::collections::BTreeMap;

use quack_config::profile::{ParamMapping, Transform};
use serde_json::Value;

use crate::Profile;

/// An internal tool as the registry describes it.
#[derive(Debug, Clone)]
pub struct InternalTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// A tool as the model sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTool {
    pub name: String,
    pub internal: String,
    pub description: String,
    pub schema: Value,
    params: BTreeMap<String, ParamMapping>,
    fixed: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolSurface {
    pub tools: Vec<SurfaceTool>,
}

impl ToolSurface {
    /// Build the surface for a profile from the tools available in this session. `known`
    /// names every internal tool, so a mapping onto one that is merely off this session
    /// (`lsp` with no servers, another edit format) is skipped quietly.
    pub fn build(
        profile: &Profile,
        internal: &[InternalTool],
        known: &[&str],
    ) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut tools = Vec::new();
        let mut mapped: Vec<&str> = Vec::new();
        for m in &profile.file.tools {
            let target = m.maps_to.as_deref().unwrap_or(&m.name);
            let Some(t) = internal.iter().find(|t| t.name == target) else {
                if !known.contains(&target) {
                    warnings.push(format!(
                        "profile `{}`: tool `{}` maps to unknown internal tool `{target}`",
                        profile.id, m.name
                    ));
                }
                continue;
            };
            mapped.push(&t.name);
            tools.push(SurfaceTool {
                name: m.name.clone(),
                internal: t.name.clone(),
                description: m
                    .description
                    .clone()
                    .unwrap_or_else(|| t.description.clone()),
                schema: model_schema(&t.schema, &m.params, &m.fixed),
                params: m.params.clone(),
                fixed: m.fixed.clone(),
            });
        }
        if !profile.file.exclusive {
            for t in internal
                .iter()
                .filter(|t| !mapped.contains(&t.name.as_str()))
            {
                if tools.iter().any(|s| s.name == t.name) {
                    warnings.push(format!(
                        "profile `{}`: tool `{}` hides the internal tool of that name",
                        profile.id, t.name
                    ));
                    continue;
                }
                tools.push(SurfaceTool {
                    name: t.name.clone(),
                    internal: t.name.clone(),
                    description: t.description.clone(),
                    schema: t.schema.clone(),
                    params: BTreeMap::new(),
                    fixed: BTreeMap::new(),
                });
            }
        }
        (Self { tools }, warnings)
    }

    /// The name the model sees for an internal tool, if offered.
    pub fn name_of(&self, internal: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|t| t.internal == internal)
            .map(|t| t.name.as_str())
    }

    /// Translate a model call into `(internal tool, internal arguments)`.
    pub fn map_call(&self, name: &str, args: Value) -> Option<(String, Value)> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        if tool.params.is_empty() && tool.fixed.is_empty() {
            return Some((tool.internal.clone(), args));
        }
        let Value::Object(map) = args else {
            return Some((tool.internal.clone(), args));
        };
        let mut out = serde_json::Map::new();
        for (k, v) in map {
            match tool.params.get(&k) {
                Some(p) => {
                    out.insert(p.to.clone(), transform(v, p.transform));
                }
                None => {
                    out.insert(k, v);
                }
            }
        }
        for p in tool.params.values() {
            if let Some(d) = &p.default {
                out.entry(p.to.clone()).or_insert_with(|| d.clone());
            }
        }
        for (k, v) in &tool.fixed {
            out.insert(k.clone(), v.clone());
        }
        Some((tool.internal.clone(), Value::Object(out)))
    }
}

fn transform(v: Value, t: Option<Transform>) -> Value {
    match t {
        Some(Transform::ZeroToOneBased) => match v.as_u64() {
            Some(n) => Value::from(n + 1),
            None => v,
        },
        Some(Transform::OneToZeroBased) => match v.as_u64() {
            Some(n) => Value::from(n.saturating_sub(1)),
            None => v,
        },
        Some(Transform::JoinLines) => match v {
            Value::Array(lines) => Value::from(
                lines
                    .iter()
                    .map(|l| l.as_str().map_or_else(|| l.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            other => other,
        },
        Some(Transform::Argv) => match v {
            Value::Array(args) => Value::from(command_line(&args)),
            other => other,
        },
        Some(Transform::AbsToWorkspace) | None => v,
    }
}

/// One command line from an argument list. A shell wrapper (`bash -lc <script>`) gives its
/// script; other arguments are quoted where needed.
fn command_line(args: &[Value]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.as_str().map_or_else(|| a.to_string(), str::to_owned))
        .collect();
    let shells = [
        "bash",
        "sh",
        "zsh",
        "/bin/bash",
        "/bin/sh",
        "/bin/zsh",
        "pwsh",
        "powershell",
    ];
    if let [shell, flag, script] = args.as_slice()
        && shells.contains(&shell.as_str())
        && matches!(flag.as_str(), "-c" | "-lc" | "-Command")
    {
        return script.clone();
    }
    args.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The schema the model sees: internal parameter names renamed, fixed ones left out, and
/// joined-line and argument-list parameters shown as arrays.
fn model_schema(
    schema: &Value,
    params: &BTreeMap<String, ParamMapping>,
    fixed: &BTreeMap<String, Value>,
) -> Value {
    let mut s = schema.clone();
    let back: BTreeMap<&str, (&str, &ParamMapping)> = params
        .iter()
        .map(|(k, p)| (p.to.as_str(), (k.as_str(), p)))
        .collect();
    if let Some(Value::Object(props)) = s.get_mut("properties") {
        let renamed: serde_json::Map<String, Value> = std::mem::take(props)
            .into_iter()
            .filter(|(k, _)| !fixed.contains_key(k))
            .map(|(k, mut v)| match back.get(k.as_str()) {
                Some((n, p)) => {
                    if matches!(p.transform, Some(Transform::JoinLines | Transform::Argv)) {
                        let doc = v.get("description").cloned();
                        v = serde_json::json!({ "type": "array", "items": { "type": "string" } });
                        if let Some(d) = doc {
                            v["description"] = d;
                        }
                    }
                    ((*n).to_owned(), v)
                }
                None => (k, v),
            })
            .collect();
        *props = renamed;
    }
    if let Some(Value::Array(req)) = s.get_mut("required") {
        req.retain(|r| r.as_str().is_none_or(|k| !fixed.contains_key(k)));
        for r in req.iter_mut() {
            if let Some((n, p)) = r.as_str().and_then(|k| back.get(k)) {
                // A parameter with a default is optional for the model.
                if p.default.is_some() {
                    *r = Value::Null;
                } else {
                    *r = Value::from(*n);
                }
            }
        }
        req.retain(|r| !r.is_null());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_config::profile::ToolMapping;
    use serde_json::json;

    #[test]
    fn renames_round_trip() {
        let mut p = Profile::empty();
        p.file.tools = vec![ToolMapping {
            name: "read_file".into(),
            maps_to: Some("read".into()),
            description: None,
            params: [
                (
                    "absolute_path".to_owned(),
                    ParamMapping {
                        to: "path".into(),
                        transform: Some(Transform::AbsToWorkspace),
                        default: None,
                    },
                ),
                (
                    "offset".to_owned(),
                    ParamMapping {
                        to: "offset".into(),
                        transform: Some(Transform::ZeroToOneBased),
                        default: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            fixed: BTreeMap::new(),
        }];
        let internal = [InternalTool {
            name: "read".into(),
            description: "Read".into(),
            schema: json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"}},"required":["path"]}),
        }];
        let (surface, warnings) = ToolSurface::build(&p, &internal, &["read"]);
        assert!(warnings.is_empty());
        let t = &surface.tools[0];
        assert_eq!(t.schema["required"][0], "absolute_path");
        assert!(t.schema["properties"].get("absolute_path").is_some());
        let (name, args) = surface
            .map_call("read_file", json!({"absolute_path": "/r/a", "offset": 0}))
            .expect("mapped");
        assert_eq!(name, "read");
        assert_eq!(args, json!({"path": "/r/a", "offset": 1}));
    }

    #[test]
    fn fixed_defaults_joined_lines_and_unlisted_tools() {
        let mut p = Profile::empty();
        p.file.tools = vec![
            ToolMapping {
                name: "Write".into(),
                maps_to: Some("write".into()),
                description: None,
                params: [(
                    "lines".to_owned(),
                    ParamMapping {
                        to: "content".into(),
                        transform: Some(Transform::JoinLines),
                        default: None,
                    },
                )]
                .into_iter()
                .collect(),
                fixed: [("mode".to_owned(), json!("strict"))].into_iter().collect(),
            },
            ToolMapping {
                name: "Lsp".into(),
                maps_to: Some("lsp".into()),
                description: None,
                params: BTreeMap::new(),
                fixed: BTreeMap::new(),
            },
        ];
        let internal = [
            InternalTool {
                name: "write".into(),
                description: "Write".into(),
                schema: json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"mode":{"type":"string"}},"required":["path","content","mode"]}),
            },
            InternalTool {
                name: "grep".into(),
                description: "Grep".into(),
                schema: json!({"type":"object"}),
            },
        ];
        // `lsp` is known but off this session: no warning, no tool.
        let (surface, warnings) = ToolSurface::build(&p, &internal, &["write", "grep", "lsp"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let names: Vec<&str> = surface.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Write", "grep"]);
        let w = &surface.tools[0];
        assert!(w.schema["properties"].get("mode").is_none());
        assert_eq!(w.schema["properties"]["lines"]["type"], "array");
        assert_eq!(w.schema["required"], json!(["path", "lines"]));
        let (_, args) = surface
            .map_call("Write", json!({"path": "a", "lines": ["x", "y"]}))
            .expect("mapped");
        assert_eq!(
            args,
            json!({"path": "a", "content": "x\ny", "mode": "strict"})
        );

        p.file.exclusive = true;
        let (surface, _) = ToolSurface::build(&p, &internal, &["write", "grep", "lsp"]);
        assert_eq!(surface.tools.len(), 1);
    }

    #[test]
    fn argv_becomes_one_command_line() {
        let line = |v: Value| command_line(v.as_array().expect("array"));
        assert_eq!(
            line(json!(["bash", "-lc", "make test && ls"])),
            "make test && ls"
        );
        assert_eq!(
            line(json!(["rg", "-n", "fn main", "src"])),
            "rg -n 'fn main' src"
        );
        assert_eq!(line(json!(["echo", "it's"])), "echo 'it'\\''s'");
    }
}
