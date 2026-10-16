//! BM25 full-text index.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::value::ElementId;

const K1: f64 = 1.2;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on", "or", "that",
    "the", "to", "was", "with",
];

/// Lower-cases and splits on anything that is not alphanumeric, dropping
/// common English stopwords.
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

#[derive(Debug, Default)]
pub(crate) struct TextIndex {
    postings: FxHashMap<String, FxHashMap<ElementId, u32>>,
    doc_len: FxHashMap<ElementId, u32>,
    total_len: u64,
}

impl TextIndex {
    pub(crate) fn insert(&mut self, id: ElementId, text: &str) {
        let tokens = tokenize(text);
        let len = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
        for token in tokens {
            *self.postings.entry(token).or_default().entry(id).or_default() += 1;
        }
        self.doc_len.insert(id, len);
        self.total_len += u64::from(len);
    }

    pub(crate) fn remove(&mut self, id: ElementId, text: &str) {
        let Some(len) = self.doc_len.remove(&id) else {
            return;
        };
        self.total_len -= u64::from(len);
        for token in tokenize(text) {
            if let Some(docs) = self.postings.get_mut(&token) {
                docs.remove(&id);
                if docs.is_empty() {
                    self.postings.remove(&token);
                }
            }
        }
    }

    /// Every document containing at least one of the terms.
    pub(crate) fn candidates(&self, terms: &[String]) -> FxHashSet<ElementId> {
        terms
            .iter()
            .filter_map(|t| self.postings.get(t))
            .flat_map(|docs| docs.keys().copied())
            .collect()
    }

    /// `true` when the document contains any of the terms.
    pub(crate) fn matches(&self, id: ElementId, terms: &[String]) -> bool {
        terms.iter().any(|t| self.postings.get(t).is_some_and(|d| d.contains_key(&id)))
    }

    /// Okapi BM25 score of a document for the query terms (0 when no term matches).
    pub(crate) fn score(&self, id: ElementId, terms: &[String]) -> f64 {
        let Some(&len) = self.doc_len.get(&id) else {
            return 0.0;
        };
        let n = self.doc_len.len() as f64;
        let avg = if n > 0.0 { self.total_len as f64 / n } else { 1.0 }.max(1.0);
        terms
            .iter()
            .filter_map(|t| {
                let docs = self.postings.get(t)?;
                let tf = f64::from(*docs.get(&id)?);
                let df = docs.len() as f64;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                Some(idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * f64::from(len) / avg)))
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_more_relevant_documents_higher() {
        let mut idx = TextIndex::default();
        idx.insert(1, "Graph databases store nodes and edges");
        idx.insert(2, "A graph is a graph is a graph");
        idx.insert(3, "Cooking pasta at home");
        let q = tokenize("graph");
        assert!(idx.score(2, &q) > idx.score(1, &q));
        assert_eq!(idx.score(3, &q), 0.0);
        assert_eq!(idx.candidates(&q).len(), 2);
        idx.remove(2, "A graph is a graph is a graph");
        assert_eq!(idx.candidates(&q).len(), 1);
    }
}
