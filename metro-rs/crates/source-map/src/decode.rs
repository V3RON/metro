//! Decoding of the `mappings` field.
//!
//! There are two decoders because the JavaScript code base has two, with
//! different quirks that the output must preserve:
//!
//! * [`decode_columns`] is `MappingsConsumer._decodeMappings`: mappings stay in
//!   encoded order, with 1-based lines.
//! * [`decode_segments`] is what `decodeVlqMap` gets from `source-map@0.5.7`
//!   (`toBabelSegments`): mappings are sorted by generated position and the
//!   input is checked more strictly.
//!
//! Anything either decoder cannot represent exactly as JavaScript does (an
//! invalid character, a field cut off in the middle, a value over 32 bits) is
//! reported as an error. The JavaScript side then runs its own implementation,
//! which produces the exact exception, or the exact result, it always did.

use crate::vlq::{digit_value, TooLong, VlqReader, LINE_SEPARATOR, SEGMENT_SEPARATOR};
use metro_rs_ob1::{add0, get0, get1, Number0, Number1};

/// Number of `i32`s per mapping in [`Columns::data`].
pub const STRIDE: usize = 5;
/// Offsets within one mapping of [`Columns::data`]. Generated lines are not
/// stored: they are given by [`Columns::line_starts`].
pub const GENERATED_COLUMN: usize = 0;
pub const SOURCE: usize = 1;
pub const ORIGINAL_LINE: usize = 2;
pub const ORIGINAL_COLUMN: usize = 3;
pub const NAME: usize = 4;
/// The source of a mapping that has only a generated position, and the name
/// of a mapping that has no name. The other fields of such a mapping are 0.
pub const NONE: i32 = i32::MIN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// A field is missing or malformed (the JavaScript invariants fire).
    Malformed,
    /// A character outside the base64 alphabet.
    InvalidCharacter,
    /// A value that does not fit in 32 bits.
    TooLarge,
}

/// Decoded mappings as flat columns, in the order they were encoded.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Columns {
    /// `STRIDE` values per mapping: generated column, source index, original
    /// line (1-based), original column and name index.
    pub data: Vec<i32>,
    /// Index of the first mapping of every generated line (0-based line).
    pub line_starts: Vec<i32>,
}

impl Columns {
    pub fn len(&self) -> usize {
        self.data.len() / STRIDE
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Where [`decode`] puts the mappings it finds.
pub trait Sink {
    fn push_mapping(&mut self, mapping: [i32; STRIDE]);
    /// A new generated line starts after `mappings` mappings so far.
    fn push_line(&mut self, mappings: usize);
}

impl Sink for Columns {
    fn push_mapping(&mut self, mapping: [i32; STRIDE]) {
        self.data.extend_from_slice(&mapping);
    }

    fn push_line(&mut self, mappings: usize) {
        self.line_starts.push(mappings as i32);
    }
}

/// Writes into buffers owned by the caller, as far as they reach, and counts
/// everything, so that the caller can tell whether the buffers were big enough
/// and retry with bigger ones.
pub struct SliceSink<'a> {
    pub data: &'a mut [i32],
    pub line_starts: &'a mut [i32],
    pub mappings: usize,
    pub lines: usize,
}

impl Sink for SliceSink<'_> {
    #[inline]
    fn push_mapping(&mut self, mapping: [i32; STRIDE]) {
        let start = self.mappings * STRIDE;
        if let Some(slot) = self.data.get_mut(start..start + STRIDE) {
            slot.copy_from_slice(&mapping);
        }
        self.mappings += 1;
    }

    fn push_line(&mut self, mappings: usize) {
        if let Some(slot) = self.line_starts.get_mut(self.lines) {
            *slot = mappings as i32;
        }
        self.lines += 1;
    }
}

