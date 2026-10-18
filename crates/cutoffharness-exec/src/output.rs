/// Output formats for `cutoffharness exec`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Final assistant message only.
    #[default]
    Text,
    /// One object at the end.
    Json,
    /// NDJSON of engine events.
    StreamJson,
}
