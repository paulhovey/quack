//! String lists in `config.toml` accept either an array (replace) or `{ append = [...] }`
//! (extend the lower layer's list). The append form is resolved during merging; this
//! deserializer only validates both shapes so per-file diagnostics keep their spans.

use std::fmt;

use schemars::JsonSchema;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};

/// Schema-only mirror of the accepted shapes.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum ListOrAppend {
    List(Vec<String>),
    Append {
        /// Appended to the list from lower layers.
        append: Vec<String>,
    },
}

pub(crate) fn de<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an array of strings, or a table `{ append = [...] }`")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                out.push(s);
            }
            Ok(out)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = None;
            while let Some(key) = map.next_key::<String>()? {
                if key != "append" {
                    return Err(de::Error::unknown_field(&key, &["append"]));
                }
                out = Some(map.next_value::<Vec<String>>()?);
            }
            out.ok_or_else(|| de::Error::missing_field("append"))
        }
    }
    d.deserialize_any(V)
}