/// Port of `MappingsConsumer._decodeMappings`. Calls `sink` for the first line
/// (which starts at mapping 0), then once for every `;`.
pub fn decode<S: Sink>(mappings: &[u8], sink: &mut S) -> Result<(), DecodeError> {
    sink.push_line(0);
    let mut count = 0usize;

    let mut generated_column: Number0 = add0(0);
    let mut original_line: Number1 = Number1::FIRST;
    let mut original_column: Number0 = add0(0);
    let mut name_index: Number0 = add0(0);
    let mut source_index: Number0 = add0(0);

    let mut fields = [0i32; 5];
    let mut field_count = 0usize;
    let mut reader = VlqReader::default();

    let length = mappings.len();
    // The end of the input ends the last segment, like a `;` that starts no line.
    for (i, byte) in mappings.iter().copied().chain([LINE_SEPARATOR]).enumerate() {
        if byte == SEGMENT_SEPARATOR || byte == LINE_SEPARATOR {
            // A trailing field cut off mid-VLQ is dropped, as `vlq` does, but
            // a missing generated column is an error.
            if field_count == 0 && !reader.is_idle() {
                return Err(DecodeError::Malformed);
            }
            reader.reset();
            if field_count > 0 {
                if field_count == 2 || field_count == 3 {
                    return Err(DecodeError::Malformed);
                }
                generated_column = generated_column + fields[0];
                let mut mapping = [get0(generated_column), NONE, 0, 0, NONE];
                if field_count >= 4 {
                    source_index = source_index + fields[1];
                    original_line = original_line + fields[2];
                    original_column = original_column + fields[3];
                    mapping[SOURCE] = get0(source_index);
                    mapping[ORIGINAL_LINE] = get1(original_line);
                    mapping[ORIGINAL_COLUMN] = get0(original_column);
                    if field_count >= 5 {
                        name_index = name_index + fields[4];
                        mapping[NAME] = get0(name_index);
                    }
                }
                sink.push_mapping(mapping);
                count += 1;
                field_count = 0;
            }
            if byte == LINE_SEPARATOR && i < length {
                sink.push_line(count);
                generated_column = add0(0);
            }
            continue;
        }
        let digit = digit_value(byte).ok_or(DecodeError::InvalidCharacter)?;
        match reader.push(digit) {
            Ok(Some(value)) => {
                if field_count < 5 {
                    fields[field_count] = value;
                }
                field_count += 1;
            }
            Ok(None) => {}
            Err(TooLong) => return Err(DecodeError::TooLarge),
        }
    }
    Ok(())
}

/// [`decode`] into freshly allocated columns.
pub fn decode_columns(mappings: &[u8]) -> Result<Columns, DecodeError> {
    // Every mapping and line takes at least one character.
    let separators = mappings
        .iter()
        .filter(|&&b| b == SEGMENT_SEPARATOR || b == LINE_SEPARATOR)
        .count();
    let mut columns = Columns {
        data: Vec::with_capacity((separators + 1) * STRIDE),
        line_starts: Vec::new(),
    };
    decode(mappings, &mut columns)?;
    Ok(columns)
}

/// One mapping as `source-map@0.5.7` reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Segment {
    pub generated_line: i32,
    pub generated_column: i32,
    /// Fields present: 1, 4 or 5.
    pub kind: u8,
    pub source: i32,
    pub original_line: i32,
    pub original_column: i32,
    pub name: i32,
}

/// `util.compareByGeneratedPositionsDeflated` of `source-map@0.5.7`. Absent
/// values compare as 0 there, because it subtracts them.
fn compare_deflated(a: &Segment, b: &Segment) -> std::cmp::Ordering {
    let key = |s: &Segment| {
        (
            s.generated_line,
            s.generated_column,
            if s.kind >= 4 { s.source } else { 0 },
            if s.kind >= 4 { s.original_line } else { 0 },
            if s.kind >= 4 { s.original_column } else { 0 },
            if s.kind >= 5 { s.name } else { 0 },
        )
    };
    key(a).cmp(&key(b))
}

