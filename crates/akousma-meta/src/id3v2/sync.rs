//! Synchsafe integers and unsynchronisation.

/// Value of a synchsafe integer, or `None` if a byte is `0x80` or more.
pub(super) fn synchsafe(bytes: [u8; 4]) -> Option<u32> {
    if u32::from_be_bytes(bytes) & 0x8080_8080 != 0 {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0, |value, &byte| (value << 7) | u32::from(byte)),
    )
}

/// Synchsafe encoding of `value`, or `None` if it needs more than 28 bits.
#[cfg(test)]
pub(super) fn to_synchsafe(value: u32) -> Option<[u8; 4]> {
    (value < 1 << 28)
        .then(|| [21, 14, 7, 0].map(|shift| u8::try_from((value >> shift) & 0x7F).unwrap_or(0)))
}

/// Replaces every `FF 00` pair of `bytes` by `FF`, in place.
///
/// Words of eight bytes without `0xFF` are copied as a whole, unless the
/// byte before them is `0xFF`.
pub(super) fn remove_unsync(bytes: &mut Vec<u8>) {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;
    let len = bytes.len();
    let (mut read, mut write) = (0, 0);
    let mut prev_ff = false;
    while read < len {
        if !prev_ff {
            let word = bytes
                .get(read..read + 8)
                .and_then(|chunk| <[u8; 8]>::try_from(chunk).ok());
            if let Some(word) = word {
                // A byte of `!word` is zero exactly where `word` has `0xFF`.
                let inverted = !u64::from_ne_bytes(word);
                if inverted.wrapping_sub(ONES) & !inverted & HIGHS == 0 {
                    bytes.copy_within(read..read + 8, write);
                    read += 8;
                    write += 8;
                    continue;
                }
            }
        }
        let byte = bytes[read];
        read += 1;
        if prev_ff && byte == 0 {
            prev_ff = false;
            continue;
        }
        bytes[write] = byte;
        write += 1;
        prev_ff = byte == 0xFF;
    }
    bytes.truncate(write);
}
