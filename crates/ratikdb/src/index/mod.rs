//! Secondary indexes: B-tree property indexes, BM25 text and HNSW vectors.

pub(crate) mod text;
pub(crate) mod vector;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use crate::value::{ElementId, OrdValue, Value};

/// Equality and range index over one property of one type. `NULL`s are not
/// indexed.
#[derive(Debug, Default)]
pub(crate) struct PropIndex {
    map: BTreeMap<OrdValue, BTreeSet<ElementId>>,
}

impl PropIndex {
    pub(crate) fn insert(&mut self, value: &Value, id: ElementId) {
        if !value.is_null() {
            self.map
                .entry(OrdValue(value.clone()))
                .or_default()
                .insert(id);
        }
    }

    pub(crate) fn remove(&mut self, value: &Value, id: ElementId) {
        let key = OrdValue(value.clone());
        if let Some(ids) = self.map.get_mut(&key) {
            ids.remove(&id);
            if ids.is_empty() {
                self.map.remove(&key);
            }
        }
    }

    /// An element other than `except` holding `value`, if any.
    pub(crate) fn conflict(&self, value: &Value, except: ElementId) -> Option<ElementId> {
        if value.is_null() {
            return None;
        }
        self.map
            .get(&OrdValue(value.clone()))
            .and_then(|ids| ids.iter().copied().find(|id| *id != except))
    }

    pub(crate) fn eq(&self, value: &Value) -> impl Iterator<Item = ElementId> + '_ {
        self.map
            .get(&OrdValue(value.clone()))
            .into_iter()
            .flatten()
            .copied()
    }

    pub(crate) fn range(&self, lo: Bound<&Value>, hi: Bound<&Value>) -> Vec<ElementId> {
        let wrap = |b: Bound<&Value>| match b {
            Bound::Included(v) => Bound::Included(OrdValue(v.clone())),
            Bound::Excluded(v) => Bound::Excluded(OrdValue(v.clone())),
            Bound::Unbounded => Bound::Unbounded,
        };
        let (lo, hi) = (wrap(lo), wrap(hi));
        if let (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) =
            (&lo, &hi)
            && a > b
        {
            return Vec::new();
        }
        let mut out: Vec<ElementId> = self
            .map
            .range((lo, hi))
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();
        out.sort_unstable();
        out
    }

    /// Number of distinct indexed values with more than one element (used
    /// when turning a plain index into a UNIQUE one).
    pub(crate) fn first_duplicate(&self) -> Option<&Value> {
        self.map
            .iter()
            .find(|(_, ids)| ids.len() > 1)
            .map(|(v, _)| &v.0)
    }
}
