//! Batch encoding of modules' raw mappings into one `mappings` string. Port of
//! `addMappingsForFile` / `addMapping` in `source-map.js`, fed with flat arrays
//! so that a whole module list crosses into Rust in one call.

use crate::decode::{decode_segments, DecodeError};
use crate::generator::{AddError, Generator};

/// Number of `i32`s per mapping in the tuple arrays.
pub const TUPLE_STRIDE: usize = 5;
/// Value of the last field of a tuple that has no source position.
pub const NO_NAME_SIMPLE: i32 = -2;
/// Value of the last field of a tuple that has a source position but no name.
pub const NO_NAME: i32 = -1;
/// Value of the last field of an entry that is not a valid tuple (its length
/// was not 2, 4 or 5). The error is only raised when the encoder reaches it,
/// so that earlier errors win, as in JavaScript.
pub const INVALID: i32 = -3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// The generator refused a mapping.
    Add(AddError),
    /// The tuple at this index (within the module) is not valid.
    InvalidMapping(usize),
    /// The VLQ map of a module cannot be decoded exactly as JavaScript does.
    /// The caller runs the JavaScript implementation.
    Fallback,
}

impl From<AddError> for EncodeError {
    fn from(error: AddError) -> Self {
        EncodeError::Add(error)
    }
}

/// Encodes modules one after the other into a single generator.
pub struct Encoder {
    generator: Generator,
    /// For every entry of the name table, its index in the generator or -1.
    name_cache: Vec<i32>,
    touched: Vec<usize>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Encoder {
            generator: Generator::new(),
            name_cache: Vec::new(),
            touched: Vec::new(),
        }
    }

    /// Prepares for tuples that refer to `table_len` names by index.
    pub fn reset_name_table(&mut self, table_len: usize) {
        self.name_cache.clear();
        self.name_cache.resize(table_len, -1);
        self.touched.clear();
    }

    pub fn generator(&self) -> &Generator {
        &self.generator
    }

    /// Takes the finished generator and starts a new one, keeping the name table.
    pub fn take_generator(&mut self) -> Generator {
        for &i in &self.touched {
            self.name_cache[i] = -1;
        }
        self.touched.clear();
        std::mem::take(&mut self.generator)
    }

    fn name_index(&mut self, names: &[String], table_index: i32) -> i32 {
        let slot = table_index as usize;
        let cached = self.name_cache[slot];
        if cached >= 0 {
            return cached;
        }
        let index = self.generator.index_for_name(&names[slot]);
        self.name_cache[slot] = index;
        self.touched.push(slot);
        index
    }

    /// Adds a module given as tuples (`TUPLE_STRIDE` values each: line, column,
    /// source line, source column, name reference). `carry_over` is added to
    /// every generated line.
    pub fn add_tuples(
        &mut self,
        tuples: &[i32],
        names: &[String],
        carry_over: i32,
    ) -> Result<(), EncodeError> {
        self.generator.start_file();
        for (i, t) in tuples.chunks_exact(TUPLE_STRIDE).enumerate() {
            let line = t[0].wrapping_add(carry_over);
            match t[4] {
                NO_NAME_SIMPLE => self.generator.add_simple_mapping(line, t[1])?,
                NO_NAME => self.generator.add_source_mapping(line, t[1], t[2], t[3])?,
                INVALID => return Err(EncodeError::InvalidMapping(i)),
                name => {
                    let name = self.name_index(names, name);
                    self.generator
                        .add_named_source_mapping(line, t[1], t[2], t[3], name)?
                }
            }
        }
        self.generator.end_file();
        Ok(())
    }

    /// Adds a module whose map is stored as VLQ, decoding it just in time.
    pub fn add_vlq(
        &mut self,
        mappings: &str,
        names: &[String],
        carry_over: i32,
    ) -> Result<(), EncodeError> {
        let segments = decode_segments(mappings.as_bytes()).map_err(|e: DecodeError| {
            let _ = e;
            EncodeError::Fallback
        })?;
        self.generator.start_file();
        for s in &segments {
            let line = s.generated_line.wrapping_add(carry_over);
            match s.kind {
                1 => self
                    .generator
                    .add_simple_mapping(line, s.generated_column)?,
                4 => self.generator.add_source_mapping(
                    line,
                    s.generated_column,
                    s.original_line,
                    s.original_column,
                )?,
                _ => {
                    // `names.at(i)` throws in the JavaScript decoder when out of range.
                    let name = usize::try_from(s.name)
                        .ok()
                        .and_then(|i| names.get(i))
                        .ok_or(EncodeError::Fallback)?;
                    let name = self.generator.index_for_name(name);
                    self.generator.add_named_source_mapping(
                        line,
                        s.generated_column,
                        s.original_line,
                        s.original_column,
                        name,
                    )?
                }
            }
        }
        self.generator.end_file();
        Ok(())
    }
}

