//! The interactive graph canvas: a force layout painted with GPUI's low-level
//! API, with pan, zoom, drag, neighbourhood focus on hover, per-label colour,
//! a legend, zoom controls and a minimap.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, BoxShadow, Context, Corners, CursorStyle, Edges, EventEmitter, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, TextRun, Window, canvas, div, fill, font, point,
    prelude::*, px, quad, size,
};

use crate::results::{ElementKind, GraphData};
use crate::theme::{FONT_MONO, FONT_UI, Theme, theme, transparent};
use crate::ui::{format_count, icon};

use super::layout::{CARD_HEIGHT, CARD_WIDTH, ForceLayout};
use super::palette::{LabelPalette, slot_colour};

/// What the canvas reports to the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphEvent {
    /// A node or edge was clicked.
    Select(ElementKind, u64),
    /// A node was double-clicked.
    Expand(u64),
    /// The empty canvas was clicked.
    Clear,
}

const MIN_ZOOM: f32 = 0.08;
const MAX_ZOOM: f32 = 3.5;
const FIT_MAX_ZOOM: f32 = 1.35;
const DRAG_THRESHOLD: f32 = 3.;
const MINIMAP_LIMIT: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    Pending {
        start: Point<Pixels>,
        node: Option<usize>,
    },
    Pan {
        last: Point<Pixels>,
    },
    Node {
        index: usize,
        offset_x: f32,
        offset_y: f32,
    },
}

/// The canvas state.
pub struct GraphView {
    graph: Option<Arc<GraphData>>,
    layout: Option<ForceLayout>,
    palette: LabelPalette,
    adjacency: HashMap<u64, HashSet<u64>>,
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
    canvas_bounds: Bounds<Pixels>,
    hovered_node: Option<usize>,
    hovered_edge: Option<usize>,
    hovered_label: Option<String>,
    /// The selected element, mirrored from the workspace.
    pub selected: Option<(ElementKind, u64)>,
    drag: Drag,
    fit_pending: bool,
    moved_during_drag: bool,
}

impl EventEmitter<GraphEvent> for GraphView {}

/// Per-frame draw list, computed from the state once per paint.
struct Snapshot {
    nodes: Vec<NodeDraw>,
    edges: Vec<EdgeDraw>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    theme: Theme,
}

struct NodeDraw {
    x: f32,
    y: f32,
    colour: Hsla,
    caption: String,
    label: String,
    degree: usize,
    dimmed: bool,
    selected: bool,
    hovered: bool,
}

struct EdgeDraw {
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
    cx: f32,
    cy: f32,
    is_loop: bool,
    label: String,
    dimmed: bool,
    highlighted: bool,
}

