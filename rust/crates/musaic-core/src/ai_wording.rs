//! User-visible text the backend produces names AI or Claude only when an AI
//! provider is ready.
//!
//! The rule mirrors the frontend's (`frontend/src/lib/aiGating.ts`): "ready"
//! is the `ready` flag `GET /api/ai/providers` reports for the provider the
//! worker uses. With a ready provider every text stays byte-identical to the
//! Python version's ([`AiWording::NamesAi`]); without one the
//! provider-neutral wording is used ([`AiWording::Neutral`]).
//!
//! Crates low in the dependency graph (resolver, checks, the work gate) take
//! this value instead of depending on the AI crate; the application reads
//! readiness from its provider and passes it down.
//!
//! Stored text is never reworded: finding summaries are part of the upsert
//! key `(check_id, album_id, summary)` and the resolver's results are
//! memoized, so both keep the original text and are reworded only where they
//! are presented. Readiness changing between runs therefore cannot duplicate
//! or orphan a finding.

/// Which wording user-visible text uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiWording {
    /// A provider is ready: the original text, which may name AI or Claude.
    NamesAi,
    /// No provider is ready: no mention of AI or Claude.
    Neutral,
}

impl AiWording {
    /// The wording for a provider readiness flag.
    pub fn for_readiness(ai_ready: bool) -> AiWording {
        if ai_ready {
            AiWording::NamesAi
        } else {
            AiWording::Neutral
        }
    }

    /// Whether text may name AI or Claude.
    pub fn names_ai(self) -> bool {
        self == AiWording::NamesAi
    }

    /// `names_ai` when text may name AI, else `neutral`.
    pub fn pick<'a>(self, names_ai: &'a str, neutral: &'a str) -> &'a str {
        match self {
            AiWording::NamesAi => names_ai,
            AiWording::Neutral => neutral,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Readiness maps to the original wording, its absence to the neutral one.
    #[test]
    fn readiness_picks_the_wording() {
        assert_eq!(AiWording::for_readiness(true), AiWording::NamesAi);
        assert_eq!(AiWording::for_readiness(false), AiWording::Neutral);
        assert!(AiWording::NamesAi.names_ai());
        assert!(!AiWording::Neutral.names_ai());
        assert_eq!(AiWording::NamesAi.pick("a", "b"), "a");
        assert_eq!(AiWording::Neutral.pick("a", "b"), "b");
    }
}
