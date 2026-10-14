//! Lookups in decoded mappings. Port of `MappingsConsumer.originalPositionFor`.

use crate::decode::{GENERATED_COLUMN, NAME, NONE, SOURCE, STRIDE};
use metro_rs_ob1::{get0, get1, Number0, Number1};

/// A view of [`crate::decode::Columns`].
#[derive(Clone, Copy)]
pub struct ColumnsRef<'a> {
    pub data: &'a [i32],
    pub line_starts: &'a [i32],
}

impl<'a> ColumnsRef<'a> {
    pub fn len(&self) -> usize {
        self.data.len() / STRIDE
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn field(&self, mapping: usize, field: usize) -> i32 {
        self.data[mapping * STRIDE + field]
    }

    /// Whether the mapping has a source position.
    pub fn has_source(&self, mapping: usize) -> bool {
        self.field(mapping, SOURCE) != NONE
    }

    /// Whether the mapping has a name.
    pub fn has_name(&self, mapping: usize) -> bool {
        self.field(mapping, NAME) != NONE
    }

    /// The range of mappings on generated line `line` (0-based).
    pub fn line_range(&self, line: usize) -> std::ops::Range<usize> {
        let start = self.line_starts[line] as usize;
        let end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len(), |&e| e as usize);
        start..end
    }

    /// Index of the last mapping on generated `line` (1-based) that starts at
    /// or before `column`, or `None` when there is none.
    pub fn original_position_for(&self, line: Number1, column: Number0) -> Option<usize> {
        let line0 = get1(line) - 1;
        if line0 < 0 || line0 as usize >= self.line_starts.len() {
            return None;
        }
        let line0 = line0 as usize;
        let mut low = self.line_starts[line0] as usize;
        let mut high = if line0 + 1 < self.line_starts.len() {
            self.line_starts[line0 + 1] as usize
        } else {
            self.len()
        };
        let line_start = low;
        let column = get0(column);
        while low < high {
            let mid = (low + high) >> 1;
            if self.field(mid, GENERATED_COLUMN) <= column {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low > line_start {
            Some(low - 1)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_columns;
    use metro_rs_ob1::{add0, add1};

    #[test]
    fn finds_the_last_mapping_at_or_before_the_column() {
        let columns = decode_columns(b"AAAA,IAAI;AACA").unwrap();
        let view = ColumnsRef {
            data: &columns.data,
            line_starts: &columns.line_starts,
        };
        assert_eq!(view.original_position_for(add1(0), add0(0)), Some(0));
        assert_eq!(view.original_position_for(add1(0), add0(3)), Some(0));
        assert_eq!(view.original_position_for(add1(0), add0(4)), Some(1));
        assert_eq!(view.original_position_for(add1(0), add0(99)), Some(1));
        assert_eq!(view.original_position_for(add1(1), add0(0)), Some(2));
    }

    #[test]
    fn misses_before_the_first_mapping_and_outside_the_lines() {
        let columns = decode_columns(b"IAAA;;AAAA").unwrap();
        let view = ColumnsRef {
            data: &columns.data,
            line_starts: &columns.line_starts,
        };
        assert_eq!(view.original_position_for(add1(0), add0(3)), None);
        // The empty second line has no mappings of its own.
        assert_eq!(view.original_position_for(add1(1), add0(0)), None);
        assert_eq!(view.original_position_for(add1(5), add0(0)), None);
        assert_eq!(view.original_position_for(add1(-1), add0(0)), None);
    }
}
