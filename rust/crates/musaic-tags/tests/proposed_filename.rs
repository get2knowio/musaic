//! The track-number rewrite of a file name's leading prefix.

use musaic_tags::proposed_filename;

#[test]
fn a_plain_prefix_is_zero_padded_to_two_digits() {
    assert_eq!(
        proposed_filename("05 - Song.flac", "7").as_deref(),
        Some("07 - Song.flac")
    );
    assert_eq!(
        proposed_filename("5. Song.flac", "12").as_deref(),
        Some("12. Song.flac")
    );
}

/// Three-digit numbers are not truncated, on either side of the rewrite.
#[test]
fn track_numbers_of_one_hundred_or_more_keep_every_digit() {
    assert_eq!(
        proposed_filename("099 - Song.flac", "100").as_deref(),
        Some("100 - Song.flac")
    );
    assert_eq!(
        proposed_filename("100 - Song.flac", "5").as_deref(),
        Some("05 - Song.flac")
    );
    assert_eq!(
        proposed_filename("100_Song.flac", "101").as_deref(),
        Some("101_Song.flac")
    );
}

/// A `disc-track` or `disc.track` prefix is one prefix: it is replaced whole
/// by the new track number, not left half behind.
#[test]
fn a_disc_and_track_prefix_is_replaced_whole() {
    assert_eq!(
        proposed_filename("1-05 - Song.flac", "6").as_deref(),
        Some("06 - Song.flac")
    );
    assert_eq!(
        proposed_filename("2.13 Song.flac", "14").as_deref(),
        Some("14 Song.flac")
    );
    assert_eq!(
        proposed_filename("1-105 - Song.flac", "106").as_deref(),
        Some("106 - Song.flac")
    );
}

/// Without a numeric prefix followed by a separator there is nothing to
/// rewrite, and a track number that is not a number proposes nothing.
#[test]
fn names_without_a_prefix_and_numbers_that_are_not_numbers_propose_nothing() {
    assert_eq!(proposed_filename("Song.flac", "5"), None);
    assert_eq!(proposed_filename("05Song.flac", "6"), None);
    assert_eq!(proposed_filename("05 - Song.flac", "x"), None);
}