impl GraphView {
    /// An empty canvas.
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            graph: None,
            layout: None,
            palette: LabelPalette::default(),
            adjacency: HashMap::new(),
            pan_x: 0.,
            pan_y: 0.,
            zoom: 1.,
            canvas_bounds: Bounds::default(),
            hovered_node: None,
            hovered_edge: None,
            hovered_label: None,
            selected: None,
            drag: Drag::None,
            fit_pending: true,
            moved_during_drag: false,
        }
    }

    /// Replaces the graph, lays it out and schedules a fit.
    pub fn set_graph(&mut self, graph: Option<Arc<GraphData>>, cx: &mut Context<Self>) {
        self.hovered_node = None;
        self.hovered_edge = None;
        self.hovered_label = None;
        self.drag = Drag::None;
        match &graph {
            Some(graph) => {
                self.palette = LabelPalette::new(graph.nodes.iter().map(|n| n.label.as_str()));
                let mut adjacency: HashMap<u64, HashSet<u64>> = HashMap::new();
                for edge in &graph.edges {
                    adjacency
                        .entry(edge.source)
                        .or_default()
                        .insert(edge.target);
                    adjacency
                        .entry(edge.target)
                        .or_default()
                        .insert(edge.source);
                }
                self.adjacency = adjacency;
                let mut layout = ForceLayout::new(graph);
                layout.warm_up(150);
                layout.spread_cards();
                self.layout = Some(layout);
            }
            None => {
                self.palette = LabelPalette::default();
                self.adjacency.clear();
                self.layout = None;
            }
        }
        self.graph = graph;
        self.fit_pending = true;
        cx.notify();
    }

    /// Updates the selection highlight.
    pub fn set_selected(&mut self, selected: Option<(ElementKind, u64)>, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    fn to_graph(&self, position: Point<Pixels>) -> (f32, f32) {
        let x = (f32::from(position.x - self.canvas_bounds.left()) - self.pan_x) / self.zoom;
        let y = (f32::from(position.y - self.canvas_bounds.top()) - self.pan_y) / self.zoom;
        (x, y)
    }

    fn fit(&mut self) {
        let Some(layout) = &self.layout else {
            return;
        };
        let (min_x, min_y, max_x, max_y) = layout.bounds();
        let width = f32::from(self.canvas_bounds.size.width);
        let height = f32::from(self.canvas_bounds.size.height);
        if width <= 0. || height <= 0. {
            return;
        }
        let graph_w = (max_x - min_x).max(1.) * 1.25;
        let graph_h = (max_y - min_y).max(1.) * 1.25;
        let zoom = (width / graph_w)
            .min(height / graph_h)
            .clamp(MIN_ZOOM, FIT_MAX_ZOOM);
        self.zoom = zoom;
        let cx = (min_x + max_x) / 2.;
        let cy = (min_y + max_y) / 2.;
        self.pan_x = width / 2. - cx * zoom;
        self.pan_y = height / 2. - cy * zoom;
    }

    fn zoom_by(&mut self, factor: f32, about: Point<Pixels>, cx: &mut Context<Self>) {
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let local_x = f32::from(about.x - self.canvas_bounds.left());
        let local_y = f32::from(about.y - self.canvas_bounds.top());
        let ratio = new_zoom / self.zoom;
        self.pan_x = local_x - (local_x - self.pan_x) * ratio;
        self.pan_y = local_y - (local_y - self.pan_y) * ratio;
        self.zoom = new_zoom;
        cx.notify();
    }

    fn zoom_step(&mut self, factor: f32, cx: &mut Context<Self>) {
        let center = self.canvas_bounds.center();
        self.zoom_by(factor, center, cx);
    }

    fn reset_layout(&mut self, cx: &mut Context<Self>) {
        if let Some(graph) = &self.graph {
            let mut layout = ForceLayout::new(graph);
            layout.warm_up(150);
            layout.spread_cards();
            self.layout = Some(layout);
            self.fit_pending = true;
            cx.notify();
        }
    }

    /// The edge whose curve passes within a few pixels of the point.
    fn edge_at(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let px_x = f32::from(position.x - self.canvas_bounds.left());
        let px_y = f32::from(position.y - self.canvas_bounds.top());
        let mut best: Option<(usize, f32)> = None;
        for (index, edge) in layout.edges.iter().enumerate() {
            let s = &layout.nodes[edge.source];
            let t = &layout.nodes[edge.target];
            let geometry = edge_geometry(
                s.x,
                s.y,
                t.x,
                t.y,
                edge.parallel_index,
                edge.parallel_count,
                edge.is_loop,
            );
            for step in 0..=16 {
                #[allow(clippy::cast_precision_loss)]
                let u = step as f32 / 16.;
                let (gx, gy) = geometry.point_at(u);
                let sx = gx * self.zoom + self.pan_x;
                let sy = gy * self.zoom + self.pan_y;
                let d = ((sx - px_x).powi(2) + (sy - px_y).powi(2)).sqrt();
                if d < 7. && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((index, d));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        let (gx, gy) = self.to_graph(event.position);
        let node = self.layout.as_ref().and_then(|l| l.node_at(gx, gy));
        if event.click_count == 2
            && let Some(index) = node
            && let Some(layout) = &self.layout
        {
            let id = layout.nodes[index].id;
            self.drag = Drag::None;
            cx.emit(GraphEvent::Expand(id));
            return;
        }
        self.drag = Drag::Pending {
            start: event.position,
            node,
        };
        self.moved_during_drag = false;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Drag::Pending { start, node } = self.drag {
            let dx = f32::from(event.position.x - start.x);
            let dy = f32::from(event.position.y - start.y);
            if (dx * dx + dy * dy).sqrt() < DRAG_THRESHOLD {
                return;
            }
            self.moved_during_drag = true;
            if let Some(index) = node
                && let Some(layout) = &mut self.layout
            {
                let (gx, gy) = {
                    let x =
                        (f32::from(start.x - self.canvas_bounds.left()) - self.pan_x) / self.zoom;
                    let y =
                        (f32::from(start.y - self.canvas_bounds.top()) - self.pan_y) / self.zoom;
                    (x, y)
                };
                let n = &mut layout.nodes[index];
                n.pinned = true;
                self.drag = Drag::Node {
                    index,
                    offset_x: gx - n.x,
                    offset_y: gy - n.y,
                };
            } else {
                self.drag = Drag::Pan { last: start };
            }
        }
        match self.drag {
            Drag::Pan { last } => {
                self.pan_x += f32::from(event.position.x - last.x);
                self.pan_y += f32::from(event.position.y - last.y);
                self.drag = Drag::Pan {
                    last: event.position,
                };
                cx.notify();
            }
            Drag::Node {
                index,
                offset_x,
                offset_y,
            } => {
                let (gx, gy) = self.to_graph(event.position);
                if let Some(layout) = &mut self.layout
                    && let Some(n) = layout.nodes.get_mut(index)
                {
                    n.x = gx - offset_x;
                    n.y = gy - offset_y;
                }
                cx.notify();
            }
            Drag::None => {
                let (gx, gy) = self.to_graph(event.position);
                let node = self.layout.as_ref().and_then(|l| l.node_at(gx, gy));
                let edge = if node.is_none() {
                    self.edge_at(event.position)
                } else {
                    None
                };
                if node != self.hovered_node || edge != self.hovered_edge {
                    self.hovered_node = node;
                    self.hovered_edge = edge;
                    cx.notify();
                }
            }
            Drag::Pending { .. } => {}
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        match drag {
            Drag::Pending { node, .. } => {
                if let Some(index) = node
                    && let Some(layout) = &self.layout
                {
                    let id = layout.nodes[index].id;
                    cx.emit(GraphEvent::Select(ElementKind::Node, id));
                } else if let Some(index) = self.edge_at(event.position)
                    && let Some(layout) = &self.layout
                {
                    let id = layout.edges[index].id;
                    cx.emit(GraphEvent::Select(ElementKind::Edge, id));
                } else {
                    cx.emit(GraphEvent::Clear);
                }
            }
            Drag::Node { index, .. } => {
                if let Some(layout) = &mut self.layout
                    && let Some(n) = layout.nodes.get_mut(index)
                {
                    n.pinned = false;
                }
            }
            Drag::Pan { .. } | Drag::None => {}
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let dy = f32::from(delta.y);
        if dy == 0. {
            return;
        }
        let factor = (1. - dy * 0.0035).clamp(0.6, 1.6);
        self.zoom_by(factor, event.position, cx);
    }

    fn snapshot(&self, theme: &Theme) -> Snapshot {
        let mode = theme.mode;
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        if let Some(layout) = &self.layout {
            let focus = self.hovered_node.or_else(|| {
                let (ElementKind::Node, id) = self.selected? else {
                    return None;
                };
                layout.index.get(&id).copied()
            });
            let visible: Option<HashSet<u64>> = focus.map(|index| {
                let id = layout.nodes[index].id;
                let mut set: HashSet<u64> = self.adjacency.get(&id).cloned().unwrap_or_default();
                set.insert(id);
                set
            });
            for (index, node) in layout.nodes.iter().enumerate() {
                let label_muted = self
                    .hovered_label
                    .as_ref()
                    .is_some_and(|l| *l != node.label);
                let neighbourhood_muted = visible.as_ref().is_some_and(|v| !v.contains(&node.id));
                nodes.push(NodeDraw {
                    x: node.x,
                    y: node.y,
                    colour: self.palette.colour(&node.label, mode),
                    caption: node.caption.clone(),
                    label: node.label.clone(),
                    degree: node.degree,
                    dimmed: label_muted || neighbourhood_muted,
                    selected: self.selected == Some((ElementKind::Node, node.id)),
                    hovered: self.hovered_node == Some(index),
                });
            }
            let focus_id = focus.map(|i| layout.nodes[i].id);
            for (index, edge) in layout.edges.iter().enumerate() {
                let s = &layout.nodes[edge.source];
                let t = &layout.nodes[edge.target];
                let label_muted = self
                    .hovered_label
                    .as_ref()
                    .is_some_and(|l| *l != s.label && *l != t.label);
                let neighbourhood_muted = focus_id.is_some_and(|f| f != s.id && f != t.id);
                let geometry = edge_geometry(
                    s.x,
                    s.y,
                    t.x,
                    t.y,
                    edge.parallel_index,
                    edge.parallel_count,
                    edge.is_loop,
                );
                edges.push(EdgeDraw {
                    sx: geometry.sx,
                    sy: geometry.sy,
                    tx: geometry.tx,
                    ty: geometry.ty,
                    cx: geometry.cx,
                    cy: geometry.cy,
                    is_loop: edge.is_loop,
                    label: edge.label.clone(),
                    dimmed: label_muted || neighbourhood_muted,
                    highlighted: self.selected == Some((ElementKind::Edge, edge.id))
                        || self.hovered_edge == Some(index),
                });
            }
        }
        Snapshot {
            nodes,
            edges,
            zoom: self.zoom,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
            theme: *theme,
        }
    }

    fn render_legend(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = *theme(cx);
        let total = self.graph.as_ref().map_or(0, |g| g.nodes.len());
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(4.))
            .max_h(px(250.))
            .id("legend-list")
            .overflow_y_scroll();
        for (index, entry) in self.palette.legend.iter().enumerate() {
            let label = entry.label.clone();
            let slot = entry.slot;
            let hover_label = slot.map(|_| label.clone());
            list = list.child(
                div()
                    .id(("legend", index))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .px(px(6.))
                    .py(px(5.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(t.surface2))
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        let next = if *hovering { hover_label.clone() } else { None };
                        if this.hovered_label != next {
                            this.hovered_label = next;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded(px(2.))
                            .bg(slot_colour(slot, t.mode)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(t.ink_secondary)
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(t.ink_muted)
                            .child(SharedString::from(format_count(entry.count))),
                    ),
            );
        }
        div()
            .absolute()
            .left(px(14.))
            .bottom(px(14.))
            .w(px(178.))
            .rounded(px(4.))
            .border_1()
            .border_color(t.hairline)
            .bg(t.mix(t.elevated, transparent(), 0.94))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .px(px(9.))
                    .pt(px(7.))
                    .pb(px(6.))
                    .border_b_1()
                    .border_color(t.hairline)
                    .font_family(FONT_UI)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(px(9.))
                    .text_color(t.ink_muted)
                    .child("NODE LABELS")
                    .child(SharedString::from(format!(
                        "{} TOTAL",
                        format_count(total as u64)
                    ))),
            )
            .child(list)
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = *theme(cx);
        let control = |id: &'static str,
                       name: &'static str,
                       cx: &mut Context<Self>,
                       on: fn(&mut Self, &mut Context<Self>)| {
            div()
                .id(id)
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .border_b_1()
                .border_color(t.hairline)
                .text_color(t.ink_secondary)
                .cursor_pointer()
                .hover(move |s| s.bg(t.mix(t.elevated, t.accent, 0.84)).text_color(t.ink))
                .on_click(cx.listener(move |this, _, _, cx| on(this, cx)))
                .child(icon(name, 15.).text_color(t.ink_secondary))
        };
        div()
            .absolute()
            .top(px(14.))
            .right(px(14.))
            .flex()
            .flex_col()
            .rounded(px(4.))
            .border_1()
            .border_color(t.hairline)
            .bg(t.mix(t.elevated, transparent(), 0.96))
            .overflow_hidden()
            .child(control("zoom-in", "plus", cx, |this, cx| {
                this.zoom_step(1.2, cx)
            }))
            .child(control("zoom-out", "minus", cx, |this, cx| {
                this.zoom_step(1. / 1.2, cx)
            }))
            .child(control("zoom-fit", "maximize", cx, |this, cx| {
                this.fit();
                cx.notify();
            }))
            .child(control("relayout", "rotate-ccw", cx, |this, cx| {
                this.reset_layout(cx)
            }))
            .into_any_element()
    }

    fn render_minimap(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let layout = self.layout.as_ref()?;
        if layout.nodes.len() > MINIMAP_LIMIT || layout.nodes.is_empty() {
            return None;
        }
        let t = *theme(cx);
        let mode = t.mode;
        let (min_x, min_y, max_x, max_y) = layout.bounds();
        let map_w = 160.;
        let map_h = 110.;
        let pad = 8.;
        let gw = (max_x - min_x).max(1.);
        let gh = (max_y - min_y).max(1.);
        let scale = ((map_w - pad * 2.) / gw).min((map_h - pad * 2.) / gh);
        let ox = pad + ((map_w - pad * 2.) - gw * scale) / 2.;
        let oy = pad + ((map_h - pad * 2.) - gh * scale) / 2.;
        let rects: Vec<(f32, f32, f32, f32, Hsla)> = layout
            .nodes
            .iter()
            .map(|n| {
                (
                    ox + (n.x - CARD_WIDTH / 2. - min_x) * scale,
                    oy + (n.y - CARD_HEIGHT / 2. - min_y) * scale,
                    (CARD_WIDTH * scale).max(2.),
                    (CARD_HEIGHT * scale).max(1.5),
                    self.palette.colour(&n.label, mode),
                )
            })
            .collect();
        // Viewport rectangle in graph space.
        let vw = f32::from(self.canvas_bounds.size.width) / self.zoom;
        let vh = f32::from(self.canvas_bounds.size.height) / self.zoom;
        let vx = -self.pan_x / self.zoom;
        let vy = -self.pan_y / self.zoom;
        let viewport = (
            ox + (vx - min_x) * scale,
            oy + (vy - min_y) * scale,
            vw * scale,
            vh * scale,
        );
        let minimap_canvas = canvas(
            move |_, _, _| {},
            move |bounds, (), window, _| {
                for (x, y, w, h, colour) in &rects {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + px(*x), bounds.top() + px(*y)),
                            size(px(*w), px(*h)),
                        ),
                        *colour,
                    ));
                }
                let (x, y, w, h) = viewport;
                let x0 = bounds.left() + px(x.clamp(0., map_w));
                let y0 = bounds.top() + px(y.clamp(0., map_h));
                let x1 = bounds.left() + px((x + w).clamp(0., map_w));
                let y1 = bounds.top() + px((y + h).clamp(0., map_h));
                window.paint_quad(quad(
                    Bounds::from_corners(point(x0, y0), point(x1, y1)),
                    Corners::all(px(2.)),
                    t.mix(t.accent, transparent(), 0.08),
                    Edges::all(px(1.)),
                    t.accent,
                    BorderStyle::Solid,
                ));
            },
        )
        .size_full();
        let (bx, by, bscale) = (min_x - ox / scale, min_y - oy / scale, scale);
        Some(
            div()
                .id("minimap")
                .absolute()
                .right(px(14.))
                .bottom(px(14.))
                .w(px(map_w))
                .h(px(map_h))
                .rounded(px(4.))
                .border_1()
                .border_color(t.hairline)
                .bg(t.mix(t.elevated, transparent(), 0.94))
                .overflow_hidden()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        // Centre the viewport on the clicked spot.
                        let local_x = f32::from(event.position.x)
                            - f32::from(this.canvas_bounds.right())
                            + 14.
                            + map_w;
                        let local_y = f32::from(event.position.y)
                            - f32::from(this.canvas_bounds.bottom())
                            + 14.
                            + map_h;
                        let gx = bx + local_x / bscale;
                        let gy = by + local_y / bscale;
                        let w = f32::from(this.canvas_bounds.size.width);
                        let h = f32::from(this.canvas_bounds.size.height);
                        this.pan_x = w / 2. - gx * this.zoom;
                        this.pan_y = h / 2. - gy * this.zoom;
                        cx.notify();
                    }),
                )
                .child(minimap_canvas)
                .into_any_element(),
        )
    }
}

