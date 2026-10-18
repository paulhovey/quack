//! Token counting. M1 ships the `estimate` source; server-reported usage corrects it.

/// Rough token count: about 3.5 bytes per token for code and English.
pub fn estimate(text: &str) -> u32 {
    u32::try_from(text.len() * 2 / 7).unwrap_or(u32::MAX)
}
