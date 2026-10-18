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
}

#[derive(Debug, Clone, Default)]
pub struct ToolSurface {
    pub tools: Vec<SurfaceTool>,
}

impl ToolSurface {
    /// Build the surface for a profile. Unknown internal tools are reported, not fatal.
    pub fn build(profile: &Profile, internal: &[InternalTool]) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        if profile.file.tools.is_empty() {
            let tools = internal
                .iter()
                .map(|t| SurfaceTool {
                    name: t.name.clone(),
                    internal: t.name.clone(),
                    description: t.description.clone(),
                    schema: t.schema.clone(),
                    params: BTreeMap::new(),
                })
                .collect();
            return (Self { tools }, warnings);
        }
        let mut tools = Vec::new();
        for m in &profile.file.tools {
            let target = m.maps_to.as_deref().unwrap_or(&m.name);
            let Some(t) = internal.iter().find(|t| t.name == target) else {
                warnings.push(format!(
                    "profile `{}`: tool `{}` maps to unknown internal tool `{target}`",
                    profile.id, m.name
                ));
                continue;
            };
            tools.push(SurfaceTool {
                name: m.name.clone(),
                internal: t.name.clone(),
                description: m
                    .description
                    .clone()
                    .unwrap_or_else(|| t.description.clone()),
                schema: rename_schema(&t.schema, &m.params),
                params: m.params.clone(),
            });
        }
        (Self { tools }, warnings)
    }

    /// Translate a model call into `(internal tool, internal arguments)`.
    pub fn map_call(&self, name: &str, args: Value) -> Option<(String, Value)> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        if tool.params.is_empty() {
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
        Some((tool.internal.clone(), Value::Object(out)))
    }
}

fn transform(v: Value, t: Option<Transform>) -> Value {
    match t {
        Some(Transform::ZeroToOneBased) => match v.as_u64() {
            Some(n) => Value::from(n + 1),
            None => v,
        },
        Some(Transform::AbsToWorkspace) | None => v,
    }
}

/// Rename internal parameter names to the model-facing ones in a JSON Schema.
fn rename_schema(schema: &Value, params: &BTreeMap<String, ParamMapping>) -> Value {
    let mut s = schema.clone();
    let back: BTreeMap<&str, &str> = params
        .iter()
        .map(|(k, p)| (p.to.as_str(), k.as_str()))
        .collect();
    if let Some(Value::Object(props)) = s.get_mut("properties") {
        let renamed: serde_json::Map<String, Value> = std::mem::take(props)
            .into_iter()
            .map(|(k, v)| (back.get(k.as_str()).map_or(k, |n| (*n).to_owned()), v))
            .collect();
        *props = renamed;
    }
    if let Some(Value::Array(req)) = s.get_mut("required") {
        for r in req.iter_mut() {
            if let Some(n) = r.as_str().and_then(|k| back.get(k)) {
                *r = Value::from(*n);
            }
        }
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
                    },
                ),
                (
                    "offset".to_owned(),
                    ParamMapping {
                        to: "offset".into(),
                        transform: Some(Transform::ZeroToOneBased),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        }];
        let internal = [InternalTool {
            name: "read".into(),
            description: "Read".into(),
            schema: json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"}},"required":["path"]}),
        }];
        let (surface, warnings) = ToolSurface::build(&p, &internal);
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
}