/// Where an edge leaves and enters its cards, and its curve control point.
struct EdgeGeometry {
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
    cx: f32,
    cy: f32,
    is_loop: bool,
}

impl EdgeGeometry {
    fn point_at(&self, t: f32) -> (f32, f32) {
        if self.is_loop {
            // Cubic loop above the card.
            let (x0, y0) = (self.sx, self.sy);
            let (x3, y3) = (self.tx, self.ty);
            let (x1, y1) = (x0 + 84., y0 - 92.);
            let (x2, y2) = (x3 - 84., y3 - 92.);
            let u = 1. - t;
            (
                u * u * u * x0 + 3. * u * u * t * x1 + 3. * u * t * t * x2 + t * t * t * x3,
                u * u * u * y0 + 3. * u * u * t * y1 + 3. * u * t * t * y2 + t * t * t * y3,
            )
        } else {
            let u = 1. - t;
            (
                u * u * self.sx + 2. * u * t * self.cx + t * t * self.tx,
                u * u * self.sy + 2. * u * t * self.cy + t * t * self.ty,
            )
        }
    }
}

fn edge_geometry(
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
    parallel_index: usize,
    parallel_count: usize,
    is_loop: bool,
) -> EdgeGeometry {
    if is_loop {
        return EdgeGeometry {
            sx: sx + CARD_WIDTH / 2.,
            sy,
            tx,
            ty: ty - CARD_HEIGHT / 2.,
            cx: sx,
            cy: sy,
            is_loop: true,
        };
    }
    let dx = tx - sx;
    let dy = ty - sy;
    let (ax, ay, bx, by) = if dx.abs() >= dy.abs() {
        if dx >= 0. {
            (sx + CARD_WIDTH / 2., sy, tx - CARD_WIDTH / 2., ty)
        } else {
            (sx - CARD_WIDTH / 2., sy, tx + CARD_WIDTH / 2., ty)
        }
    } else if dy >= 0. {
        (sx, sy + CARD_HEIGHT / 2., tx, ty - CARD_HEIGHT / 2.)
    } else {
        (sx, sy - CARD_HEIGHT / 2., tx, ty + CARD_HEIGHT / 2.)
    };
    #[allow(clippy::cast_precision_loss)]
    let centred = parallel_index as f32 - (parallel_count as f32 - 1.) / 2.;
    let offset = centred * 28.;
    let ddx = bx - ax;
    let ddy = by - ay;
    let length = (ddx * ddx + ddy * ddy).sqrt().max(1.);
    EdgeGeometry {
        sx: ax,
        sy: ay,
        tx: bx,
        ty: by,
        cx: (ax + bx) / 2. - ddy / length * offset,
        cy: (ay + by) / 2. + ddx / length * offset,
        is_loop: false,
    }
}

