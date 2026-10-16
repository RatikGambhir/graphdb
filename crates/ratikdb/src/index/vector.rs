//! HNSW (Hierarchical Navigable Small World) approximate nearest-neighbour
//! index, after Malkov & Yashunin (2016).
//!
//! Vectors live in one flat `Vec<f32>` for cache-friendly distance scans.
//! Deletes are tombstones: the point stays navigable but is never returned.
//! When tombstones outnumber live points the graph is rebuilt.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use std::cell::RefCell;

use rustc_hash::FxHashMap;

use crate::schema::Metric;
use crate::value::ElementId;

const M: usize = 16;
const M0: usize = M * 2;
const EF_CONSTRUCTION: usize = 64;
const MAX_LEVEL: usize = 16;

/// Epoch-stamped visited set: clearing it is O(1), so one buffer per thread
/// serves every search instead of allocating a hash set each time.
#[derive(Debug, Default)]
struct Visited {
    marks: Vec<u32>,
    epoch: u32,
}

impl Visited {
    fn reset(&mut self, len: usize) {
        if self.marks.len() < len {
            self.marks.resize(len, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    /// Marks `point`; `true` if it was not visited yet.
    fn insert(&mut self, point: u32) -> bool {
        let mark = &mut self.marks[point as usize];
        let fresh = *mark != self.epoch;
        *mark = self.epoch;
        fresh
    }
}

thread_local! {
    static VISITED: RefCell<Visited> = RefCell::new(Visited::default());
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored(f32, u32);

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Hnsw {
    metric: Metric,
    dim: usize,
    data: Vec<f32>,
    ids: Vec<ElementId>,
    /// `links[point][layer]` → neighbour points.
    links: Vec<Vec<Vec<u32>>>,
    deleted: Vec<bool>,
    lookup: FxHashMap<ElementId, u32>,
    entry: Option<u32>,
    top_level: usize,
    live: usize,
    rng: u64,
}

impl Hnsw {
    pub(crate) fn new(dim: usize, metric: Metric) -> Self {
        Self {
            metric,
            dim,
            data: Vec::new(),
            ids: Vec::new(),
            links: Vec::new(),
            deleted: Vec::new(),
            lookup: FxHashMap::default(),
            entry: None,
            top_level: 0,
            live: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.live
    }

    fn vector(&self, point: u32) -> &[f32] {
        let start = point as usize * self.dim;
        &self.data[start..start + self.dim]
    }

    fn prepare(&self, v: &[f32]) -> Vec<f32> {
        match self.metric {
            Metric::Cosine => {
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    v.iter().map(|x| x / norm).collect()
                } else {
                    v.to_vec()
                }
            }
            Metric::Euclidean | Metric::Dot => v.to_vec(),
        }
    }

    /// Smaller is closer, for every metric.
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.metric {
            Metric::Cosine => 1.0 - dot(a, b),
            Metric::Dot => -dot(a, b),
            Metric::Euclidean => a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum(),
        }
    }

    fn random_level(&mut self) -> usize {
        // xorshift64*: deterministic, fast, good enough for level sampling.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let uniform = ((r >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let ml = 1.0 / (M as f64).ln();
        ((-uniform.ln() * ml) as usize).min(MAX_LEVEL)
    }

    pub(crate) fn insert(&mut self, id: ElementId, v: &[f32]) {
        if v.len() != self.dim {
            return;
        }
        self.remove(id);
        let q = self.prepare(v);
        let point = u32::try_from(self.ids.len()).expect("vector index exceeds u32::MAX points");
        let level = self.random_level();
        self.data.extend_from_slice(&q);
        self.ids.push(id);
        self.links.push(vec![Vec::new(); level + 1]);
        self.deleted.push(false);
        self.lookup.insert(id, point);
        self.live += 1;

        let Some(mut ep) = self.entry else {
            self.entry = Some(point);
            self.top_level = level;
            return;
        };
        for layer in (level + 1..=self.top_level).rev() {
            ep = self.greedy(&q, ep, layer);
        }
        let mut eps = vec![ep];
        for layer in (0..=level.min(self.top_level)).rev() {
            let found = self.search_layer(&q, &eps, EF_CONSTRUCTION, layer);
            let max = if layer == 0 { M0 } else { M };
            let neighbours = self.select(&found, M);
            self.links[point as usize][layer] = neighbours.clone();
            for n in neighbours {
                self.links[n as usize][layer].push(point);
                if self.links[n as usize][layer].len() > max {
                    self.shrink(n, layer, max);
                }
            }
            eps = found.iter().map(|s| s.1).collect();
        }
        if level > self.top_level {
            self.top_level = level;
            self.entry = Some(point);
        }
    }

    pub(crate) fn remove(&mut self, id: ElementId) {
        let Some(point) = self.lookup.remove(&id) else {
            return;
        };
        self.deleted[point as usize] = true;
        self.live -= 1;
        if self.live == 0 {
            *self = Self::new(self.dim, self.metric);
        } else if self.ids.len() > 1024 && self.ids.len() - self.live > self.live {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let mut fresh = Self::new(self.dim, self.metric);
        for (point, id) in self.ids.iter().enumerate() {
            if !self.deleted[point] {
                fresh.insert(*id, self.vector(point as u32));
            }
        }
        *self = fresh;
    }

    fn greedy(&self, q: &[f32], mut ep: u32, layer: usize) -> u32 {
        let mut best = self.distance(q, self.vector(ep));
        loop {
            let mut improved = false;
            for &n in self.links[ep as usize]
                .get(layer)
                .map_or(&[][..], Vec::as_slice)
            {
                let d = self.distance(q, self.vector(n));
                if d < best {
                    best = d;
                    ep = n;
                    improved = true;
                }
            }
            if !improved {
                return ep;
            }
        }
    }

    /// Returns up to `ef` closest points on `layer`, sorted closest first.
    fn search_layer(&self, q: &[f32], eps: &[u32], ef: usize, layer: usize) -> Vec<Scored> {
        VISITED.with_borrow_mut(|visited| self.search_layer_with(q, eps, ef, layer, visited))
    }

    fn search_layer_with(
        &self,
        q: &[f32],
        eps: &[u32],
        ef: usize,
        layer: usize,
        visited: &mut Visited,
    ) -> Vec<Scored> {
        visited.reset(self.ids.len());
        for &ep in eps {
            visited.insert(ep);
        }
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        let mut results: BinaryHeap<Scored> = BinaryHeap::new();
        for &ep in eps {
            let s = Scored(self.distance(q, self.vector(ep)), ep);
            candidates.push(Reverse(s));
            results.push(s);
        }
        while let Some(Reverse(current)) = candidates.pop() {
            let worst = results.peek().map_or(f32::INFINITY, |s| s.0);
            if current.0 > worst && results.len() >= ef {
                break;
            }
            for &n in self.links[current.1 as usize]
                .get(layer)
                .map_or(&[][..], Vec::as_slice)
            {
                if !visited.insert(n) {
                    continue;
                }
                let d = self.distance(q, self.vector(n));
                let worst = results.peek().map_or(f32::INFINITY, |s| s.0);
                if results.len() < ef || d < worst {
                    candidates.push(Reverse(Scored(d, n)));
                    results.push(Scored(d, n));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        results.into_sorted_vec()
    }

    /// Neighbour selection heuristic (paper, algorithm 4): prefer candidates
    /// that are closer to the new point than to any already-selected
    /// neighbour, which keeps the graph navigable across clusters.
    fn select(&self, sorted: &[Scored], m: usize) -> Vec<u32> {
        let mut chosen: Vec<u32> = Vec::with_capacity(m);
        for s in sorted {
            if chosen.len() >= m {
                break;
            }
            let v = self.vector(s.1);
            if chosen
                .iter()
                .all(|&c| self.distance(v, self.vector(c)) > s.0)
            {
                chosen.push(s.1);
            }
        }
        // Top up with the closest leftovers so sparse regions stay connected.
        for s in sorted {
            if chosen.len() >= m {
                break;
            }
            if !chosen.contains(&s.1) {
                chosen.push(s.1);
            }
        }
        chosen
    }

    fn shrink(&mut self, point: u32, layer: usize, max: usize) {
        let base = self.vector(point).to_vec();
        let mut scored: Vec<Scored> = self.links[point as usize][layer]
            .iter()
            .map(|&n| Scored(self.distance(&base, self.vector(n)), n))
            .collect();
        // Plain closest-`max` pruning: the diversity heuristic is applied when
        // a point is inserted; re-running it on every overflow costs O(max²)
        // distance computations per neighbour and dominated build time.
        if scored.len() > max {
            scored.select_nth_unstable(max);
            scored.truncate(max);
        }
        self.links[point as usize][layer] = scored.into_iter().map(|s| s.1).collect();
    }

    /// The `k` nearest live elements to `query`, closest first.
    pub(crate) fn search(&self, query: &[f32], k: usize, ef: usize) -> Vec<ElementId> {
        let Some(mut ep) = self.entry else {
            return Vec::new();
        };
        if query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let q = self.prepare(query);
        for layer in (1..=self.top_level).rev() {
            ep = self.greedy(&q, ep, layer);
        }
        // Over-fetch to make up for tombstones.
        let tombstones = self.ids.len() - self.live;
        let ef = ef.max(k) + tombstones.min(ef.max(k));
        self.search_layer(&q, &[ep], ef, 0)
            .into_iter()
            .filter(|s| !self.deleted[s.1 as usize])
            .take(k)
            .map(|s| self.ids[s.1 as usize])
            .collect()
    }
}

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_force(points: &[(u64, Vec<f32>)], q: &[f32], k: usize) -> Vec<u64> {
        let idx = Hnsw::new(q.len(), Metric::Euclidean);
        let mut scored: Vec<(f32, u64)> = points
            .iter()
            .map(|(id, v)| (idx.distance(q, v), *id))
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        scored.into_iter().take(k).map(|s| s.1).collect()
    }

    #[test]
    fn recall_is_high_on_random_data() {
        let mut seed = 42u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((seed >> 33) as f32) / (1u64 << 31) as f32
        };
        let points: Vec<(u64, Vec<f32>)> = (0..2000)
            .map(|i| (i, (0..16).map(|_| next()).collect()))
            .collect();
        let mut idx = Hnsw::new(16, Metric::Euclidean);
        for (id, v) in &points {
            idx.insert(*id, v);
        }
        let mut hits = 0;
        for _ in 0..50 {
            let q: Vec<f32> = (0..16).map(|_| next()).collect();
            let truth = brute_force(&points, &q, 10);
            let got = idx.search(&q, 10, 64);
            hits += got.iter().filter(|id| truth.contains(id)).count();
        }
        assert!(hits as f64 / 500.0 > 0.9, "recall too low: {hits}/500");
    }

    #[test]
    fn deleted_points_are_never_returned() {
        let mut idx = Hnsw::new(2, Metric::Cosine);
        idx.insert(1, &[1.0, 0.0]);
        idx.insert(2, &[0.9, 0.1]);
        idx.insert(3, &[0.0, 1.0]);
        idx.remove(1);
        assert_eq!(idx.search(&[1.0, 0.0], 1, 16), vec![2]);
        assert_eq!(idx.len(), 2);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore = "benchmark: cargo test --release -p ratikdb hnsw_build -- --ignored --nocapture"]
    fn hnsw_build() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((seed >> 33) as f32) / (1u64 << 31) as f32
        };
        let n: usize = std::env::var("HNSW_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50_000);
        let points: Vec<Vec<f32>> = (0..n).map(|_| (0..16).map(|_| next()).collect()).collect();
        let started = std::time::Instant::now();
        let mut idx = Hnsw::new(16, Metric::Cosine);
        for (i, p) in points.iter().enumerate() {
            idx.insert(i as u64, p);
        }
        eprintln!("built {n} x 16d in {:?}", started.elapsed());
        let queries: Vec<Vec<f32>> = (0..100)
            .map(|_| (0..16).map(|_| next()).collect())
            .collect();
        let started = std::time::Instant::now();
        let mut hits = 0;
        for q in &queries {
            let got = idx.search(q, 10, 64);
            let mut truth: Vec<(f32, usize)> = points
                .iter()
                .enumerate()
                .map(|(i, p)| (idx.distance(&idx.prepare(q), &idx.prepare(p)), i))
                .collect();
            truth.sort_by(|a, b| a.0.total_cmp(&b.0));
            hits += got
                .iter()
                .filter(|id| truth[..10].iter().any(|t| t.1 as u64 == **id))
                .count();
        }
        eprintln!(
            "recall@10 = {:.3} (100 queries incl. brute force in {:?})",
            hits as f64 / 1000.0,
            started.elapsed()
        );
    }
}
