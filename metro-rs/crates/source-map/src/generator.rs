//! Incremental encoder for the `mappings` field. Port of `Generator.js` and
//! `B64Builder.js`; the JavaScript `Generator` class keeps the source list, the
//! source contents and the metadata, and this keeps everything that depends on
//! the encoded positions.

use crate::vlq;
use std::collections::HashMap;

/// Why a mapping could not be added.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddError {
    /// `Mapping is for a position preceding an earlier mapping`
    PrecedingMapping,
    /// `Cannot add mapping before starting a file with addFile()`
    NoFile,
}

impl AddError {
    /// The message of the error thrown by the JavaScript implementation.
    pub fn message(self) -> &'static str {
        match self {
            AddError::PrecedingMapping => "Mapping is for a position preceding an earlier mapping",
            AddError::NoFile => "Cannot add mapping before starting a file with `addFile()`",
        }
    }
}

/// The state a JavaScript `Generator` needs to continue where Rust stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LastPositions {
    pub generated_column: i32,
    pub generated_line: i32,
    pub name: i32,
    pub source: i32,
    pub source_column: i32,
    pub source_line: i32,
}

/// Names in order of first use, as the JavaScript `IndexedSet`.
#[derive(Default)]
pub struct NameSet {
    names: Vec<String>,
    index: HashMap<String, i32>,
}

impl NameSet {
    pub fn index_for(&mut self, name: &str) -> i32 {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.names.len() as i32;
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), i);
        i
    }

    pub fn into_items(self) -> Vec<String> {
        self.names
    }

    pub fn items(&self) -> &[String] {
        &self.names
    }
}

pub struct Generator {
    buffer: Vec<u8>,
    has_segment: bool,
    last: LastPositions,
    /// Index of the file being added, or -1 outside of a file.
    source: i32,
    next_source: i32,
    names: NameSet,
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Generator {
    pub fn new() -> Self {
        Generator {
            buffer: Vec::new(),
            has_segment: false,
            last: LastPositions {
                generated_column: 0,
                generated_line: 1,
                name: 0,
                source: 0,
                source_column: 0,
                source_line: 1,
            },
            source: -1,
            next_source: 0,
            names: NameSet::default(),
        }
    }

    /// Marks the beginning of a new source file and returns its index.
    pub fn start_file(&mut self) -> i32 {
        self.source = self.next_source;
        self.next_source += 1;
        self.source
    }

    /// Marks the end of the current source file.
    pub fn end_file(&mut self) {
        self.source = -1;
    }

    pub fn index_for_name(&mut self, name: &str) -> i32 {
        self.names.index_for(name)
    }

    pub fn add_simple_mapping(&mut self, line: i32, column: i32) -> Result<(), AddError> {
        let last = &mut self.last;
        if self.source == -1 {
            return Err(AddError::NoFile);
        }
        if (line == last.generated_line && column < last.generated_column)
            || line < last.generated_line
        {
            return Err(AddError::PrecedingMapping);
        }
        if line > last.generated_line {
            // markLines
            self.has_segment = false;
            let count = (line - last.generated_line) as usize;
            self.buffer
                .resize(self.buffer.len() + count, vlq::LINE_SEPARATOR);
            last.generated_line = line;
            last.generated_column = 0;
        }
        // startSegment
        if self.has_segment {
            self.buffer.push(vlq::SEGMENT_SEPARATOR);
        } else {
            self.has_segment = true;
        }
        vlq::encode(column.wrapping_sub(last.generated_column), &mut self.buffer);
        last.generated_column = column;
        Ok(())
    }

    pub fn add_source_mapping(
        &mut self,
        line: i32,
        column: i32,
        source_line: i32,
        source_column: i32,
    ) -> Result<(), AddError> {
        self.add_simple_mapping(line, column)?;
        let last = &mut self.last;
        vlq::encode(self.source.wrapping_sub(last.source), &mut self.buffer);
        vlq::encode(source_line.wrapping_sub(last.source_line), &mut self.buffer);
        vlq::encode(
            source_column.wrapping_sub(last.source_column),
            &mut self.buffer,
        );
        last.source = self.source;
        last.source_column = source_column;
        last.source_line = source_line;
        Ok(())
    }

    /// `name_index` comes from [`Generator::index_for_name`].
    pub fn add_named_source_mapping(
        &mut self,
        line: i32,
        column: i32,
        source_line: i32,
        source_column: i32,
        name_index: i32,
    ) -> Result<(), AddError> {
        self.add_source_mapping(line, column, source_line, source_column)?;
        vlq::encode(name_index.wrapping_sub(self.last.name), &mut self.buffer);
        self.last.name = name_index;
        Ok(())
    }

    pub fn last(&self) -> LastPositions {
        self.last
    }

    pub fn has_segment(&self) -> bool {
        self.has_segment
    }

    pub fn mappings(&self) -> &[u8] {
        &self.buffer
    }

    pub fn names(&self) -> &[String] {
        self.names.items()
    }

    pub fn into_parts(self) -> (Vec<u8>, Vec<String>) {
        (self.buffer, self.names.into_items())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(g: &Generator) -> String {
        String::from_utf8(g.mappings().to_vec()).unwrap()
    }

    #[test]
    fn encodes_simple_and_source_mappings() {
        let mut g = Generator::new();
        g.start_file();
        g.add_simple_mapping(1, 0).unwrap();
        g.add_source_mapping(1, 4, 3, 2).unwrap();
        g.add_source_mapping(3, 1, 4, 0).unwrap();
        g.end_file();
        // "A" ; "IAEE" ; ; "CAC" with the source/line/column deltas.
        assert_eq!(text(&g), "A,IAEE;;CACF");
    }

    #[test]
    fn names_are_numbered_by_first_use() {
        let mut g = Generator::new();
        g.start_file();
        let b = g.index_for_name("b");
        let a = g.index_for_name("a");
        assert_eq!((b, a, g.index_for_name("b")), (0, 1, 0));
        g.add_named_source_mapping(1, 0, 1, 0, b).unwrap();
        g.add_named_source_mapping(1, 2, 1, 2, a).unwrap();
        assert_eq!(g.names(), ["b", "a"]);
        assert_eq!(text(&g), "AAAAA,EAAEC");
    }

    #[test]
    fn second_file_continues_the_deltas() {
        let mut g = Generator::new();
        g.start_file();
        g.add_source_mapping(1, 0, 1, 0).unwrap();
        g.end_file();
        g.start_file();
        g.add_source_mapping(2, 0, 1, 0).unwrap();
        assert_eq!(text(&g), "AAAA;ACAA");
        assert_eq!(g.last().source, 1);
    }

    #[test]
    fn rejects_positions_before_the_previous_mapping() {
        let mut g = Generator::new();
        g.start_file();
        g.add_simple_mapping(2, 5).unwrap();
        assert_eq!(g.add_simple_mapping(2, 4), Err(AddError::PrecedingMapping));
        assert_eq!(g.add_simple_mapping(1, 9), Err(AddError::PrecedingMapping));
        assert!(g.add_simple_mapping(2, 5).is_ok());
    }

    #[test]
    fn rejects_mappings_outside_a_file() {
        let mut g = Generator::new();
        assert_eq!(g.add_simple_mapping(1, 0), Err(AddError::NoFile));
        g.start_file();
        g.end_file();
        assert_eq!(g.add_simple_mapping(1, 0), Err(AddError::NoFile));
        assert_eq!(
            AddError::NoFile.message(),
            "Cannot add mapping before starting a file with `addFile()`"
        );
    }
}