/// Number of `i32`s per module in a batch's `modules` array.
pub const MODULE_STRIDE: usize = 4;
/// Kind of a module whose map is tuples `start..end` of the batch.
pub const MODULE_TUPLES: i32 = 1;
/// Kind of a module whose map is the batch's VLQ map number `start`.
pub const MODULE_VLQ: i32 = 2;
/// Kind of a module with a map that is neither; JavaScript throws for it.
pub const MODULE_UNEXPECTED: i32 = 3;

/// What stopped a batch, with the module it happened in. The JavaScript side
/// turns it into the exception the JavaScript implementation throws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchError {
    /// `generator.add*Mapping` refused a mapping.
    Add { module: usize, error: AddError },
    /// An entry of the module is not a valid tuple.
    InvalidMapping { module: usize, index: usize },
    /// The module has an unexpected kind of map.
    UnexpectedModule { module: usize },
    /// Run the JavaScript implementation instead.
    Fallback { module: usize },
}

/// A list of modules, flattened for one crossing.
#[derive(Clone, Copy)]
pub struct Batch<'a> {
    /// Tuples of all modules, `TUPLE_STRIDE` values each.
    pub tuples: &'a [i32],
    /// Names that tuples refer to.
    pub names: &'a [String],
    /// `MODULE_STRIDE` values per module: kind, start, end, carry over.
    pub modules: &'a [i32],
    pub vlq_mappings: &'a [String],
    pub vlq_names: &'a [Vec<String>],
}

/// The `mappings` and `names` of one generator.
pub type Encoded = (Vec<u8>, Vec<String>);

impl Encoder {
    fn add_module(
        &mut self,
        batch: &Batch,
        module: usize,
        shift_lines: bool,
    ) -> Result<(), BatchError> {
        let m = &batch.modules[module * MODULE_STRIDE..][..MODULE_STRIDE];
        let (kind, start, end) = (m[0], m[1] as usize, m[2] as usize);
        let carry_over = if shift_lines { m[3] } else { 0 };
        let result = match kind {
            MODULE_TUPLES => self.add_tuples(
                &batch.tuples[start * TUPLE_STRIDE..end * TUPLE_STRIDE],
                batch.names,
                carry_over,
            ),
            MODULE_VLQ => self.add_vlq(
                &batch.vlq_mappings[start],
                &batch.vlq_names[start],
                carry_over,
            ),
            _ => return Err(BatchError::UnexpectedModule { module }),
        };
        result.map_err(|error| match error {
            EncodeError::Add(error) => BatchError::Add { module, error },
            EncodeError::InvalidMapping(index) => BatchError::InvalidMapping { module, index },
            EncodeError::Fallback => BatchError::Fallback { module },
        })
    }

    /// Adds every module of `batch`, in order, to the generator.
    pub fn add_batch(&mut self, batch: &Batch) -> Result<(), BatchError> {
        self.reset_name_table(batch.names.len());
        for module in 0..batch.modules.len() / MODULE_STRIDE {
            self.add_module(batch, module, true)?;
        }
        Ok(())
    }

