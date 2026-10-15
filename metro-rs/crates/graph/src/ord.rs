//! A map that iterates in insertion order with the semantics of a JavaScript
//! `Map`: setting an existing key keeps its position, deleting a key and
//! setting it again moves it to the end, and iteration is live (entries
//! deleted before the cursor reaches them are skipped, entries added are
//! visited).
//!
//! Each entry carries the sequence number of its insertion, so the caller can
//! tell which entries were (re)inserted after a given point.

use std::collections::HashMap;
use std::hash::Hash;

pub struct OrdMap<K: Copy + Eq + Hash, V> {
    entries: Vec<Option<(K, V, u64)>>,
    index: HashMap<K, usize>,
    next_seq: u64,
}

impl<K: Copy + Eq + Hash, V> Default for OrdMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash, V> OrdMap<K, V> {
    pub fn new() -> Self {
        OrdMap {
            entries: Vec::new(),
            index: HashMap::new(),
            next_seq: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        let i = *self.index.get(key)?;
        self.entries[i].as_ref().map(|e| &e.1)
    }

    /// The sequence number of the insertion of `key`'s current entry.
    pub fn seq(&self, key: &K) -> Option<u64> {
        let i = *self.index.get(key)?;
        self.entries[i].as_ref().map(|e| e.2)
    }

    /// The sequence number the next new entry gets.
    pub fn mark(&self) -> u64 {
        self.next_seq
    }

    /// Like `Map.prototype.set`. Returns true if the key was new.
    pub fn insert(&mut self, key: K, value: V) -> bool {
        if let Some(&i) = self.index.get(&key) {
            if let Some(e) = self.entries[i].as_mut() {
                e.1 = value;
            }
            return false;
        }
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((key, value, self.next_seq)));
        self.next_seq += 1;
        true
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let i = self.index.remove(key)?;
        self.entries[i].take().map(|e| e.1)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// Live iteration: the next entry at or after `*pos`, advancing `*pos`.
    pub fn next_live(&self, pos: &mut usize) -> Option<(K, &V)> {
        while *pos < self.entries.len() {
            let i = *pos;
            *pos += 1;
            if let Some(e) = &self.entries[i] {
                return Some((e.0, &e.1));
            }
        }
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().map(|e| (e.0, &e.1)))
    }

    pub fn for_each_mut(&mut self, mut f: impl FnMut(K, &mut V)) {
        for e in self.entries.iter_mut().flatten() {
            f(e.0, &mut e.1);
        }
    }

    /// Drops the tombstones. Must not be called while a cursor is in use.
    pub fn compact(&mut self) {
        if self.entries.len() <= 2 * self.index.len() + 64 {
            return;
        }
        self.entries.retain(|e| e.is_some());
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(e) = e {
                self.index.insert(e.0, i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_position_on_reinsert_and_moves_after_delete() {
        let mut m = OrdMap::new();
        m.insert(1u32, 'a');
        m.insert(2, 'b');
        m.insert(3, 'c');
        m.insert(1, 'z');
        assert_eq!(m.iter().map(|(k, _)| k).collect::<Vec<_>>(), [1, 2, 3]);
        m.remove(&1);
        m.insert(1, 'y');
        assert_eq!(m.iter().map(|(k, _)| k).collect::<Vec<_>>(), [2, 3, 1]);
    }

    #[test]
    fn iteration_is_live() {
        let mut m = OrdMap::new();
        for k in 0u32..4 {
            m.insert(k, ());
        }
        let mut pos = 0;
        let mut seen = vec![];
        while let Some((k, _)) = m.next_live(&mut pos) {
            seen.push(k);
            if k == 0 {
                m.remove(&2);
                m.insert(9, ());
            }
        }
        assert_eq!(seen, [0, 1, 3, 9]);
    }

    #[test]
    fn compaction_keeps_order_and_seq() {
        let mut m = OrdMap::new();
        for k in 0u32..200 {
            m.insert(k, k);
        }
        for k in 0u32..190 {
            m.remove(&k);
        }
        let seq = m.seq(&195);
        m.compact();
        assert_eq!(m.seq(&195), seq);
        assert_eq!(m.iter().count(), 10);
        assert_eq!(m.iter().next().unwrap().0, 190);
    }
}