fn with_alpha(colour: Hsla, factor: f32) -> Hsla {
    Hsla {
        a: colour.a * factor,
        ..colour
    }
}

/// Shapes a caption truncated to a pixel width.
fn fit_text(
    text: &str,
    max_width: Pixels,
    font_size: Pixels,
    run_font: gpui::Font,
    colour: Hsla,
    window: &Window,
) -> gpui::ShapedLine {
    let mut candidate = text.to_owned();
    loop {
        let run = TextRun {
            len: candidate.len(),
            font: run_font.clone(),
            color: colour,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            SharedString::from(candidate.clone()),
            font_size,
            &[run],
            None,
        );
        if line.width <= max_width || candidate.chars().count() <= 2 {
            return line;
        }
        let keep = candidate.chars().count().saturating_sub(2).max(1);
        candidate = candidate
            .chars()
            .take(keep)
            .collect::<String>()
            .trim_end()
            .to_owned()
            + "…";
    }
}

fn paint_snapshot(snapshot: &Snapshot, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let t = &snapshot.theme;
    let zoom = snapshot.zoom;
    let origin = bounds.origin;
    let to_screen = |x: f32, y: f32| {
        point(
            origin.x + px(x * zoom + snapshot.pan_x),
            origin.y + px(y * zoom + snapshot.pan_y),
        )
    };

    // Dot grid, moving with the viewport like a plotting surface.
    let gap = 22. * zoom;
    if gap >= 9. {
        let dot = px((1.2 * zoom).clamp(0.8, 2.));
        let colour = t.mix(t.ink_muted, transparent(), 0.30);
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let start_x = snapshot.pan_x.rem_euclid(gap);
        let start_y = snapshot.pan_y.rem_euclid(gap);
        let mut y = start_y;
        while y < h {
            let mut x = start_x;
            while x < w {
                window.paint_quad(quad(
                    Bounds::new(point(origin.x + px(x), origin.y + px(y)), size(dot, dot)),
                    Corners::all(dot),
                    colour,
                    Edges::all(px(0.)),
                    transparent(),
                    BorderStyle::Solid,
                ));
                x += gap;
            }
            y += gap;
        }
    }

    // Edges first so cards sit on top.
    let edge_colour = t.mix(t.ink_muted, transparent(), 0.58);
    let mut labelled: Vec<(Point<Pixels>, &str)> = Vec::new();
    for edge in &snapshot.edges {
        let alpha = if edge.dimmed { 0.12 } else { 1. };
        let colour = if edge.highlighted {
            t.accent
        } else {
            edge_colour
        };
        let colour = with_alpha(colour, alpha);
        let width = px((if edge.highlighted { 2.25 } else { 1.25 } * zoom).clamp(0.75, 4.));
        let mut builder = PathBuilder::stroke(width);
        let start = to_screen(edge.sx, edge.sy);
        let end = to_screen(edge.tx, edge.ty);
        builder.move_to(start);
        if edge.is_loop {
            let c1 = to_screen(edge.sx + 84., edge.sy - 92.);
            let c2 = to_screen(edge.tx - 84., edge.ty - 92.);
            builder.cubic_bezier_to(end, c1, c2);
        } else {
            builder.curve_to(end, to_screen(edge.cx, edge.cy));
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, colour);
        }
        // Arrowhead at the target, aligned with the curve's end tangent.
        let (tan_x, tan_y) = if edge.is_loop {
            (edge.tx - (edge.tx - 84.), edge.ty - (edge.ty - 92.))
        } else {
            (edge.tx - edge.cx, edge.ty - edge.cy)
        };
        let len = (tan_x * tan_x + tan_y * tan_y).sqrt().max(0.001);
        let (ux, uy) = (tan_x / len, tan_y / len);
        let size_px = (14. * zoom).clamp(5., 22.);
        let tip = to_screen(edge.tx, edge.ty);
        let base_x = f32::from(tip.x) - ux * size_px;
        let base_y = f32::from(tip.y) - uy * size_px;
        let half = size_px * 0.42;
        let mut head = PathBuilder::fill();
        head.move_to(tip);
        head.line_to(point(px(base_x - uy * half), px(base_y + ux * half)));
        head.line_to(point(px(base_x + uy * half), px(base_y - ux * half)));
        head.close();
        if let Ok(path) = head.build() {
            window.paint_path(path, colour);
        }
        if edge.highlighted && !edge.label.is_empty() {
            let mid = if edge.is_loop {
                to_screen((edge.sx + edge.tx) / 2., edge.sy.min(edge.ty) - 70.)
            } else {
                to_screen(
                    (edge.sx + 2. * edge.cx + edge.tx) / 4.,
                    (edge.sy + 2. * edge.cy + edge.ty) / 4.,
                )
            };
            labelled.push((mid, edge.label.as_str()));
        }
    }

    let card_w = px(CARD_WIDTH * zoom);
    let card_h = px(CARD_HEIGHT * zoom);
    let radius = Corners::all(px(5. * zoom));
    let bold_font = gpui::Font {
        weight: gpui::FontWeight::SEMIBOLD,
        ..font(FONT_UI)
    };
    let mono_font = font(FONT_MONO);
    let draw_text = zoom >= 0.3;
    for node in &snapshot.nodes {
        let alpha = if node.dimmed { 0.12 } else { 1. };
        let center = to_screen(node.x, node.y);
        let card = Bounds::new(
            point(center.x - card_w / 2., center.y - card_h / 2.),
            size(card_w, card_h),
        );
        if !node.dimmed {
            window.paint_shadows(
                card,
                radius,
                &[BoxShadow {
                    color: with_alpha(gpui::black(), 0.08),
                    offset: point(px(0.), px(2. * zoom)),
                    blur_radius: px(6. * zoom),
                    spread_radius: px(0.),
                }],
            );
        }
        let border_colour = if node.selected {
            t.accent
        } else if node.hovered {
            t.mix(node.colour, t.border, 0.55)
        } else {
            t.mix(t.border, transparent(), 0.82)
        };
        if node.selected {
            let ring = Bounds::new(
                point(card.left() - px(3. * zoom), card.top() - px(3. * zoom)),
                size(card_w + px(6. * zoom), card_h + px(6. * zoom)),
            );
            window.paint_quad(quad(
                ring,
                Corners::all(px(8. * zoom)),
                with_alpha(t.mix(t.accent, transparent(), 0.20), alpha),
                Edges::all(px(0.)),
                transparent(),
                BorderStyle::Solid,
            ));
        }
        window.paint_quad(quad(
            card,
            radius,
            with_alpha(t.mix(t.elevated, transparent(), 0.96), alpha),
            Edges::all(px(1.)),
            with_alpha(border_colour, alpha),
            BorderStyle::Solid,
        ));
        // Accent bar.
        window.paint_quad(quad(
            Bounds::new(
                point(card.left() + px(9. * zoom), center.y - px(14. * zoom)),
                size(px(4. * zoom), px(28. * zoom)),
            ),
            Corners::all(px(3. * zoom)),
            with_alpha(node.colour, alpha),
            Edges::all(px(0.)),
            transparent(),
            BorderStyle::Solid,
        ));
        // Port dot.
        let port = px(7. * zoom);
        window.paint_quad(quad(
            Bounds::new(
                point(card.right() - px(12. * zoom) - port, center.y - port / 2.),
                size(port, port),
            ),
            Corners::all(port),
            transparent(),
            Edges::all(px(2. * zoom)),
            with_alpha(t.mix(node.colour, t.ink, 0.70), alpha * 0.68),
            BorderStyle::Solid,
        ));
        if draw_text {
            let text_x = card.left() + px(23. * zoom);
            let max_width = card_w - px(23. * zoom) - px(26. * zoom);
            let caption = fit_text(
                &node.caption,
                max_width,
                px(12. * zoom),
                bold_font.clone(),
                with_alpha(t.ink, alpha),
                window,
            );
            caption
                .paint(
                    point(text_x, card.top() + px(9. * zoom)),
                    px(14.5 * zoom),
                    window,
                    cx,
                )
                .ok();
            let meta = fit_text(
                &format!("{} · {}", node.label, node.degree),
                max_width,
                px(9.5 * zoom),
                mono_font.clone(),
                with_alpha(t.ink_muted, alpha),
                window,
            );
            meta.paint(
                point(text_x, card.top() + px(27. * zoom)),
                px(12. * zoom),
                window,
                cx,
            )
            .ok();
        }
    }

    // Edge labels float above the cards.
    if draw_text {
        for (mid, label) in labelled {
            let line = fit_text(
                label,
                px(160. * zoom),
                px(9.5 * zoom),
                mono_font.clone(),
                t.ink_secondary,
                window,
            );
            let pad_x = px(7. * zoom);
            let pad_y = px(3. * zoom);
            let w = line.width + pad_x * 2.;
            let h = px(12. * zoom) + pad_y * 2.;
            let pill = Bounds::new(point(mid.x - w / 2., mid.y - h / 2.), size(w, h));
            window.paint_shadows(
                pill,
                Corners::all(px(5. * zoom)),
                &[BoxShadow {
                    color: with_alpha(gpui::black(), 0.14),
                    offset: point(px(0.), px(2. * zoom)),
                    blur_radius: px(8. * zoom),
                    spread_radius: px(0.),
                }],
            );
            window.paint_quad(quad(
                pill,
                Corners::all(px(5. * zoom)),
                t.mix(t.elevated, transparent(), 0.96),
                Edges::all(px(1.)),
                t.hairline,
                BorderStyle::Solid,
            ));
            line.paint(
                point(pill.left() + pad_x, pill.top() + pad_y),
                px(12. * zoom),
                window,
                cx,
            )
            .ok();
        }
    }
}