/// Port of `SourceMapConsumer._parseMappings` in `source-map@0.5.7`, returning
/// the generated mappings sorted the way its `eachMapping` yields them.
pub fn decode_segments(mappings: &[u8]) -> Result<Vec<Segment>, DecodeError> {
    let mut segments: Vec<Segment> = Vec::with_capacity(mappings.len() / 4);
    let mut generated_line = 1i32;
    let mut previous_generated_column = 0i32;
    let mut previous_original_line = 0i32;
    let mut previous_original_column = 0i32;
    let mut previous_source = 0i32;
    let mut previous_name = 0i32;

    let mut fields = [0i32; 5];
    let mut field_count = 0usize;
    let mut reader = VlqReader::default();

    // The end of the input ends the last segment.
    for byte in mappings.iter().copied().chain([SEGMENT_SEPARATOR]) {
        if byte == SEGMENT_SEPARATOR || byte == LINE_SEPARATOR {
            // This parser throws on a field cut off by the separator.
            if !reader.is_idle() {
                return Err(DecodeError::Malformed);
            }
            if field_count > 0 {
                if field_count == 2 || field_count == 3 {
                    return Err(DecodeError::Malformed);
                }
                let mut segment = Segment {
                    generated_line,
                    generated_column: previous_generated_column.wrapping_add(fields[0]),
                    kind: 1,
                    source: 0,
                    original_line: 0,
                    original_column: 0,
                    name: 0,
                };
                previous_generated_column = segment.generated_column;
                if field_count > 1 {
                    segment.kind = 4;
                    segment.source = previous_source.wrapping_add(fields[1]);
                    previous_source = segment.source;
                    previous_original_line = previous_original_line.wrapping_add(fields[2]);
                    segment.original_line = previous_original_line.wrapping_add(1);
                    previous_original_column = previous_original_column.wrapping_add(fields[3]);
                    segment.original_column = previous_original_column;
                    if field_count > 4 {
                        segment.kind = 5;
                        segment.name = previous_name.wrapping_add(fields[4]);
                        previous_name = segment.name;
                    }
                }
                segments.push(segment);
                field_count = 0;
            }
            if byte == LINE_SEPARATOR {
                generated_line = generated_line.wrapping_add(1);
                previous_generated_column = 0;
            }
            continue;
        }
        let digit = digit_value(byte).ok_or(DecodeError::InvalidCharacter)?;
        match reader.push(digit) {
            Ok(Some(value)) => {
                if field_count < 5 {
                    fields[field_count] = value;
                }
                field_count += 1;
            }
            Ok(None) => {}
            Err(TooLong) => return Err(DecodeError::TooLarge),
        }
    }
    if !segments
        .windows(2)
        .all(|w| compare_deflated(&w[0], &w[1]).is_le())
    {
        segments.sort_by(compare_deflated);
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(columns: &Columns) -> Vec<[i32; STRIDE]> {
        columns
            .data
            .chunks(STRIDE)
            .map(|c| c.try_into().unwrap())
            .collect()
    }

    #[test]
    fn decodes_lines_and_fields() {
        // line 1: a generated-only mapping and a named mapping; line 3: a source mapping.
        let columns = decode_columns(b"A,CAAEA;;EACC").unwrap();
        assert_eq!(columns.line_starts, vec![0, 2, 2]);
        assert_eq!(
            rows(&columns),
            vec![[0, NONE, 0, 0, NONE], [1, 0, 1, 2, 0], [2, 0, 2, 3, NONE],]
        );
    }

    #[test]
    fn empty_input_has_one_line_and_no_mappings() {
        let columns = decode_columns(b"").unwrap();
        assert!(columns.is_empty());
        assert_eq!(columns.line_starts, vec![0]);
    }

    #[test]
    fn trailing_semicolon_starts_a_new_line() {
        assert_eq!(decode_columns(b"A;").unwrap().line_starts, vec![0, 1]);
        assert_eq!(decode_columns(b"A").unwrap().line_starts, vec![0]);
    }

    #[test]
    fn a_trailing_cut_off_field_is_dropped() {
        // "g" has a continuation bit and nothing after it.
        let columns = decode_columns(b"A,Cg").unwrap();
        assert_eq!(columns.len(), 2);
        assert_eq!(decode_columns(b"g"), Err(DecodeError::Malformed));
    }

    #[test]
    fn malformed_segments_are_errors() {
        assert_eq!(decode_columns(b"AA"), Err(DecodeError::Malformed));
        assert_eq!(decode_columns(b"AAA"), Err(DecodeError::Malformed));
        assert_eq!(decode_columns(b"g,"), Err(DecodeError::Malformed));
        assert_eq!(decode_columns(b"A?"), Err(DecodeError::InvalidCharacter));
    }

    #[test]
    fn original_deltas_accumulate_across_lines_but_the_generated_column_restarts() {
        let columns = decode_columns(b"CAAC;CAAC").unwrap();
        assert_eq!(rows(&columns), vec![[1, 0, 1, 1, NONE], [1, 0, 1, 2, NONE]]);
    }

    #[test]
    fn slice_sink_counts_what_does_not_fit() {
        let mut data = [0i32; STRIDE];
        let mut line_starts = [0i32; 1];
        let mut sink = SliceSink {
            data: &mut data,
            line_starts: &mut line_starts,
            mappings: 0,
            lines: 0,
        };
        decode(b"AAAA,CAAC;AACD", &mut sink).unwrap();
        // Three mappings on two lines, room for one and one.
        assert_eq!((sink.mappings, sink.lines), (3, 2));
        assert_eq!(data, [0, 0, 1, 0, NONE]);
    }

    #[test]
    fn segments_come_back_sorted() {
        // Second segment of line 1 goes backwards in the encoding.
        let segments = decode_segments(b"KAAA,DAAA").unwrap();
        let columns: Vec<i32> = segments.iter().map(|s| s.generated_column).collect();
        assert_eq!(columns, vec![4, 5]);
    }

    #[test]
    fn segments_use_one_based_original_lines() {
        let segments = decode_segments(b"AAAA,CAAC").unwrap();
        assert_eq!(segments[0].original_line, 1);
        assert_eq!(segments[1].original_line, 1);
        assert_eq!(segments[1].original_column, 1);
        assert_eq!(segments[1].kind, 4);
    }

    #[test]
    fn segments_reject_cut_off_fields() {
        assert_eq!(decode_segments(b"A,g,C"), Err(DecodeError::Malformed));
        assert_eq!(decode_segments(b"Ag"), Err(DecodeError::Malformed));
        assert_eq!(decode_segments(b"AAA"), Err(DecodeError::Malformed));
    }
}
