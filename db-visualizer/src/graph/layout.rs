//! Force-directed layout.
//!
//! A plain spring/repulsion model with the repulsion term approximated by a
//! Barnes–Hut quadtree, so a tick is O(n log n). The layout is warmed up once
//! and the cards are then spread apart so their captions never overlap; the
//! canvas does not animate.

use std::collections::HashMap;

use crate::results::{GraphData, node_caption};

/// A positioned node.
#[derive(Clone, Debug)]
pub struct LayoutNode {
    pub id: u64,
    pub label: String,
    pub caption: String,
    pub x: f32,
    pub y: f32,
    vx: f32,
    vy: f32,
    pub degree: usize,
    pub pinned: bool,
    pub radius: f32,
}

/// A positioned edge, with parallel-edge bookkeeping for arc offsets.
#[derive(Clone, Debug)]
pub struct LayoutEdge {
    pub id: u64,
    pub label: String,
    pub source: usize,
    pub target: usize,
    pub parallel_index: usize,
    pub parallel_count: usize,
    pub is_loop: bool,
}

/// Tunables.
#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    pub link_distance: f32,
    pub charge: f32,
    pub gravity: f32,
    pub damping: f32,
    pub theta: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            link_distance: 115.,
            charge: 5200.,
            gravity: 0.03,
            damping: 0.82,
            theta: 0.9,
        }
    }
}

const COLLISION_PADDING: f32 = 14.;
const MIN_RADIUS: f32 = 5.;
const MAX_RADIUS: f32 = 22.;

/// Card dimensions used when spreading nodes.
pub const CARD_WIDTH: f32 = 164.;
/// Card height.
pub const CARD_HEIGHT: f32 = 52.;
const CARD_GAP: f32 = 18.;

/// Node size grows with degree, flattening so hubs stay on screen.
#[must_use]
pub fn node_radius(degree: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let d = degree as f32;
    (MIN_RADIUS + d.sqrt() * 2.6).min(MAX_RADIUS)
}

/// Deterministic PRNG so the same graph always starts from the same layout.
struct Random(u32);

impl Random {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        #[allow(clippy::cast_precision_loss)]
        let v = (t ^ (t >> 14)) as f32 / 4_294_967_296.;
        v
    }
}

fn hash_id(id: u64) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in id.to_le_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

/// The simulation state.
#[derive(Clone, Debug)]
pub struct ForceLayout {
    pub nodes: Vec<LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub index: HashMap<u64, usize>,
    pub options: LayoutOptions,
    alpha: f32,
}

impl ForceLayout {
    /// Seeds positions on a phyllotaxis spiral, jittered per id.
    #[must_use]
    pub fn new(graph: &GraphData) -> Self {
        let options = LayoutOptions::default();
        let mut degrees: HashMap<u64, usize> = HashMap::new();
        for edge in &graph.edges {
            *degrees.entry(edge.source).or_default() += 1;
            *degrees.entry(edge.target).or_default() += 1;
        }
        let mut random = Random(0x5eed);
        let spacing = (options.link_distance * 0.75).max(40.);
        let mut nodes = Vec::with_capacity(graph.nodes.len());
        let mut index = HashMap::with_capacity(graph.nodes.len());
        for (i, node) in graph.nodes.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let fi = i as f32;
            let angle = fi * 2.399_963_2; // golden angle
            let radius = spacing * fi.sqrt();
            #[allow(clippy::cast_precision_loss)]
            let jitter = (hash_id(node.id) % 1000) as f32 / 1000. - 0.5;
            let degree = degrees.get(&node.id).copied().unwrap_or(0);
            index.insert(node.id, nodes.len());
            nodes.push(LayoutNode {
                id: node.id,
                label: node.label.clone(),
                caption: node_caption(node),
                x: angle.cos() * radius + jitter * 12.,
                y: angle.sin() * radius + (random.next() - 0.5) * 12.,
                vx: 0.,
                vy: 0.,
                degree,
                pinned: false,
                radius: node_radius(degree),
            });
        }

