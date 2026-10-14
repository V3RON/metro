//! Composition of source maps. Port of the mapping loop of
//! `composeSourceMaps.js`: chains each mapping of the last map through the
//! earlier maps, drops duplicates and encodes the result. The JavaScript side
//! builds the consumers, interns the source and name strings and assembles the
//! final object (contents, metadata, ignore list).

use crate::consumer::ColumnsRef;
use crate::decode::{GENERATED_COLUMN, NAME, ORIGINAL_COLUMN, ORIGINAL_LINE, SOURCE};
use crate::vlq;
use metro_rs_ob1::{add0, add1};

/// A decoded map with the tables that turn its indices into global ids.
#[derive(Clone, Copy)]
pub struct ComposeMap<'a> {
    pub columns: ColumnsRef<'a>,
    /// Global id of each of the map's sources, or -1 for a source that is not a
    /// usable string (JavaScript requires it to be non-empty).
    pub sources: &'a [i32],
    /// Global id of each of the map's names.
    pub names: &'a [i32],
}

#[derive(Debug, Eq, PartialEq)]
pub enum ComposeError {
    /// The mappings of the last map are not in generated order, so JavaScript
    /// sorts them with its own comparator.
    Unsorted,
    /// A mapping that JavaScript rejects (or looks up outside its tables).
    Invalid,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Composed {
    pub mappings: Vec<u8>,
    /// Global ids of the composed `sources`, in order of first use.
    pub sources: Vec<i32>,
    /// Global ids of the composed `names`, in order of first use.
    pub names: Vec<i32>,
}

#[derive(Clone, Copy)]
struct Resolved {
    line: i32,
    column: i32,
    /// Index into the composed sources, or -1.
    source: i32,
    original_line: i32,
    original_column: i32,
    /// Index into the composed names, or -1.
    name: i32,
}

fn lookup<T: Copy>(table: &[T], index: i32) -> Result<T, ComposeError> {
    usize::try_from(index)
        .ok()
        .and_then(|i| table.get(i))
        .copied()
        .ok_or(ComposeError::Invalid)
}

/// Interns global ids in order of first use.
struct Interner {
    slots: Vec<i32>,
    order: Vec<i32>,
}

impl Interner {
    fn new() -> Self {
        Interner {
            slots: Vec::new(),
            order: Vec::new(),
        }
    }

