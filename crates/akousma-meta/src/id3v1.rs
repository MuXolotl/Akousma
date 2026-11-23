//! ID3v1 and ID3v1.1 tags and the `TAG+` extension.
//!
//! An ID3v1 tag is the last [`TAG_LEN`] bytes of a file. All text is
//! ISO-8859-1, padded with `0x00` or spaces:
//!
//! | Bytes | Field |
//! | :--- | :--- |
//! | `0..3` | `"TAG"` |
//! | `3..33` | title |
//! | `33..63` | artist |
//! | `63..93` | album |
//! | `93..97` | year |
//! | `97..127` | comment; in ID3v1.1, if byte 125 is `0` and byte 126 is not, the comment is `97..125` and byte 126 is the track number |
//! | `127` | genre index into [`GENRES`], `255` for none |
//!
//! The unofficial `TAG+` extension is a block of [`EXTENDED_TAG_LEN`] bytes
//! right before the ID3v1 tag. It continues the title, artist and album by 60
//! bytes each and adds a free-text genre, a speed and start and end times:
//!
//! | Bytes | Field |
//! | :--- | :--- |
//! | `0..4` | `"TAG+"` |
//! | `4..64` | title, continued |
//! | `64..124` | artist, continued |
//! | `124..184` | album, continued |
//! | `184` | speed: `0` unset, `1` slow, `2` medium, `3` fast, `4` hardcore |
//! | `185..215` | genre name |
//! | `215..221` | start time, `mmm:ss` |
//! | `221..227` | end time, `mmm:ss` |
//!
//! Empty fields are left out of the revision.
//!
//! | Raw key | Raw value | Normalized |
//! | :--- | :--- | :--- |
//! | `Title` | text | [`TrackTitle`](StandardTag::TrackTitle) |
//! | `Artist` | text | [`Artist`](StandardTag::Artist) |
//! | `Album` | text | [`Album`](StandardTag::Album) |
//! | `Year` | text | [`ReleaseDate`](StandardTag::ReleaseDate) if four digits |
//! | `Comment` | text | [`Comment`](StandardTag::Comment) |
//! | `Track` | integer | [`TrackNumber`](StandardTag::TrackNumber) |
//! | `Genre` | integer (the index) | [`Genre`](StandardTag::Genre) if the index is in [`GENRES`] |
//! | `Speed` | integer | none |
//! | `GenreName` | text | [`Genre`](StandardTag::Genre) |
//! | `StartTime`, `EndTime` | text | none |
//!
//! A `TAG+` block can carry both a genre index and a free-text genre. Both
//! keep their normalized [`Genre`](StandardTag::Genre), in stored order: the
//! index first, then the usually more specific `GenreName`.

use akousma_core::meta::{MetadataRevision, RawValue, StandardTag, Tag};
use akousma_core::{Error, ErrorKind};

use crate::encoding::{latin1, split_nul};
use crate::{TextTag, new_tag};

/// Length of an ID3v1 tag in bytes.
pub const TAG_LEN: usize = 128;

/// Length of a `TAG+` extension block in bytes.
pub const EXTENDED_TAG_LEN: usize = 227;

/// First bytes of an ID3v1 tag.
pub const MAGIC: [u8; 3] = *b"TAG";

/// First bytes of a `TAG+` extension block.
pub const EXTENDED_MAGIC: [u8; 4] = *b"TAG+";

/// Genre index that means "no genre".
const GENRE_NONE: u8 = 255;