        let pair_key = |a: u64, b: u64| if a < b { (a, b) } else { (b, a) };
        let mut pair_counts: HashMap<(u64, u64), usize> = HashMap::new();
        for edge in &graph.edges {
            *pair_counts
                .entry(pair_key(edge.source, edge.target))
                .or_default() += 1;
        }
        let mut pair_seen: HashMap<(u64, u64), usize> = HashMap::new();
        let mut edges = Vec::with_capacity(graph.edges.len());
        for edge in &graph.edges {
            let (Some(&source), Some(&target)) = (index.get(&edge.source), index.get(&edge.target))
            else {
                continue;
            };
            let key = pair_key(edge.source, edge.target);
            let parallel_index = pair_seen.entry(key).or_default();
            let this_index = *parallel_index;
            *parallel_index += 1;
            edges.push(LayoutEdge {
                id: edge.id,
                label: edge.label.clone(),
                source,
                target,
                parallel_index: this_index,
                parallel_count: pair_counts.get(&key).copied().unwrap_or(1),
                is_loop: source == target,
            });
        }

        Self {
            nodes,
            edges,
            index,
            options,
            alpha: 1.,
        }
    }

    /// `true` once the simulation has cooled.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.alpha < 0.005
    }

    /// One simulation step.
    pub fn tick(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let LayoutOptions {
            link_distance,
            charge,
            gravity,
            damping,
            theta,
        } = self.options;

        let tree = QuadTree::build(&self.nodes);
        for i in 0..self.nodes.len() {
            let (x, y) = (self.nodes[i].x, self.nodes[i].y);
            let (fx, fy) = tree.repulsion(x, y, charge, theta);
            let node = &mut self.nodes[i];
            node.vx += fx - node.x * gravity;
            node.vy += fy - node.y * gravity;
        }

        for edge in &self.edges {
            if edge.is_loop {
                continue;
            }
            let (sx, sy, sd) = {
                let s = &self.nodes[edge.source];
                (s.x, s.y, s.degree)
            };
            let (tx, ty, td) = {
                let t = &self.nodes[edge.target];
                (t.x, t.y, t.degree)
            };
            let dx = tx - sx;
            let dy = ty - sy;
            let distance = (dx * dx + dy * dy).sqrt().max(0.01);
            #[allow(clippy::cast_precision_loss)]
            let stiffness = 0.12 / (sd.min(td).clamp(1, 12) as f32);
            let force = (distance - link_distance) * stiffness;
            let fx = dx / distance * force;
            let fy = dy / distance * force;
            self.nodes[edge.source].vx += fx;
            self.nodes[edge.source].vy += fy;
            self.nodes[edge.target].vx -= fx;
            self.nodes[edge.target].vy -= fy;
        }

        for node in &mut self.nodes {
            if node.pinned {
                node.vx = 0.;
                node.vy = 0.;
                continue;
            }
            node.vx *= damping;
            node.vy *= damping;
            node.x += node.vx * self.alpha;
            node.y += node.vy * self.alpha;
        }

        self.separate();
        self.alpha *= 0.985;
    }

    /// Pushes overlapping nodes apart with a spatial hash.
    fn separate(&mut self) {
        let cell = (MAX_RADIUS * 2. + COLLISION_PADDING) * 1.5;
        let mut buckets: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        #[allow(clippy::cast_possible_truncation)]
        let key = |x: f32, y: f32| ((x / cell).floor() as i32, (y / cell).floor() as i32);
        for (i, node) in self.nodes.iter().enumerate() {
            buckets.entry(key(node.x, node.y)).or_default().push(i);
        }
        for i in 0..self.nodes.len() {
            let (cx, cy) = key(self.nodes[i].x, self.nodes[i].y);
            for ox in -1..=1 {
                for oy in -1..=1 {
                    let Some(bucket) = buckets.get(&(cx + ox, cy + oy)) else {
                        continue;
                    };
                    for &j in bucket {
                        if j == i {
                            continue;
                        }
                        let dx = self.nodes[j].x - self.nodes[i].x;
                        let dy = self.nodes[j].y - self.nodes[i].y;
                        let minimum =
                            self.nodes[i].radius + self.nodes[j].radius + COLLISION_PADDING;
                        let distance = (dx * dx + dy * dy).sqrt();
                        if distance >= minimum {
                            continue;
                        }
                        let overlap = (minimum - distance) / 2.;
                        let (ux, uy) = if distance > 0.01 {
                            (dx / distance, dy / distance)
                        } else if self.nodes[i].id < self.nodes[j].id {
                            (-1., 0.)
                        } else {
                            (1., 0.)
                        };
                        if !self.nodes[i].pinned {
                            self.nodes[i].x -= ux * overlap;
                            self.nodes[i].y -= uy * overlap;
                        }
                        if !self.nodes[j].pinned {
                            self.nodes[j].x += ux * overlap;
                            self.nodes[j].y += uy * overlap;
                        }
                    }
                }
            }
        }
    }

    /// Runs enough ticks to reach a usable layout.
    pub fn warm_up(&mut self, ticks: usize) {
        for _ in 0..ticks {
            if self.settled() {
                break;
            }
            self.tick();
        }
    }

    /// Stretches the layout and resolves card overlaps, since the simulation
    /// was tuned for circular marks and the canvas draws information cards.
    pub fn spread_cards(&mut self) {
        // Cards are three times wider than tall: widen the simulation's
        // circular layout before resolving overlaps, or the resolution pushes
        // everything into a tall column.
        for node in &mut self.nodes {
            node.x *= 1.6;
            node.y *= 1.0;
        }
        let minimum_x = CARD_WIDTH + CARD_GAP;
        let minimum_y = CARD_HEIGHT + CARD_GAP;
        let passes = if self.nodes.len() > 800 { 12 } else { 160 };
        for _ in 0..passes {
            let mut moved = false;
            for i in 0..self.nodes.len() {
                for j in (i + 1)..self.nodes.len() {
                    let dx = self.nodes[j].x - self.nodes[i].x;
                    let dy = self.nodes[j].y - self.nodes[i].y;
                    let overlap_x = minimum_x - dx.abs();
                    let overlap_y = minimum_y - dy.abs();
                    if overlap_x <= 0. || overlap_y <= 0. {
                        continue;
                    }
                    moved = true;
                    // Compare overlaps relative to card size so wide cards
                    // spread sideways as readily as they stack.
                    if overlap_x / minimum_x < overlap_y / minimum_y {
                        let direction = if dx == 0. {
                            if self.nodes[i].id < self.nodes[j].id {
                                1.
                            } else {
                                -1.
                            }
                        } else {
                            dx.signum()
                        };
                        let shift = overlap_x / 2. + 0.5;
                        self.nodes[i].x -= direction * shift;
                        self.nodes[j].x += direction * shift;
                    } else {
                        let direction = if dy == 0. {
                            if self.nodes[i].id < self.nodes[j].id {
                                1.
                            } else {
                                -1.
                            }
                        } else {
                            dy.signum()
                        };
                        let shift = overlap_y / 2. + 0.5;
                        self.nodes[i].y -= direction * shift;
                        self.nodes[j].y += direction * shift;
                    }
                }
            }
            if !moved {
                break;
            }
        }
    }

    /// Bounds of the card rectangles.
    #[must_use]
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        if self.nodes.is_empty() {
            return (-1., -1., 1., 1.);
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &self.nodes {
            min_x = min_x.min(node.x - CARD_WIDTH / 2.);
            min_y = min_y.min(node.y - CARD_HEIGHT / 2.);
            max_x = max_x.max(node.x + CARD_WIDTH / 2.);
            max_y = max_y.max(node.y + CARD_HEIGHT / 2.);
        }
        (min_x, min_y, max_x, max_y)
    }

    /// The topmost card under a point in graph space.
    #[must_use]
    pub fn node_at(&self, x: f32, y: f32) -> Option<usize> {
        self.nodes.iter().rposition(|n| {
            (x - n.x).abs() <= CARD_WIDTH / 2. && (y - n.y).abs() <= CARD_HEIGHT / 2.
        })
    }
}

