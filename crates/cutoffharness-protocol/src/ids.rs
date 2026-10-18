use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    )*};
}

string_id!(
    /// Session identifier (UUID v7 once `cutoffharness-store` lands).
    SessionId,
    /// Tool call identifier, as reported by the provider or synthesised.
    CallId,
    /// Provider id: the `id` key of a `providers.d/*.toml` file.
    ProviderId,
    /// Model id as the provider names it.
    ModelId,
    /// Checkpoint identifier.
    CheckpointId,
);

/// Monotonic turn counter within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub u64);

/// Index of an event in a session log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventIndex(pub u64);