/// Genre names by ID3v1 genre index.
///
/// Indices `0..=79` are the original ID3v1 list; `80..=191` are Winamp
/// extensions, of which `80..=125` are also listed in appendix A of the
/// ID3v2.3 informal standard and `148..=191` were added in Winamp 5.6.
///
/// Every name is spelled as in the Winamp list, which corrects some
/// spellings of the original list and of appendix A (for example index 40 is
/// `"Alt Rock"`, not `"AlternRock"`; index 67 is `"Psychedelic"`, not
/// `"Psychadelic"`; index 85 is `"Bebop"`, not `"Bebob"`) and renames
/// index 133 to `"Afro-Punk"`. Other software may show different names for
/// these indices; the raw value of a `Genre` tag is always the index.
pub const GENRES: [&str; 192] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "Alt Rock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta Rap",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychedelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast-Fusion",
    "Bebop",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A Cappella",
    "Euro-House",
    "Dance Hall",
    "Goa",
    "Drum & Bass",
    "Club-House",
    "Hardcore",
    "Terror",
    "Indie",
    "BritPop",
    "Afro-Punk",
    "Polsk Punk",
    "Beat",
    "Christian Gangsta Rap",
    "Heavy Metal",
    "Black Metal",
    "Crossover",
    "Contemporary Christian",
    "Christian Rock",
    "Merengue",
    "Salsa",
    "Thrash Metal",
    "Anime",
    "JPop",
    "Synthpop",
    "Abstract",
    "Art Rock",
    "Baroque",
    "Bhangra",
    "Big Beat",
    "Breakbeat",
    "Chillout",
    "Downtempo",
    "Dub",
    "EBM",
    "Eclectic",
    "Electro",
    "Electroclash",
    "Emo",
    "Experimental",
    "Garage",
    "Global",
    "IDM",
    "Illbient",
    "Industro-Goth",
    "Jam Band",
    "Krautrock",
    "Leftfield",
    "Lounge",
    "Math Rock",
    "New Romantic",
    "Nu-Breakz",
    "Post-Punk",
    "Post-Rock",
    "Psytrance",
    "Shoegaze",
    "Space Rock",
    "Trop Rock",
    "World Music",
    "Neoclassical",
    "Audiobook",
    "Audio Theatre",
    "Neue Deutsche Welle",
    "Podcast",
    "Indie Rock",
    "G-Funk",
    "Dubstep",
    "Garage Rock",
    "Psybient",
];

/// Returns the name of ID3v1 genre `index`, or `None` for `255` (no genre)
/// and indices without a name.
///
/// ```
/// use akousma_meta::id3v1::genre_name;
///
/// assert_eq!(genre_name(17), Some("Rock"));
/// assert_eq!(genre_name(191), Some("Psybient"));
/// assert_eq!(genre_name(192), None);
/// assert_eq!(genre_name(255), None);
/// ```
#[must_use]
pub fn genre_name(index: u8) -> Option<&'static str> {
    GENRES.get(usize::from(index)).copied()
}

/// Parses an ID3v1 or ID3v1.1 tag.
///
/// ```
/// use akousma_core::meta::StandardTag;
/// use akousma_meta::id3v1::{self, TAG_LEN};
///
/// let mut tag = [0u8; TAG_LEN];
/// tag[..3].copy_from_slice(b"TAG");
/// tag[3..8].copy_from_slice(b"Title");
/// tag[93..97].copy_from_slice(b"1999");
/// tag[126] = 4; // ID3v1.1 track number
/// tag[127] = 17;
///
/// let revision = id3v1::parse(&tag)?;
/// let standard: Vec<_> = revision.tags.into_iter().filter_map(|t| t.standard).collect();
/// assert_eq!(
///     standard,
///     [
///         StandardTag::TrackTitle("Title".into()),
///         StandardTag::ReleaseDate("1999".into()),
///         StandardTag::TrackNumber(4),
///         StandardTag::Genre("Rock".into()),
///     ]
/// );
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// [`ErrorKind::InvalidHeader`] at offset 0 if `tag` does not start with
/// [`MAGIC`].
pub fn parse(tag: &[u8; TAG_LEN]) -> Result<MetadataRevision, Error> {
    parse_fields(tag, None)
}

