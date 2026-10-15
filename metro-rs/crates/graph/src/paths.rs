//! The process-wide table of module paths (rule 4): a path gets an id once,
//! ids are never reused, and JavaScript keeps the matching `id -> string`
//! array, extended in batches.

use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
struct Table {
    ids: HashMap<Box<str>, u32>,
    paths: Vec<Box<str>>,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

/// Interns each path and returns its id. Paths seen before keep their id.
pub fn intern_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<u32> {
    TABLE.with(|t| {
        let mut t = t.borrow_mut();
        paths
            .into_iter()
            .map(|p| {
                if let Some(&id) = t.ids.get(p) {
                    return id;
                }
                let id = t.paths.len() as u32;
                t.ids.insert(p.into(), id);
                t.paths.push(p.into());
                id
            })
            .collect()
    })
}

/// The path of an id, if it was interned (for diagnostics).
pub fn path_of(id: u32) -> Option<String> {
    TABLE.with(|t| t.borrow().paths.get(id as usize).map(|p| p.to_string()))
}
