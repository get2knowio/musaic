# Metadata evidence

## MusicBrainz identifiers

MusicBrainz names three things that are easy to mix up. A result lists each identifier with its type.

- A **release group** is the abstract album ("the album as an idea"), shared by every edition.
- A **release** is one edition: a country, a date, a track list, a barcode. It has its own identifier.
- A **recording** is one performance, shared by every release that carries it.

Submit the release that matches the files, plus its release group. Use recording identifiers only for track-level changes. All identifiers are lowercase UUIDs. Do not invent one; leave it out if you did not read it from MusicBrainz.

## Aligning the track list

Compare the files with the candidate release before you trust it:

1. The number of tracks must match, per disc.
2. Track titles must match after ignoring case, punctuation and common suffixes such as "(Remastered)".
3. Durations should agree within a few seconds. A large gap means a different edition.
4. A **multi-disc** set may be stored as one folder with disc numbers, or as several folders. Align each disc on its own and check that the disc count equals the release's. Do not merge discs by title order alone.

If two editions fit equally well (a standard and a deluxe edition with the same first tracks), say so as an uncertainty and prefer the one whose total track count matches.

## Evidence

Every claim in a proposal needs a record: where you read it (a MusicBrainz page, a label page), when, what it says, and its limits ("tracklist only; no barcode"). Evidence records are short and factual.

## Stating uncertainty

- Put what you could not check in `uncertainties`, one plain sentence each.
- Set `confidence` between 0 and 1 for the recommendation as a whole. Below about 0.6, prefer `needs_input`.
- Never hide a doubt to look more certain. The person decides, and a stated doubt helps them.

## Worked examples (synthetic albums)

**Successful.** Album "Example Artist - Sample Record", ten tracks, one disc. One MusicBrainz release matches the titles and the count; its release group is clear. Result: outcome `proposal`, confidence 0.9, two identifiers (release group and release), album fields for title and year, two evidence records, one uncertainty ("year taken from the earliest release; a reissue has a later year").

**Ambiguous.** Album "Sample Duo - Test Tapes", twelve tracks. A standard edition has twelve tracks and a deluxe edition has sixteen whose first twelve are identical. Both fit. Result: outcome `needs_input`, an unresolved recommendation whose explanation names both editions and whose question asks which edition the person owns, confidence 0.5.

**No match.** Album "Nobody - Unreleased Demo", six tracks with generic titles. MusicBrainz has nothing under that name and no release has six tracks with those durations. Result: outcome `no_match`, an unresolved recommendation that says what you searched and that nothing matched, an empty or short evidence list, confidence 0.