/// Parses a `TAG+` extension block and the ID3v1 tag that follows it.
///
/// The extended title, artist and album are appended to the ID3v1 fields.
///
/// ```
/// use akousma_core::meta::StandardTag;
/// use akousma_meta::id3v1::{self, EXTENDED_TAG_LEN, TAG_LEN};
///
/// let title = "A title that is longer than the thirty bytes of ID3v1";
/// let mut extension = [0u8; EXTENDED_TAG_LEN];
/// extension[..4].copy_from_slice(b"TAG+");
/// extension[4..4 + title.len() - 30].copy_from_slice(&title.as_bytes()[30..]);
/// extension[185..190].copy_from_slice(b"Synth");
/// let mut tag = [0u8; TAG_LEN];
/// tag[..3].copy_from_slice(b"TAG");
/// tag[3..33].copy_from_slice(&title.as_bytes()[..30]);
/// tag[127] = 255;
///
/// let revision = id3v1::parse_extended(&extension, &tag)?;
/// assert_eq!(revision.tags[0].standard, Some(StandardTag::TrackTitle(title.into())));
/// assert_eq!(revision.tags[1].standard, Some(StandardTag::Genre("Synth".into())));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// [`ErrorKind::InvalidHeader`] at offset 0 if `extension` does not start
/// with [`EXTENDED_MAGIC`], or at offset [`EXTENDED_TAG_LEN`] (the start of
/// `tag` when both are read as one block) if `tag` does not start with
/// [`MAGIC`].
pub fn parse_extended(
    extension: &[u8; EXTENDED_TAG_LEN],
    tag: &[u8; TAG_LEN],
) -> Result<MetadataRevision, Error> {
    if !extension.starts_with(&EXTENDED_MAGIC) {
        return Err(Error::at(ErrorKind::InvalidHeader, 0));
    }
    parse_fields(tag, Some(extension)).map_err(|err| Error::at(err.kind, EXTENDED_TAG_LEN as u64))
}

/// Text of a fixed-size field and its optional continuation: each part ends
/// at its first `0x00`, trailing spaces of the joined text are removed.
fn text(field: &[u8], continuation: Option<&[u8]>) -> String {
    let mut text = latin1(split_nul(field).0).into_owned();
    if let Some(tail) = continuation {
        text.push_str(&latin1(split_nul(tail).0));
    }
    let trimmed = text.trim_end_matches(' ').len();
    text.truncate(trimmed);
    text
}

/// Appends a text field with its normalized meaning, unless it is empty.
fn push_text(tags: &mut Vec<Tag>, key: &str, value: String, standard: TextTag) {
    if !value.is_empty() {
        let normalized = standard(value.clone());
        tags.push(new_tag(key, RawValue::Text(value), Some(normalized)));
    }
}

