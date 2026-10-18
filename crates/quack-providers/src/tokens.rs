//! Token counting. M1 ships the `estimate` source; server-reported usage corrects it.

/// Rough token count: about 3.5 bytes per token for code and English.
pub fn estimate(text: &str) -> u32 {
    u32::try_from(text.len() * 2 / 7).unwrap_or(u32::MAX)
}

/// The byte estimate scaled by what the server actually reported, per model. Serialized
/// JSON inflates the raw count; the learned ratio absorbs that too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimator {
    ratio: f64,
}

impl Default for Estimator {
    fn default() -> Self {
        Self { ratio: 1.0 }
    }
}

impl Estimator {
    /// A rough count corrected by the observed ratio.
    pub fn scale(&self, estimated: u32) -> u32 {
        (f64::from(estimated) * self.ratio)
            .round()
            .min(f64::from(u32::MAX)) as u32
    }

    /// Learn from one response: `estimated` for the request, `actual` prompt tokens reported.
    pub fn observe(&mut self, estimated: u32, actual: u32) {
        // Tiny requests are dominated by template overhead and would skew the ratio.
        if estimated < 64 || actual == 0 {
            return;
        }
        let sample = (f64::from(actual) / f64::from(estimated)).clamp(0.25, 4.0);
        // Moving average: follows a new model quickly, ignores one-off outliers.
        self.ratio = 0.5 * self.ratio + 0.5 * sample;
    }

    pub fn ratio(&self) -> f64 {
        self.ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimator_converges_on_reported_usage() {
        let mut e = Estimator::default();
        assert_eq!(e.scale(1000), 1000);
        for _ in 0..8 {
            e.observe(1000, 700);
        }
        assert!((e.ratio() - 0.7).abs() < 0.01, "{}", e.ratio());
        assert!((1395..=1410).contains(&e.scale(2000)), "{}", e.scale(2000));
        e.observe(10, 1000);
        assert!((e.ratio() - 0.7).abs() < 0.01, "small requests are ignored");
        e.observe(1000, 1_000_000);
        assert!(e.ratio() < 2.36, "outliers are clamped");
    }
}
