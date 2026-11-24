//! The check texts a user reads. A few of the Python app's texts name AI;
//! they are shown in a wording that does not.
//!
//! Only the presentation changes. Checks keep emitting the Python app's
//! summaries, so the stored findings and their upsert key
//! `(check_id, album_id, summary)` stay the same as in a database the Python
//! app wrote: an existing finding is neither duplicated nor orphaned, and an
//! ignored finding stays ignored. The runner's API shapes
//! ([`crate::runner::finding_to_json`], [`crate::runner::meta_to_json`])
//! apply these functions.

use std::borrow::Cow;

use crate::base::CheckMeta;
use crate::checks::tags_out_of_sync::{NO_ANCHOR_SUFFIX, NO_ANCHOR_SUFFIX_NEUTRAL};

/// Check descriptions that name AI, reworded: the same text without the AI
/// fallback.
pub const NEUTRAL_DESCRIPTIONS: [(&str, &str); 3] = [
    (
        "no-cover-art",
        "Albums with no cover: none embedded in the files and no cover.jpg, folder.jpg or front.jpg in the \
folder. Fix looks for one on MusicBrainz and the Cover Art Archive. An album whose lookup failed \
recently is skipped for a while, so a bulk run does not keep retrying it.",
    ),
    (
        "track-gaps",
        "Albums whose track numbers skip (1, 2, 4) or repeat (1, 2, 2). This looks only at the track number \
tags; the same audio stored twice is found by \"Duplicate audio files\". Fix works out the right \
numbers by matching titles against MusicBrainz, then by looking up each track's audio fingerprint on \
AcoustID.",
    ),
    (
        "tags-out-of-sync",
        "Albums whose tags differ from what MusicBrainz has for the release, lack a title, artist, year or \
genre, or lack the sort names players such as Plex and Roon use to alphabetise. Fix applies \
everything MusicBrainz can supply in one step; what it cannot supply (genre, or any field on an \
album not yet matched to MusicBrainz) is left as it is.",
    ),
];

/// A check's description as shown.
pub fn description(meta: &CheckMeta) -> &'static str {
    NEUTRAL_DESCRIPTIONS
        .iter()
        .find(|(id, _)| *id == meta.id)
        .map_or(meta.description, |(_, d)| d)
}

/// A finding's summary as shown. Only tags-out-of-sync summaries of
/// unanchored albums name AI (their [`NO_ANCHOR_SUFFIX`]).
pub fn finding_summary<'a>(check_id: &str, summary: &'a str) -> Cow<'a, str> {
    if check_id != crate::checks::tags_out_of_sync::META.id {
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

    // No shown description names AI, and only the three that did are
    // reworded.
    #[test]
    fn neutral_descriptions_name_no_ai() {
        let reg = Registry::default();
        let mut reworded = 0;
        for m in reg.all_meta() {
            let n = description(m);
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
        assert_eq!(
            finding_summary(id, &stored),
            "missing: genre · 1 canonical drift (no MusicBrainz match yet)"
        );
        let anchored = build_summary(&["genre"], 1, 0, 0, true);
        assert_eq!(finding_summary(id, &anchored), anchored);
        // Another check's summary is never touched.
        assert_eq!(finding_summary("track-gaps", &stored), stored);
    }
}