    fn index_of(&mut self, id: i32) -> i32 {
        let id = id as usize;
        if id >= self.slots.len() {
            self.slots.resize(id + 1, -1);
        }
        if self.slots[id] < 0 {
            self.slots[id] = self.order.len() as i32;
            self.order.push(id as i32);
        }
        self.slots[id]
    }
}

/// `maps[0]` is the map of the generated code; the others are looked up in
/// order, the way `composeSourceMaps` reverses its argument.
pub fn compose(maps: &[ComposeMap]) -> Result<Composed, ComposeError> {
    let (last, earlier) = maps.split_first().ok_or(ComposeError::Invalid)?;
    let columns = last.columns;
    let count = columns.len();
    let mut resolved: Vec<Resolved> = Vec::with_capacity(count);
    let mut sources = Interner::new();
    let mut names = Interner::new();
    let mut sorted = true;

    for line_index in 0..columns.line_starts.len() {
        let generated_line = line_index as i32 + 1;
        for m in columns.line_range(line_index) {
            let generated_column = columns.field(m, GENERATED_COLUMN);
            // (source id, original line, original column, name id)
            let mut original: Option<(i32, i32, i32, Option<i32>)> = None;
            if columns.has_source(m) {
                if earlier.is_empty() {
                    let source = lookup(last.sources, columns.field(m, SOURCE))?;
                    let name = if columns.has_name(m) {
                        Some(lookup(last.names, columns.field(m, NAME))?)
                    } else {
                        None
                    };
                    original = Some((
                        source,
                        columns.field(m, ORIGINAL_LINE),
                        columns.field(m, ORIGINAL_COLUMN),
                        name,
                    ));
                } else {
                    let mut line = columns.field(m, ORIGINAL_LINE);
                    let mut column = columns.field(m, ORIGINAL_COLUMN);
                    let mut found = None;
                    for consumer in earlier {
                        let hit = consumer
                            .columns
                            .original_position_for(add1(line - 1), add0(column));
                        let Some(hit) = hit else {
                            found = None;
                            break;
                        };
                        if !consumer.columns.has_source(hit) {
                            found = None;
                            break;
                        }
                        line = consumer.columns.field(hit, ORIGINAL_LINE);
                        column = consumer.columns.field(hit, ORIGINAL_COLUMN);
                        let source = lookup(consumer.sources, consumer.columns.field(hit, SOURCE))?;
                        let name = if consumer.columns.has_name(hit) {
                            Some(lookup(consumer.names, consumer.columns.field(hit, NAME))?)
                        } else {
                            None
                        };
                        found = Some((source, line, column, name));
                    }
                    original = found;
                }
            }
            // The validity check of composeSourceMaps.
            if generated_line <= 0 || generated_column < 0 {
                return Err(ComposeError::Invalid);
            }
            let mut entry = Resolved {
                line: generated_line,
                column: generated_column,
                source: -1,
                original_line: 0,
                original_column: 0,
                name: -1,
            };
            if let Some((source, line, column, name)) = original {
                if line <= 0 || column < 0 || source < 0 {
                    return Err(ComposeError::Invalid);
                }
                entry.source = sources.index_of(source);
                entry.original_line = line;
                entry.original_column = column;
                if let Some(name) = name {
                    entry.name = names.index_of(name);
                }
            }
            if let Some(previous) = resolved.last() {
                if entry.line < previous.line
                    || (entry.line == previous.line && entry.column < previous.column)
                {
                    sorted = false;
                }
            }
            resolved.push(entry);
        }
    }
    if !sorted {
        return Err(ComposeError::Unsorted);
    }

    // Encode the mappings, dropping any identical to the one before it.
    let mut out: Vec<u8> = Vec::with_capacity(resolved.len() * 6);
    let mut has_segment = false;
    let mut previous: Option<&Resolved> = None;
    let mut previous_line = 1i32;
    let mut previous_column = 0i32;
    let mut previous_source = 0i32;
    let mut previous_original_line = 0i32;
    let mut previous_original_column = 0i32;
    let mut previous_name = 0i32;
    for entry in &resolved {
        if entry.line != previous_line {
            let lines = entry.line - previous_line;
            if lines >= 1 {
                has_segment = false;
                out.resize(out.len() + lines as usize, vlq::LINE_SEPARATOR);
            }
            previous_line = entry.line;
            previous_column = 0;
        } else if let Some(p) = previous {
            if p.column == entry.column
                && p.source == entry.source
                && p.original_line == entry.original_line
                && p.original_column == entry.original_column
                && p.name == entry.name
            {
                previous = Some(entry);
                continue;
            }
        }
        previous = Some(entry);

        if has_segment {
            out.push(vlq::SEGMENT_SEPARATOR);
        } else {
            has_segment = true;
        }
        vlq::encode(entry.column.wrapping_sub(previous_column), &mut out);
        previous_column = entry.column;
        if entry.source != -1 {
            vlq::encode(entry.source.wrapping_sub(previous_source), &mut out);
            previous_source = entry.source;
            // Original lines are 0-based in the encoding.
            vlq::encode(
                (entry.original_line - 1).wrapping_sub(previous_original_line),
                &mut out,
            );
            previous_original_line = entry.original_line - 1;
            vlq::encode(
                entry.original_column.wrapping_sub(previous_original_column),
                &mut out,
            );
            previous_original_column = entry.original_column;
            if entry.name != -1 {
                vlq::encode(entry.name.wrapping_sub(previous_name), &mut out);
                previous_name = entry.name;
            }
        }
    }
    Ok(Composed {
        mappings: out,
        sources: sources.order,
        names: names.order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_columns;

    fn view(columns: &crate::decode::Columns) -> ColumnsRef<'_> {
        ColumnsRef {
            data: &columns.data,
            line_starts: &columns.line_starts,
        }
    }

    #[test]
    fn a_single_map_is_reencoded_without_duplicates() {
        // Two identical mappings in a row collapse into one.
        let columns = decode_columns(b"AAAA,AAAA,CAAC").unwrap();
        let map = ComposeMap {
            columns: view(&columns),
            sources: &[0],
            names: &[],
        };
        let composed = compose(&[map]).unwrap();
        assert_eq!(String::from_utf8(composed.mappings).unwrap(), "AAAA,CAAC");
        assert_eq!(composed.sources, vec![0]);
        assert!(composed.names.is_empty());
    }

    #[test]
    fn chains_through_earlier_maps() {
        // Last map: generated (1,0) -> (1,0) and (1,4) -> (2,0) of an intermediate file.
        let last = decode_columns(b"AAAA,IACA").unwrap();
        // Earlier map: intermediate (1,0) -> file 7 line 10 col 0; (2,0) -> file 7 line 20 col 5.
        let earlier = decode_columns(b"AASA;AAmBK").unwrap();
        let composed = compose(&[
            ComposeMap {
                columns: view(&last),
                sources: &[0],
                names: &[],
            },
            ComposeMap {
                columns: view(&earlier),
                sources: &[7],
                names: &[],
            },
        ])
        .unwrap();
        assert_eq!(composed.sources, vec![7]);
        // Checked against composeSourceMaps in JavaScript.
        assert_eq!(String::from_utf8(composed.mappings).unwrap(), "AASA,IAmBK");
    }

    #[test]
    fn mappings_that_miss_the_earlier_maps_lose_their_source() {
        let last = decode_columns(b"AAAA").unwrap();
        let earlier = decode_columns(b"").unwrap();
        let composed = compose(&[
            ComposeMap {
                columns: view(&last),
                sources: &[0],
                names: &[],
            },
            ComposeMap {
                columns: view(&earlier),
                sources: &[],
                names: &[],
            },
        ])
        .unwrap();
        assert_eq!(String::from_utf8(composed.mappings).unwrap(), "A");
        assert!(composed.sources.is_empty());
    }

    #[test]
    fn unsorted_input_is_left_to_javascript() {
        let columns = decode_columns(b"KAAA,DAAA").unwrap();
        let map = ComposeMap {
            columns: view(&columns),
            sources: &[0],
            names: &[],
        };
        assert_eq!(compose(&[map]), Err(ComposeError::Unsorted));
    }

    #[test]
    fn invalid_sources_are_left_to_javascript() {
        let columns = decode_columns(b"AAAA").unwrap();
        let map = ComposeMap {
            columns: view(&columns),
            sources: &[-1],
            names: &[],
        };
        assert_eq!(compose(&[map]), Err(ComposeError::Invalid));
        let map = ComposeMap {
            columns: view(&columns),
            sources: &[],
            names: &[],
        };
        assert_eq!(compose(&[map]), Err(ComposeError::Invalid));
        assert_eq!(compose(&[]), Err(ComposeError::Invalid));
    }

    #[test]
    fn names_are_interned_in_order_of_use() {
        let columns = decode_columns(b"AAAAC,CAAAD,CAAAC").unwrap();
        let map = ComposeMap {
            columns: view(&columns),
            sources: &[3],
            names: &[9, 4],
        };
        let composed = compose(&[map]).unwrap();
        // Name 1 is used first, then name 0.
        assert_eq!(composed.names, vec![4, 9]);
        assert_eq!(
            String::from_utf8(composed.mappings).unwrap(),
            "AAAAA,CAAAC,CAAAD"
        );
    }
}