    /// Like [`Encoder::add_batch`], but every module gets a generator of its
    /// own (and no line shift): the `mappings` and `names` of each, in order.
    pub fn encode_each(&mut self, batch: &Batch) -> Result<Vec<Encoded>, BatchError> {
        self.reset_name_table(batch.names.len());
        let count = batch.modules.len() / MODULE_STRIDE;
        let mut out = Vec::with_capacity(count);
        for module in 0..count {
            self.add_module(batch, module, false)?;
            out.push(self.take_generator().into_parts());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuple(line: i32, col: i32, sl: i32, sc: i32, name: i32) -> [i32; 5] {
        [line, col, sl, sc, name]
    }

    fn text(e: &Encoder) -> String {
        String::from_utf8(e.generator().mappings().to_vec()).unwrap()
    }

    #[test]
    fn encodes_tuples_with_names_and_carry_over() {
        let mut e = Encoder::new();
        let names = vec!["a".to_string(), "b".to_string()];
        e.reset_name_table(names.len());
        let tuples: Vec<i32> = [
            tuple(1, 0, 1, 0, NO_NAME),
            tuple(1, 3, 1, 3, 1),
            tuple(2, 0, 2, 0, NO_NAME_SIMPLE),
        ]
        .concat();
        e.add_tuples(&tuples, &names, 2).unwrap();
        assert_eq!(e.generator().names(), ["b"]);
        assert_eq!(text(&e), ";;AAAA,GAAGA;A");
    }

    #[test]
    fn invalid_tuples_are_reported_in_order() {
        let mut e = Encoder::new();
        let tuples: Vec<i32> = [
            tuple(1, 0, 1, 0, NO_NAME),
            tuple(0, 0, 1, 0, INVALID),
            tuple(1, 0, 1, 0, NO_NAME),
        ]
        .concat();
        assert_eq!(
            e.add_tuples(&tuples, &[], 0),
            Err(EncodeError::InvalidMapping(1))
        );
        // An out of order mapping before the invalid one wins.
        let mut e = Encoder::new();
        let tuples: Vec<i32> = [
            tuple(2, 0, 1, 0, NO_NAME),
            tuple(1, 0, 1, 0, NO_NAME),
            tuple(0, 0, 1, 0, INVALID),
        ]
        .concat();
        assert_eq!(
            e.add_tuples(&tuples, &[], 0),
            Err(EncodeError::Add(AddError::PrecedingMapping))
        );
    }

    #[test]
    fn vlq_modules_are_decoded_and_reencoded() {
        let mut e = Encoder::new();
        e.add_vlq("AAAA,CAAC;AACD", &[], 3).unwrap();
        // Re-encoded two lines lower, with the second module's file index.
        assert_eq!(text(&e), ";;;AAAA,CAAC;AACD");
    }

    #[test]
    fn vlq_modules_sort_their_mappings() {
        let mut e = Encoder::new();
        e.add_vlq("KAAA,DAAA", &[], 0).unwrap();
        assert_eq!(text(&e), "IAAA,CAAA");
    }

    #[test]
    fn vlq_modules_that_javascript_would_reject_fall_back() {
        let mut e = Encoder::new();
        assert_eq!(e.add_vlq("AA", &[], 0), Err(EncodeError::Fallback));
        assert_eq!(
            e.add_vlq("AAAAA", &["x".to_string()][..0], 0),
            Err(EncodeError::Fallback)
        );
    }

    #[test]
    fn take_generator_resets_the_name_cache() {
        let mut e = Encoder::new();
        let table = vec!["a".to_string()];
        e.reset_name_table(1);
        e.add_tuples(&tuple(1, 0, 1, 0, 0), &table, 0).unwrap();
        let (_, names) = e.take_generator().into_parts();
        assert_eq!(names, ["a"]);
        e.add_tuples(&tuple(1, 0, 1, 0, 0), &table, 0).unwrap();
        assert_eq!(e.generator().names(), ["a"]);
    }
}