impl Render for GraphView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme(cx);
        let entity = cx.entity();
        let cursor = match self.drag {
            Drag::Pan { .. } => CursorStyle::ClosedHand,
            Drag::Node { .. } => CursorStyle::ClosedHand,
            Drag::Pending { .. } => CursorStyle::ClosedHand,
            Drag::None if self.hovered_node.is_some() || self.hovered_edge.is_some() => {
                CursorStyle::PointingHand
            }
            Drag::None => CursorStyle::OpenHand,
        };
        let painter = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |view, cx| {
                    let resized = view.canvas_bounds.size != bounds.size;
                    view.canvas_bounds = bounds;
                    if view.fit_pending
                        || (resized && view.layout.is_some() && view.zoom == 1. && view.pan_x == 0.)
                    {
                        view.fit();
                        view.fit_pending = false;
                    }
                    view.snapshot(theme(cx))
                })
            },
            |bounds, snapshot, window, cx| {
                window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                    paint_snapshot(&snapshot, bounds, window, cx);
                });
            },
        )
        .absolute()
        .inset_0()
        .size_full();

        let minimap = self.render_minimap(cx);
        div()
            .id("graph-canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(t.surface0)
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(painter)
            .child(
                div()
                    .absolute()
                    .top(px(14.))
                    .left(px(14.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(t.hairline)
                    .bg(t.mix(t.elevated, transparent(), 0.90))
                    .text_size(px(10.))
                    .text_color(t.ink_muted)
                    .child("Drag nodes · scroll to zoom · drag canvas to pan · double-click a node to expand"),
            )
            .child(self.render_controls(cx))
            .child(self.render_legend(cx))
            .children(minimap)
    }
}
