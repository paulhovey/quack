//! JSON Schema generation for editor completion (`cutoffharness config schema --write`).
//! Default TOML files reference these with a relative `#:schema ../schemas/<kind>.json`.

use serde_json::{Value, json};

use crate::profile::ProfileFile;
use crate::provider::ProviderFile;
use crate::settings::Settings;

/// `(file name, schema)` for every config file kind. Kinds whose crates have not landed
/// yet get permissive stubs so `#:schema` lines resolve.
pub fn all() -> Vec<(&'static str, Value)> {
    let gen_ = |v: schemars::Schema| v.to_value();
    vec![
        ("config.json", gen_(schemars::schema_for!(Settings))),
        ("provider.json", gen_(schemars::schema_for!(ProviderFile))),
        ("profile.json", gen_(schemars::schema_for!(ProfileFile))),
        ("mcp.json", stub("MCP server (mcp.d/<id>.toml)", "M3")),
        ("lsp.json", stub("LSP server (lsp.d/<id>.toml)", "M4")),
        ("hook.json", stub("Hook (hooks.d/<id>.toml)", "M3")),
        (
            "compat.json",
            stub("Other-harness detection list (compat.toml)", "M2"),
        ),
    ]
}

fn stub(title: &str, milestone: &str) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "description": format!("Stub: the full schema is generated once this file kind is implemented ({milestone})."),
        "type": "object"
    })
}
