//! Snappy decompression, which is how a HAP frame is stored.
//!
//! Only the decompressing half, and only the raw format — not the framed one
//! with its stream header and checksums, because HAP does not use that.
//!
//! # Why this is here rather than a dependency
//!
//! There is no network to fetch one from on the machine this builds on, and
//! this is eighty lines of a format that has not changed since 2011. What
//! matters more is that every byte of it is bounds-checked: a frame arrives
//! from a file an operator imported, and a decompressor that trusted its input
//! would be a way to take the renderer down by putting a bad clip in a slot.
//!
//! So: no `unsafe`, no indexing without a check, and a refusal rather than a
//! panic for every malformed input the format allows — a length that runs off
//! the end, an offset that points before the start, a preamble that promises
//! more than the frame can hold.
//!
//! # The format, for the reader
//!
//! A varint saying how long the result is, then elements until it is that
//! long. Each element starts with a tag byte whose low two bits say which of
//! four it is: a run of literal bytes, or a copy of something already
//! decompressed, with the copy's offset held in one, two or four bytes.
//!
//! A copy may overlap what it is copying — `abcabcabc` is stored as three
//! literals and a copy of six from three back — so it is written a byte at a
//! time rather than with a block move. That is the one place where the obvious
//! optimisation is wrong.

/// What a frame could not be read as.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnappyError {
    /// The length preamble ran off the end of the input, or was absurd.
    #[error("the compressed block does not begin with a length")]
    NoLength,
    /// The block claims to decompress to more than this build will hold.
    #[error(
        "the compressed block claims to hold {wanted} bytes, and the most this reads is {most}"
    )]
    TooLarge {
        /// What the preamble said.
        wanted: u64,
        /// The ceiling.
        most: u64,
    },
    /// An element wanted bytes the block does not have.
    #[error("the compressed block ends in the middle of an element")]
    Truncated,
    /// A copy pointed further back than anything decompressed so far.
    #[error("a copy points {offset} bytes back into {written} bytes of output")]
    BadCopy {
        /// Where it pointed.
        offset: usize,
        /// How much there was to point into.
        written: usize,
    },
    /// The elements produced a different amount from what the preamble said.
    #[error("the block promised {wanted} bytes and produced {produced}")]
    WrongLength {
        /// What the preamble said.
        wanted: usize,
        /// What came out.
        produced: usize,
    },
}

/// The largest frame this will decompress.
///
/// A 8192x8192 BC7 texture is 64 MiB, which is past anything a show will hold
/// and far short of what a corrupt four-byte length could ask for. The ceiling
/// is here so that a bad preamble is refused rather than allocated.
const MOST: u64 = 256 * 1024 * 1024;

/// Decompresses one Snappy block into `out`, which is cleared first.
///
/// # Errors
///
/// Returns [`SnappyError`] for any input that does not decompress exactly: a
/// length that overruns, a copy that points before the start, a block that
/// produces a different amount from what it promised.
pub fn decompress(input: &[u8], out: &mut Vec<u8>) -> Result<(), SnappyError> {
    let (wanted, mut at) = length(input)?;
    if wanted > MOST {
        return Err(SnappyError::TooLarge { wanted, most: MOST });
    }
    let wanted = wanted as usize;

    out.clear();
    out.reserve(wanted);

    while at < input.len() {
        let tag = input[at];
        at += 1;

        if tag & 0b11 == 0 {
            // A run of literal bytes. The length is one more than the top six
            // bits of the tag, except that 60 to 63 mean "the length is in the
            // next one to four bytes instead".
            let first = usize::from(tag >> 2);
            let (length, read) = if first < 60 {
                (first + 1, 0)
            } else {
                let extra = first - 59;
                let bytes = input.get(at..at + extra).ok_or(SnappyError::Truncated)?;
                let mut value: usize = 0;
                for (index, byte) in bytes.iter().enumerate() {
                    value |= usize::from(*byte) << (8 * index);
                }
                (value.checked_add(1).ok_or(SnappyError::Truncated)?, extra)
            };
            at += read;

            let end = at.checked_add(length).ok_or(SnappyError::Truncated)?;
            let literal = input.get(at..end).ok_or(SnappyError::Truncated)?;
            out.extend_from_slice(literal);
            at = end;
            continue;
        }

        // A copy. Which of the three it is decides how the length and the
        // offset are spelled, and nothing else.
        let (length, offset) = match tag & 0b11 {
            1 => {
                let low = *input.get(at).ok_or(SnappyError::Truncated)?;
                at += 1;
                let length = 4 + usize::from((tag >> 2) & 0b111);
                let offset = (usize::from(tag >> 5) << 8) | usize::from(low);
                (length, offset)
            }
            2 => {
                let bytes = input.get(at..at + 2).ok_or(SnappyError::Truncated)?;
                at += 2;
                let offset = usize::from(bytes[0]) | (usize::from(bytes[1]) << 8);
                (usize::from(tag >> 2) + 1, offset)
            }
            _ => {
                let bytes = input.get(at..at + 4).ok_or(SnappyError::Truncated)?;
                at += 4;
                let offset = usize::from(bytes[0])
                    | (usize::from(bytes[1]) << 8)
                    | (usize::from(bytes[2]) << 16)
                    | (usize::from(bytes[3]) << 24);
                (usize::from(tag >> 2) + 1, offset)
            }
        };

        if offset == 0 || offset > out.len() {
            return Err(SnappyError::BadCopy {
                offset,
                written: out.len(),
            });
        }

        // A byte at a time, because a copy may overlap what it copies: `abcabc`
        // is three literals and a copy of three from three back, and a block
        // move would read bytes that have not been written yet.
        let from = out.len() - offset;
        for step in 0..length {
            let byte = out[from + step];
            out.push(byte);
        }
    }

    if out.len() == wanted {
        Ok(())
    } else {
        Err(SnappyError::WrongLength {
            wanted,
            produced: out.len(),
        })
    }
}