/// Barnes–Hut quadtree over node positions.
struct QuadTree {
    nodes: Vec<Quad>,
}

#[derive(Clone, Copy, Debug)]
struct Quad {
    cx: f32,
    cy: f32,
    half: f32,
    mass: f32,
    mx: f32,
    my: f32,
    body: Option<(f32, f32)>,
    children: [Option<usize>; 4],
}

impl QuadTree {
    fn build(nodes: &[LayoutNode]) -> Self {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for n in nodes {
            min_x = min_x.min(n.x);
            min_y = min_y.min(n.y);
            max_x = max_x.max(n.x);
            max_y = max_y.max(n.y);
        }
        let half = ((max_x - min_x).max(max_y - min_y) / 2. + 1.).max(1.);
        let root = Quad {
            cx: (min_x + max_x) / 2.,
            cy: (min_y + max_y) / 2.,
            half,
            mass: 0.,
            mx: 0.,
            my: 0.,
            body: None,
            children: [None; 4],
        };
        let mut tree = Self { nodes: vec![root] };
        for n in nodes {
            tree.insert(0, n.x, n.y, 0);
        }
        tree.finish(0);
        tree
    }

    fn quadrant(quad: &Quad, x: f32, y: f32) -> usize {
        (usize::from(x >= quad.cx)) + 2 * usize::from(y >= quad.cy)
    }

