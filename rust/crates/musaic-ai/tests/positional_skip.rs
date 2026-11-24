//! Tracks Tier 1 aligns by position count as resolved for the settle rule,
//! so an anchored album aligned that way gets a synthesized suggestion and
//! needs no research. The Python app has no positional alignment; this pins
//! the decided behaviour against the resolver's own positional output, not a
//! hand-set `unresolved_track_count`.

use musaic_ai::synth::{resolver_settles_metadata, unresolved_for_skip_rule};
use musaic_resolve::find_release::{MbMatch, MbTrack};
use musaic_resolve::resolution::{POSITIONAL_CONFIDENCE, refine_from_anchor};
use musaic_resolve::source::{FlacTrack, TrackNumber};
use musaic_resolve::{AlbumResolution, FieldFact};

#[test]
fn positional_matches_count_as_resolved_for_the_skip_rule() {
    let mut r = AlbumResolution::new("album");
    r.anchor = Some(FieldFact::new("rg-1", "dir-search", 0.9));
    // Every FLAC 4 s longer than its MusicBrainz track: outside the 2 s
    // duration pass, inside the positional bound.
    let titles = ["One", "Two", "Three"];
    let tracklist: Vec<MbTrack> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| MbTrack {
            position: i as i64 + 1,
            title: (*t).to_string(),
            length_ms: Some(100_000 + 20_000 * i as i64),
            artist: None,
            recording_mbid: None,
        })
        .collect();
    let flac: Vec<FlacTrack> = (0..3)
        .map(|i| FlacTrack {
            filename: format!("0{} x.flac", i + 1),
            tracknumber: TrackNumber::Missing,
            duration: Some(104.0 + 20.0 * i as f64),
        })
        .collect();
    let m = MbMatch {
        confident: true,
        score: 100,
        confidence: "high",
        mbid: Some("rg-1".into()),
        release_mbid: Some("rel-1".into()),
        tracklist,
        ..Default::default()
    };
    refine_from_anchor(&mut r, &m, &flac, &Default::default());
    // The resolver aligned every track by position...
    assert_eq!(r.tracks.len(), 3);
    for (t, want) in r.tracks.iter().zip(titles) {
        let f = t.canonical_title.as_ref().expect("aligned");
        assert_eq!(f.value, want);
        assert_eq!(f.confidence, POSITIONAL_CONFIDENCE, "positional");
    }
    // ...and the skip rule counts none of them as unresolved.
    assert_eq!(unresolved_for_skip_rule(&r), 0);
    assert!(
        resolver_settles_metadata(&r),
        "positional: settled by the resolver"
    );
}
