//! Feeds bytes to the tag parsers of `akousma-meta` directly, without a
//! container around them.
//!
//! Input: one selector byte, then the bytes of the tag. The selector modulo 7
//! picks the parser:
//!
//! - 0: the scanner of `anchors::scan` over the bytes as a whole file, which
//!   reads leading ID3v2 and APE tags and trailing APE, ID3v1 (including
//!   `TAG+`) and Lyrics3 tags;
//! - 1: `id3v2::parse`;
//! - 2: `ape::ApeHeader::parse` on the first 32 bytes, then `ape::parse_items`
//!   on the rest;
//! - 3: `id3v1::parse` on the first 128 bytes;
//! - 4: `id3v1::parse_extended` on the first 227 bytes and the 128 after them;
//! - 5: `lyrics3::parse_v2`, the last 128 bytes as the ID3v1 tag after it;
//! - 6: `riff::parse_list_info`.
//!
//! An input too short for the fixed-size block of its parser is skipped.

#![no_main]

use std::io::Cursor;

use akousma::{DemuxOptions, io::ByteReader, meta::MetadataBudget};
use akousma_meta::{anchors, ape, id3v1, id3v2, lyrics3, riff};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&selector, tag)) = data.split_first() else {
        return;
    };
    match selector % 7 {
        0 => {
            let options = DemuxOptions::new();
            let mut budget = MetadataBudget::new(&options.limits);
            let mut reader = ByteReader::new(Cursor::new(tag));
            let _ = anchors::scan(&mut reader, tag.len() as u64, &options, &mut budget);
        }
        1 => {
            let _ = id3v2::parse(tag);
        }
        2 => {
            let Some((header, items)) = tag.split_first_chunk::<{ ape::HEADER_LEN }>() else {
                return;
            };
            if let Ok(header) = ape::ApeHeader::parse(header) {
                let _ = ape::parse_items(&header, items);
            }
        }
        3 => {
            if let Some(block) = tag.first_chunk::<{ id3v1::TAG_LEN }>() {
                let _ = id3v1::parse(block);
            }
        }
        4 => {
            let Some((extension, rest)) = tag.split_first_chunk::<{ id3v1::EXTENDED_TAG_LEN }>()
            else {
                return;
            };
            if let Some(block) = rest.first_chunk::<{ id3v1::TAG_LEN }>() {
                let _ = id3v1::parse_extended(extension, block);
            }
        }
        5 => {
            if let Some((lyrics, block)) = tag.split_last_chunk::<{ id3v1::TAG_LEN }>() {
                let _ = lyrics3::parse_v2(lyrics, block);
            }
        }
        _ => {
            let _ = riff::parse_list_info(tag);
        }
    }
});