    fn child_for(&mut self, index: usize, q: usize) -> usize {
        if let Some(c) = self.nodes[index].children[q] {
            return c;
        }
        let parent = self.nodes[index];
        let half = parent.half / 2.;
        let cx = parent.cx + if q % 2 == 1 { half } else { -half };
        let cy = parent.cy + if q >= 2 { half } else { -half };
        let child = Quad {
            cx,
            cy,
            half,
            mass: 0.,
            mx: 0.,
            my: 0.,
            body: None,
            children: [None; 4],
        };
        self.nodes.push(child);
        let id = self.nodes.len() - 1;
        self.nodes[index].children[q] = Some(id);
        id
    }

    fn insert(&mut self, index: usize, x: f32, y: f32, depth: usize) {
        let quad = self.nodes[index];
        self.nodes[index].mass += 1.;
        self.nodes[index].mx += x;
        self.nodes[index].my += y;
        if quad.mass == 0. && quad.children.iter().all(Option::is_none) {
            self.nodes[index].body = Some((x, y));
            return;
        }
        if depth > 40 {
            // Coincident points: keep them aggregated in this cell.
            return;
        }
        if let Some((bx, by)) = quad.body {
            self.nodes[index].body = None;
            let q = Self::quadrant(&quad, bx, by);
            let child = self.child_for(index, q);
            self.insert(child, bx, by, depth + 1);
        }
        let q = Self::quadrant(&quad, x, y);
        let child = self.child_for(index, q);
        self.insert(child, x, y, depth + 1);
    }

    fn finish(&mut self, index: usize) {
        let mass = self.nodes[index].mass;
        if mass > 0. {
            self.nodes[index].mx /= mass;
            self.nodes[index].my /= mass;
        }
        for q in 0..4 {
            if let Some(child) = self.nodes[index].children[q] {
                self.finish(child);
            }
        }
    }

    fn repulsion(&self, x: f32, y: f32, charge: f32, theta: f32) -> (f32, f32) {
        let mut fx = 0.;
        let mut fy = 0.;
        let mut stack = vec![0usize];
        while let Some(index) = stack.pop() {
            let quad = &self.nodes[index];
            if quad.mass == 0. {
                continue;
            }
            let dx = x - quad.mx;
            let dy = y - quad.my;
            let dist2 = dx * dx + dy * dy;
            let is_leaf = quad.children.iter().all(Option::is_none);
            if is_leaf || (quad.half * 2.) / dist2.sqrt().max(1e-3) < theta {
                if dist2 < 1e-4 {
                    continue;
                }
                let force = charge * quad.mass / dist2;
                let dist = dist2.sqrt();
                fx += dx / dist * force * 0.01;
                fy += dy / dist * force * 0.01;
            } else {
                for child in quad.children.iter().flatten() {
                    stack.push(*child);
                }
            }
        }
        (fx, fy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::{GraphEdge, GraphNode};
    use std::collections::BTreeMap;

    fn graph(n: usize) -> GraphData {
        let nodes = (0..n as u64)
            .map(|id| GraphNode {
                id,
                label: "T".into(),
                properties: BTreeMap::new(),
            })
            .collect();
        let edges = (1..n as u64)
            .map(|id| GraphEdge {
                id: 1000 + id,
                label: "E".into(),
                source: id - 1,
                target: id,
                properties: BTreeMap::new(),
            })
            .collect();
        GraphData {
            nodes,
            edges,
            dangling_edges: 0,
        }
    }

    #[test]
    fn layout_is_deterministic_and_separates_cards() {
        let g = graph(30);
        let mut a = ForceLayout::new(&g);
        let mut b = ForceLayout::new(&g);
        a.warm_up(100);
        b.warm_up(100);
        a.spread_cards();
        b.spread_cards();
        assert_eq!(a.nodes[7].x.to_bits(), b.nodes[7].x.to_bits());
        for i in 0..a.nodes.len() {
            for j in (i + 1)..a.nodes.len() {
                let dx = (a.nodes[i].x - a.nodes[j].x).abs();
                let dy = (a.nodes[i].y - a.nodes[j].y).abs();
                assert!(
                    dx >= CARD_WIDTH || dy >= CARD_HEIGHT,
                    "cards {i} and {j} overlap"
                );
            }
        }
        assert!(a.nodes.iter().all(|n| n.x.is_finite() && n.y.is_finite()));
    }
}
