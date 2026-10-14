//! An insertion-ordered string-keyed map with JS `Map` semantics, used for
//! `FileData` and for the change aggregator's `Map`s and `Set`s: setting an
//! existing key keeps its position; deleting a key and setting it again moves
//! it to the end.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct JsMap<V> {
    entries: Vec<Option<(String, V)>>,
    index: HashMap<String, usize>,
}

impl<V> Default for JsMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<V> JsMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            entries: Vec::with_capacity(n),
            index: HashMap::with_capacity(n),
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.index
            .get(key)
            .and_then(|i| self.entries[*i].as_ref())
            .map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let i = *self.index.get(key)?;
        self.entries[i].as_mut().map(|(_, v)| v)
    }

    /// `map.set(key, value)`: returns the replaced value.
    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        if let Some(i) = self.index.get(&key) {
            let slot = self.entries[*i].as_mut().expect("indexed entry is live");
            return Some(std::mem::replace(&mut slot.1, value));
        }
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push(Some((key, value)));
        None
    }

    /// `map.delete(key)`: returns the removed value.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let i = self.index.remove(key)?;
        let removed = self.entries[i].take().map(|(_, v)| v);
        // Compact when more than half of the slots are tombstones.
        if self.entries.len() > 32 && self.index.len() * 2 < self.entries.len() {
            self.compact();
        }
        removed
    }

    fn compact(&mut self) {
        let old = std::mem::take(&mut self.entries);
        self.entries = old.into_iter().flatten().map(Some).collect();
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some((k, _)) = entry {
                *self.index.get_mut(k.as_str()).expect("live key is indexed") = i;
            }
        }
    }

    /// `map.entries()`, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().flatten().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut V)> {
        self.entries
            .iter_mut()
            .flatten()
            .map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.iter().map(|(k, _)| k)
    }

    pub fn into_vec(self) -> Vec<(String, V)> {
        self.entries.into_iter().flatten().collect()
    }
}

impl<V> FromIterator<(String, V)> for JsMap<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut map = JsMap::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// A JS `Set<string>`.
#[derive(Clone, Debug, Default)]
pub struct JsSet(JsMap<()>);

impl JsSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn add(&mut self, key: &str) {
        if !self.0.contains_key(key) {
            self.0.insert(key.to_string(), ());
        }
    }

    pub fn delete(&mut self, key: &str) -> bool {
        self.0.remove(key).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.keys()
    }

    pub fn into_vec(self) -> Vec<String> {
        self.0.into_vec().into_iter().map(|(k, _)| k).collect()
    }
}
