//! Backend AI wording is gated on
//! provider readiness): the check texts a user reads, in the wording
//! [`AiWording`] asks for.
//!
//! Only the presentation changes. Checks keep emitting the Python app's
//! summaries, so the stored findings and their upsert key
//! `(check_id, album_id, summary)` are the same whatever the readiness: a
//! provider becoming ready (or going away) between two runs neither
//! duplicates nor orphans a finding, and an ignored finding stays ignored.
//! The runner's API shapes ([`crate::runner::finding_to_json`],
//! [`crate::runner::meta_to_json`]) apply these functions.
//!
//! With [`AiWording::NamesAi`] every function returns its input unchanged.

use std::borrow::Cow;

use musaic_core::AiWording;

use crate::base::CheckMeta;
use crate::checks::tags_out_of_sync::{NO_ANCHOR_SUFFIX, NO_ANCHOR_SUFFIX_NEUTRAL};

/// Check descriptions that name AI, reworded for [`AiWording::Neutral`]:
/// the same text without the AI fallback.
pub const NEUTRAL_DESCRIPTIONS: [(&str, &str); 3] = [
    (
        "no-cover-art",
        "Albums missing both embedded cover art and a sidecar cover.jpg / folder.jpg / front.jpg. \
Fix tries MusicBrainz / Cover Art Archive (free). Albums with a recent failed lookup are skipped so bulk \
passes don't keep retrying dead-ends.",
    ),
    (
        "track-gaps",
        "Albums whose track-number sequence has gaps (1, 2, 4) or repeats (1, 2, 2). Detected \
from tag inspection — fingerprints not required to detect, but Fix uses them when available. Cascade: MB \
title match → AcoustID fingerprint match. Note: this is about the TAG; the duplicate-tracks check \
catches duplicate AUDIO regardless of tag.",
    ),
    (
        "tags-out-of-sync",
        "Albums where Vorbis tags drift from MusicBrainz's canonical values, are missing a \
required field (title / artist / year / genre), or are missing the *_SORT variants used by Plex and Roon \
for sidebar alphabetization. Fix applies all MusicBrainz-derivable corrections in one click; fields the \
resolver can't fill (genre, or anything on an unanchored album) are left as they are.",
    ),
];

/// A check's description as shown.
pub fn description(meta: &CheckMeta, wording: AiWording) -> &'static str {
    if wording.names_ai() {
        return meta.description;
    }
    NEUTRAL_DESCRIPTIONS
        .iter()
        .find(|(id, _)| *id == meta.id)
        .map_or(meta.description, |(_, d)| d)
}

/// A finding's summary as shown. Only tags-out-of-sync summaries of
/// unanchored albums name AI (their [`NO_ANCHOR_SUFFIX`]).
pub fn finding_summary<'a>(check_id: &str, summary: &'a str, wording: AiWording) -> Cow<'a, str> {
    if wording.names_ai() || check_id != crate::checks::tags_out_of_sync::META.id {
        return Cow::Borrowed(summary);
    }
    match summary.strip_suffix(NO_ANCHOR_SUFFIX) {
        Some(head) => Cow::Owned(format!("{head}{NO_ANCHOR_SUFFIX_NEUTRAL}")),
        None => Cow::Borrowed(summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::tags_out_of_sync::build_summary;
    use crate::registry::Registry;

    fn names_ai(s: &str) -> bool {
        let re = regex::Regex::new(r"\bAI\b|Claude").unwrap();
        re.is_match(s)
    }

    // With a ready provider every description is the Python app's; without
    // one none names AI, and only the three that did are reworded.
    #[test]
    fn neutral_descriptions_name_no_ai() {
        let reg = Registry::default();
        let mut reworded = 0;
        for m in reg.all_meta() {
            assert_eq!(description(m, AiWording::NamesAi), m.description);
            let n = description(m, AiWording::Neutral);
            assert!(!names_ai(n), "{}: {n}", m.id);
            if n != m.description {
                assert!(names_ai(m.description), "{} reworded needlessly", m.id);
                reworded += 1;
            }
        }
        assert_eq!(reworded, NEUTRAL_DESCRIPTIONS.len());
    }

    // The stored summary (the upsert key) keeps the Python app's text;
    // only the shown one changes, and only for an unanchored album.
    #[test]
    fn neutral_summaries_name_no_ai() {
        let stored = build_summary(&["genre"], 1, 0, 0, false);
        assert_eq!(
            stored,
            "missing: genre · 1 canonical drift (no MB anchor — AI required)"
        );
        let id = "tags-out-of-sync";
        assert_eq!(finding_summary(id, &stored, AiWording::NamesAi), stored);
        assert_eq!(
            finding_summary(id, &stored, AiWording::Neutral),
            "missing: genre · 1 canonical drift (no MusicBrainz match yet)"
        );
        let anchored = build_summary(&["genre"], 1, 0, 0, true);
        assert_eq!(finding_summary(id, &anchored, AiWording::Neutral), anchored);
        // Another check's summary is never touched.
        assert_eq!(
            finding_summary("track-gaps", &stored, AiWording::Neutral),
            stored
        );
    }
}