/// Reads the varint length preamble, returning it and how many bytes it took.
fn length(input: &[u8]) -> Result<(u64, usize), SnappyError> {
    let mut value: u64 = 0;
    for (index, byte) in input.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(SnappyError::NoLength)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decompresses, or says what went wrong.
    fn read(input: &[u8]) -> Result<Vec<u8>, SnappyError> {
        let mut out = Vec::new();
        decompress(input, &mut out)?;
        Ok(out)
    }

    #[test]
    fn a_literal_run_comes_back_as_itself() {
        // Length 5, then one literal element of five bytes.
        let block = [5, 4 << 2, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(read(&block).expect("reads"), b"hello");
    }

    #[test]
    fn a_copy_repeats_what_came_before() {
        // "abcdabcd": four literals, then a copy of four from four back.
        //
        // Four is the shortest copy the format has — the length is stored as
        // three bits of "how many past four" — so the obvious example,
        // "abcabc", is not expressible and a real compressor emits literals
        // for it.
        // Tag 0b00000001: a copy, offset in one byte, length four.
        let block = [8, 3 << 2, b'a', b'b', b'c', b'd', 0b0000_0001, 4];
        assert_eq!(read(&block).expect("reads"), b"abcdabcd");
    }

    #[test]
    fn a_copy_may_overlap_what_it_copies() {
        // "aaaaaaa" is one literal and a copy of six from one back — which
        // reads bytes as it writes them. A block move would produce rubbish
        // here, and this is the only reason the copy is written a byte at a
        // time.
        // Tag 0b00001001: a copy, offset in one byte, length six.
        let block = [7, 0 << 2, b'a', 0b0000_1001, 1];
        assert_eq!(read(&block).expect("reads"), b"aaaaaaa");
    }

    #[test]
    fn a_long_literal_says_its_length_in_extra_bytes() {
        // Sixty-four bytes, which is past the six bits the tag holds, so the
        // length goes in one extra byte.
        let mut block = vec![64, 60 << 2, 63];
        block.extend(std::iter::repeat_n(b'x', 64));
        assert_eq!(read(&block).expect("reads"), vec![b'x'; 64]);
    }

    #[test]
    fn a_length_that_runs_off_the_end_is_refused() {
        // Promises five bytes and holds two.
        let block = [5, 4 << 2, b'h', b'i'];
        assert!(matches!(read(&block), Err(SnappyError::Truncated)));
    }

    #[test]
    fn a_copy_from_before_the_start_is_refused() {
        // Nothing has been written, so there is nothing three bytes back. A
        // decompressor that indexed anyway would read whatever was next to the
        // buffer, which is how a bad clip becomes a hole in the process.
        let block = [6, 0b0000_0101, 3];
        assert!(matches!(read(&block), Err(SnappyError::BadCopy { .. })));
    }

    #[test]
    fn a_block_that_promises_more_than_it_delivers_is_refused() {
        // Two literals where the preamble said six. Accepting it would put a
        // short texture on the card and the rest would be whatever the
        // allocation held.
        let block = [6, 1 << 2, b'h', b'i'];
        assert!(matches!(read(&block), Err(SnappyError::WrongLength { .. })));
    }

    #[test]
    fn an_absurd_length_is_refused_rather_than_allocated() {
        // Four gigabytes, from four bytes of a corrupt file.
        let block = [0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
        assert!(matches!(read(&block), Err(SnappyError::TooLarge { .. })));
    }

    #[test]
    fn an_empty_block_is_an_empty_result() {
        assert_eq!(read(&[0]).expect("reads"), Vec::<u8>::new());
    }

    #[test]
    fn nothing_at_all_is_refused_rather_than_read_as_empty() {
        assert!(matches!(read(&[]), Err(SnappyError::NoLength)));
    }
}
