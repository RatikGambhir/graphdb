//! BM25 full-text index.

const K1: f64 = 1.2;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "to", "was", "with",
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

/// Okapi BM25 contribution of one term to one document.
///
/// `tf`: term frequency in the document, `df`: documents containing the term,
/// `docs`: documents indexed, `total_len`: their summed length, `len`: this
/// document's length (in tokens).
pub(crate) fn bm25(tf: u32, df: u64, docs: u64, total_len: u64, len: u32) -> f64 {
    let n = docs as f64;
    let avg = if docs > 0 { total_len as f64 / n } else { 1.0 }.max(1.0);
    let (tf, df) = (f64::from(tf), df as f64);
    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * f64::from(len) / avg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_and_scores() {
        assert_eq!(
            tokenize("The Graph-DB, of the year!"),
            ["graph", "db", "year"]
        );
        // More occurrences and rarer terms score higher.
        assert!(bm25(3, 1, 10, 100, 10) > bm25(1, 1, 10, 100, 10));
        assert!(bm25(1, 1, 10, 100, 10) > bm25(1, 9, 10, 100, 10));
    }
}