/// Parses the fields of `tag` and of its `TAG+` extension, if any.
fn parse_fields(
    tag: &[u8; TAG_LEN],
    extension: Option<&[u8; EXTENDED_TAG_LEN]>,
) -> Result<MetadataRevision, Error> {
    if !tag.starts_with(&MAGIC) {
        return Err(Error::at(ErrorKind::InvalidHeader, 0));
    }
    let mut tags = Vec::new();
    let ext = |range: core::ops::Range<usize>| extension.map(|e| &e[range]);
    push_text(
        &mut tags,
        "Title",
        text(&tag[3..33], ext(4..64)),
        StandardTag::TrackTitle,
    );
    push_text(
        &mut tags,
        "Artist",
        text(&tag[33..63], ext(64..124)),
        StandardTag::Artist,
    );
    push_text(
        &mut tags,
        "Album",
        text(&tag[63..93], ext(124..184)),
        StandardTag::Album,
    );

    let year = text(&tag[93..97], None);
    if !year.is_empty() {
        let date = (year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()))
            .then(|| StandardTag::ReleaseDate(year.clone()));
        tags.push(new_tag("Year", RawValue::Text(year), date));
    }

    // ID3v1.1: a zero byte before a non-zero track number.
    let (comment, track) = match (tag[125], tag[126]) {
        (0, track @ 1..) => (&tag[97..125], Some(track)),
        _ => (&tag[97..127], None),
    };
    push_text(
        &mut tags,
        "Comment",
        text(comment, None),
        StandardTag::Comment,
    );
    if let Some(track) = track {
        tags.push(new_tag(
            "Track",
            RawValue::Int(i64::from(track)),
            Some(StandardTag::TrackNumber(u32::from(track))),
        ));
    }

    let genre = tag[127];
    if genre != GENRE_NONE {
        let name = genre_name(genre).map(|name| StandardTag::Genre(name.to_owned()));
        tags.push(new_tag("Genre", RawValue::Int(i64::from(genre)), name));
    }

    if let Some(extension) = extension {
        let speed = extension[184];
        if speed != 0 {
            tags.push(new_tag("Speed", RawValue::Int(i64::from(speed)), None));
        }
        push_text(
            &mut tags,
            "GenreName",
            text(&extension[185..215], None),
            StandardTag::Genre,
        );
        for (key, range) in [("StartTime", 215..221), ("EndTime", 221..227)] {
            let time = text(&extension[range], None);
            if !time.is_empty() {
                tags.push(new_tag(key, RawValue::Text(time), None));
            }
        }
    }

    Ok(MetadataRevision {
        tags,
        ..MetadataRevision::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_with(fields: &[(usize, &[u8])]) -> [u8; TAG_LEN] {
        let mut tag = [0u8; TAG_LEN];
        tag[..3].copy_from_slice(&MAGIC);
        tag[127] = GENRE_NONE;
        for (at, bytes) in fields {
            tag[*at..*at + bytes.len()].copy_from_slice(bytes);
        }
        tag
    }

    fn summary(revision: &MetadataRevision) -> Vec<(&str, RawValue, Option<StandardTag>)> {
        revision
            .tags
            .iter()
            .map(|t| (t.raw.key.as_str(), t.raw.value.clone(), t.standard.clone()))
            .collect()
    }

    #[test]
    fn genre_table() {
        let spot_checks = [
            (0, "Blues"),
            (40, "Alt Rock"),
            (59, "Gangsta Rap"),
            (67, "Psychedelic"),
            (79, "Hard Rock"),
            (80, "Folk"),
            (84, "Fast-Fusion"),
            (85, "Bebop"),
            (123, "A Cappella"),
            (125, "Dance Hall"),
            (126, "Goa"),
            (133, "Afro-Punk"),
            (147, "Synthpop"),
            (148, "Abstract"),
            (191, "Psybient"),
        ];
        for (index, name) in spot_checks {
            assert_eq!(genre_name(index), Some(name));
        }
        assert!((192..=255).all(|index| genre_name(index).is_none()));
    }

    #[test]
    fn id3v1_fields() {
        // Comment fills all 30 bytes: ID3v1.0, no track number.
        let tag = tag_with(&[
            (3, b"Title with spaces   "),
            (33, b"Caf\xE9 \xA9"),
            (63, b"Album\0garbage"),
            (93, b"1987"),
            (97, b"A comment that is thirty bytes"),
            (127, &[148]),
        ]);
        let text = |s: &str| RawValue::Text(s.into());
        assert_eq!(
            summary(&parse(&tag).unwrap()),
            [
                (
                    "Title",
                    text("Title with spaces"),
                    Some(StandardTag::TrackTitle("Title with spaces".into()))
                ),
                (
                    "Artist",
                    text("Caf\u{E9} \u{A9}"),
                    Some(StandardTag::Artist("Caf\u{E9} \u{A9}".into()))
                ),
                (
                    "Album",
                    text("Album"),
                    Some(StandardTag::Album("Album".into()))
                ),
                (
                    "Year",
                    text("1987"),
                    Some(StandardTag::ReleaseDate("1987".into()))
                ),
                (
                    "Comment",
                    text("A comment that is thirty bytes"),
                    Some(StandardTag::Comment(
                        "A comment that is thirty bytes".into()
                    ))
                ),
                (
                    "Genre",
                    RawValue::Int(148),
                    Some(StandardTag::Genre("Abstract".into()))
                ),
            ]
        );

        // Empty fields, blank and non-numeric years, unknown genres.
        assert!(parse(&tag_with(&[])).unwrap().tags.is_empty());
        assert!(
            parse(&tag_with(&[(3, b"    "), (93, b"    ")]))
                .unwrap()
                .tags
                .is_empty()
        );
        assert_eq!(
            summary(&parse(&tag_with(&[(93, b"19x9")])).unwrap()),
            [("Year", text("19x9"), None)]
        );
        assert_eq!(
            summary(&parse(&tag_with(&[(93, b"99")])).unwrap()),
            [("Year", text("99"), None)]
        );
        assert_eq!(
            summary(&parse(&tag_with(&[(127, &[192])])).unwrap()),
            [("Genre", RawValue::Int(192), None)]
        );
        assert_eq!(
            summary(&parse(&tag_with(&[(127, &[0])])).unwrap()),
            [(
                "Genre",
                RawValue::Int(0),
                Some(StandardTag::Genre("Blues".into()))
            )]
        );

        let err = parse(&[0u8; TAG_LEN]).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(0))
        );
    }

    #[test]
    fn id3v1_1_track_number() {
        // Byte 125 zero, byte 126 non-zero: 28-byte comment and a track.
        let tag = tag_with(&[(97, b"Twenty-eight bytes of commen"), (126, &[7])]);
        assert_eq!(
            summary(&parse(&tag).unwrap()),
            [
                (
                    "Comment",
                    RawValue::Text("Twenty-eight bytes of commen".into()),
                    Some(StandardTag::Comment("Twenty-eight bytes of commen".into()))
                ),
                ("Track", RawValue::Int(7), Some(StandardTag::TrackNumber(7))),
            ]
        );
        let tag = tag_with(&[(126, &[255])]);
        assert_eq!(
            summary(&parse(&tag).unwrap()),
            [(
                "Track",
                RawValue::Int(255),
                Some(StandardTag::TrackNumber(255))
            )]
        );

        // Both zero: an ID3v1.0 comment without a track.
        let tag = tag_with(&[(97, b"Short")]);
        assert_eq!(summary(&parse(&tag).unwrap()).len(), 1);

        // Byte 125 non-zero: byte 126 is part of the comment.
        let tag = tag_with(&[(97, b"Comment of 29 bytes padded xz"), (126, b"y")]);
        let expected = "Comment of 29 bytes padded xzy";
        assert_eq!(
            summary(&parse(&tag).unwrap()),
            [(
                "Comment",
                RawValue::Text(expected.into()),
                Some(StandardTag::Comment(expected.into()))
            )]
        );
    }

    #[test]
    fn tag_plus_extension() {
        let long_title =
            "Thirty bytes of title text ok and sixty more bytes in the extension block!!!";
        let (head, tail) = long_title.as_bytes().split_at(30);
        let mut extension = [0u8; EXTENDED_TAG_LEN];
        extension[..4].copy_from_slice(&EXTENDED_MAGIC);
        extension[4..4 + tail.len()].copy_from_slice(tail);
        extension[64..70].copy_from_slice(b"Ignore");
        extension[184] = 2;
        extension[185..203].copy_from_slice(b"Progressive Trance");
        extension[215..221].copy_from_slice(b"001:30");
        extension[221..227].copy_from_slice(b"003:15");
        // Artist field shorter than 30 bytes: its continuation still follows the
        // first 0x00, as written by taggers that fill both blocks.
        let tag = tag_with(&[(3, head), (33, b"Artist"), (127, &[31])]);

        let revision = parse_extended(&extension, &tag).unwrap();
        let text = |s: &str| RawValue::Text(s.into());
        assert_eq!(
            summary(&revision),
            [
                (
                    "Title",
                    text(long_title),
                    Some(StandardTag::TrackTitle(long_title.into()))
                ),
                (
                    "Artist",
                    text("ArtistIgnore"),
                    Some(StandardTag::Artist("ArtistIgnore".into()))
                ),
                (
                    "Genre",
                    RawValue::Int(31),
                    Some(StandardTag::Genre("Trance".into()))
                ),
                ("Speed", RawValue::Int(2), None),
                (
                    "GenreName",
                    text("Progressive Trance"),
                    Some(StandardTag::Genre("Progressive Trance".into()))
                ),
                ("StartTime", text("001:30"), None),
                ("EndTime", text("003:15"), None),
            ]
        );

        let mut bad = extension;
        bad[3] = b'-';
        let err = parse_extended(&bad, &tag).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(0))
        );
        let err = parse_extended(&extension, &[0u8; TAG_LEN]).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(227))
        );
    }
}
