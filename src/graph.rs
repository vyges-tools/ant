// SPDX-License-Identifier: Apache-2.0
//! Per-gate metal attribution by walking the routed shape graph.
//!
//! # Why a graph and not a sum
//!
//! The antenna ratio protects one gate. The charge that reaches it is collected by the metal
//! *electrically connected to that gate at the moment a given layer is deposited* — which is the
//! routing reachable from it over layers at or below that one. Two gates on the same net can sit
//! on different branches and collect very different metal, right up until a higher layer joins
//! them.
//!
//! Summing a net's metal per layer and charging all of it to every gate is therefore wrong in
//! both directions: it over-charges gates on small branches, and it hides the staged nature of
//! the check. Measured against OpenROAD `check_antennas` on one net, the summing version reported
//! 5685.6 for all five pins where OpenROAD gives 720.2 for four of them and 2786.0 for the fifth.
//!
//! # The model
//!
//! Manufacturing order is bottom-up, so the check is evaluated per **stage**: at stage *L* only
//! shapes on layers ≤ *L* exist, and only vias whose upper layer is ≤ *L* have been cut. Metal
//! reachable from a gate under that restriction is what that gate has collected.
//!
//! Connectivity is geometric: shapes on the same layer that touch or overlap are one conductor
//! (abutment counts — two segments sharing an edge are one piece of metal), and a via joins the
//! shapes it lands on across its two layers.

use std::collections::HashMap;
use vyges_opendb::WireShape;

/// Union-find over shape indices. Small and local rather than a dependency: the whole structure
/// is thirty lines and carries no geometry opinions of its own.
struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n as u32).collect() }
    }
    fn find(&mut self, mut i: u32) -> u32 {
        while self.parent[i as usize] != i {
            // Path halving: keeps find near-constant without a second pass.
            self.parent[i as usize] = self.parent[self.parent[i as usize] as usize];
            i = self.parent[i as usize];
        }
        i
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra as usize] = rb;
        }
    }
}

/// Area and perimeter of the **union** of axis-aligned rectangles, in DBU.
///
/// Summing rectangles individually double-counts, and for perimeter it is much worse than for
/// area: overlap inflates area only where shapes actually overlap, but *every* junction between
/// abutting collinear segments contributes two interior edges that exist in no physical wire. A
/// routed net is delivered as many small rectangles, so the inflation is systematic rather than
/// occasional — and on technologies whose only antenna limit is a side-area ratio (sky130's
/// `DiffPSR`), perimeter *is* the numerator.
///
/// Exact, by coordinate compression: the distinct x and y edges cut the plane into cells, each
/// wholly inside or outside the union. Area is the covered cells; perimeter is every cell edge
/// with cover on one side and not the other. No floating point, no tolerance.
pub fn union_area_perimeter(rects: &[(i32, i32, i32, i32)]) -> (i64, i64) {
    let rects: Vec<_> = rects.iter().filter(|r| r.2 > r.0 && r.3 > r.1).copied().collect();
    if rects.is_empty() {
        return (0, 0);
    }
    let mut xs: Vec<i32> = rects.iter().flat_map(|r| [r.0, r.2]).collect();
    let mut ys: Vec<i32> = rects.iter().flat_map(|r| [r.1, r.3]).collect();
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let (nx, ny) = (xs.len() - 1, ys.len() - 1);
    if nx == 0 || ny == 0 {
        return (0, 0);
    }

    let mut covered = vec![false; nx * ny];
    for &(x0, y0, x1, y1) in &rects {
        let i0 = xs.partition_point(|&v| v < x0);
        let i1 = xs.partition_point(|&v| v < x1);
        let j0 = ys.partition_point(|&v| v < y0);
        let j1 = ys.partition_point(|&v| v < y1);
        for i in i0..i1 {
            for j in j0..j1 {
                covered[i * ny + j] = true;
            }
        }
    }

    let cov = |i: isize, j: isize| -> bool {
        i >= 0 && j >= 0 && (i as usize) < nx && (j as usize) < ny && covered[i as usize * ny + j as usize]
    };

    let (mut area, mut perim) = (0i64, 0i64);
    for i in 0..nx {
        let dx = (xs[i + 1] - xs[i]) as i64;
        for j in 0..ny {
            if !covered[i * ny + j] {
                continue;
            }
            let dy = (ys[j + 1] - ys[j]) as i64;
            area += dx * dy;
            // A cell edge is on the boundary of the union exactly when its neighbour is not
            // covered — which is what makes interior junction edges vanish.
            let (i, j) = (i as isize, j as isize);
            if !cov(i - 1, j) {
                perim += dy;
            }
            if !cov(i + 1, j) {
                perim += dy;
            }
            if !cov(i, j - 1) {
                perim += dx;
            }
            if !cov(i, j + 1) {
                perim += dx;
            }
        }
    }
    (area, perim)
}

/// Metal collected by one gate at one stage, in DBU units (area DBU², perimeter DBU).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Collected {
    /// Metal on the stage layer itself — the partial (PAR/PSR) numerator.
    pub layer_area: i64,
    pub layer_perimeter: i64,
    /// Metal on the stage layer and everything below it — the cumulative (CAR/CSR) numerator.
    pub cumulative_area: i64,
    pub cumulative_perimeter: i64,
}

/// The routed shapes of one net, indexed for repeated stage queries.
pub struct WireGraph {
    shapes: Vec<WireShape>,
    /// Routing layer numbers present on this net, ascending — the stages to evaluate.
    pub layers: Vec<i64>,
}

impl WireGraph {
    pub fn new(shapes: Vec<WireShape>) -> Self {
        let mut layers: Vec<i64> =
            shapes.iter().filter(|s| !s.is_via && s.layer >= 0).map(|s| s.layer).collect();
        layers.sort_unstable();
        layers.dedup();
        Self { shapes, layers }
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Index of the shape where a pin meets the metal.
    ///
    /// Containment first, on the lowest layer that covers the point: a pin connects at the
    /// bottom of the stack, and a higher layer merely passing overhead is a different
    /// conductor until a via joins them.
    ///
    /// Falling back to the **nearest** shape is not a nicety — it is required. The pin location
    /// odb reports is the terminal's average point, while the router lands on an access point
    /// somewhere inside the pin rectangle, so the wire frequently stops short of the centroid.
    /// Demanding containment left 9831 of ~10000 gates unanchored on a real block, i.e. almost
    /// every gate silently unchecked. The pin is known to be on this net (it came from the
    /// net's own terminal list), so the closest piece of the net's metal is the conductor it
    /// reaches; distance only breaks the tie.
    pub fn anchor(&self, x: i32, y: i32) -> Option<usize> {
        let routing = || {
            self.shapes.iter().enumerate().filter(|(_, s)| !s.is_via && s.layer >= 0)
        };
        if let Some((i, _)) =
            routing().filter(|(_, s)| s.contains(x, y)).min_by_key(|(_, s)| s.layer)
        {
            return Some(i);
        }
        // Squared distance from the point to the rectangle (0 inside), then lowest layer.
        routing()
            .min_by_key(|(_, s)| {
                let dx = (s.x0 - x).max(0).max(x - s.x1) as i64;
                let dy = (s.y0 - y).max(0).max(y - s.y1) as i64;
                (dx * dx + dy * dy, s.layer)
            })
            .map(|(i, _)| i)
    }

    /// Connected components of the routing as it exists at `stage` — shapes on layers ≤ stage,
    /// joined where they touch, plus vias whose upper layer has been cut by then.
    fn components_at(&self, stage: i64) -> UnionFind {
        let mut uf = UnionFind::new(self.shapes.len());

        // Same-layer contact. Bucketed by layer so this is not an all-pairs sweep across the
        // whole net; within a layer it still is, which is fine for the shape counts a single
        // net carries.
        let mut by_layer: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, s) in self.shapes.iter().enumerate() {
            if !s.is_via && s.layer >= 0 && s.layer <= stage {
                by_layer.entry(s.layer).or_default().push(i);
            }
        }
        for idxs in by_layer.values() {
            for (a, &i) in idxs.iter().enumerate() {
                for &j in &idxs[a + 1..] {
                    if self.shapes[i].touches(&self.shapes[j]) {
                        uf.union(i as u32, j as u32);
                    }
                }
            }
        }

        // Vias join across layers, but only once cut — a via whose upper layer is above the
        // stage does not exist yet, and treating it as present is exactly the error that makes
        // a staged check collapse into a flat one.
        for (vi, v) in self.shapes.iter().enumerate() {
            if !v.is_via || v.via_top > stage || v.via_bottom < 0 {
                continue;
            }
            for (i, s) in self.shapes.iter().enumerate() {
                if s.is_via || s.layer < 0 || s.layer > stage {
                    continue;
                }
                if (s.layer == v.via_bottom || s.layer == v.via_top) && s.touches(v) {
                    uf.union(vi as u32, i as u32);
                }
            }
        }
        uf
    }

    /// Group gates into the conductors they share at `stage`, with what each conductor has
    /// collected.
    ///
    /// `anchors[g]` is the shape gate `g` attaches to. Returns one entry per distinct
    /// conductor: the gate indices on it, and its metal.
    ///
    /// Grouping matters because the ratio is charged **per region, not per gate**. The charge a
    /// conductor collects is shared by every gate connected to it, so the denominator is the
    /// summed gate area of the region. Measured against OpenROAD on one net: our region metal
    /// of 1603.33 µm² over the region's three gates (0.4347 + 0.4347 + 0.126 = 0.9954) gives
    /// 1610.7 against their 1611.2 — 0.03%. Charging each gate its own area instead gave 7.9×.
    pub fn regions_at(&self, stage: i64, anchors: &[usize]) -> Vec<(Vec<usize>, Collected)> {
        let mut uf = self.components_at(stage);
        let mut by_root: HashMap<u32, Vec<usize>> = HashMap::new();
        for (g, &a) in anchors.iter().enumerate() {
            by_root.entry(uf.find(a as u32)).or_default().push(g);
        }
        // Shapes of each conductor, per layer, for the union measure.
        let mut shapes_by_root: HashMap<u32, HashMap<i64, Vec<(i32, i32, i32, i32)>>> =
            HashMap::new();
        for (i, s) in self.shapes.iter().enumerate() {
            if s.is_via || s.layer < 0 || s.layer > stage {
                continue;
            }
            let root = uf.find(i as u32);
            if !by_root.contains_key(&root) {
                continue; // a conductor with no gate on it collects for nobody
            }
            shapes_by_root
                .entry(root)
                .or_default()
                .entry(s.layer)
                .or_default()
                .push((s.x0, s.y0, s.x1, s.y1));
        }

        by_root
            .into_iter()
            .map(|(root, gates)| {
                let mut c = Collected::default();
                if let Some(by_layer) = shapes_by_root.get(&root) {
                    for (layer, rects) in by_layer {
                        let (area, perim) = union_area_perimeter(rects);
                        c.cumulative_area += area;
                        c.cumulative_perimeter += perim;
                        if *layer == stage {
                            c.layer_area = area;
                            c.layer_perimeter = perim;
                        }
                    }
                }
                (gates, c)
            })
            .collect()
    }

    /// What the gate anchored at `anchor` has collected at each stage.
    ///
    /// Returns one entry per routing layer in [`WireGraph::layers`], ascending.
    #[cfg(test)]
    pub fn collected_by_stage(&self, anchor: usize) -> Vec<(i64, Collected)> {
        self.layers
            .iter()
            .map(|&stage| {
                let mut uf = self.components_at(stage);
                let root = uf.find(anchor as u32);

                // Gather this gate's conductor per layer, then union each layer. Unioning is
                // per layer because layers are physically separate sheets of metal: shapes on
                // different layers never share a boundary, so their perimeters simply add.
                let mut by_layer: HashMap<i64, Vec<(i32, i32, i32, i32)>> = HashMap::new();
                for (i, s) in self.shapes.iter().enumerate() {
                    if s.is_via || s.layer < 0 || s.layer > stage {
                        continue;
                    }
                    if uf.find(i as u32) != root {
                        continue; // a different conductor at this stage
                    }
                    by_layer.entry(s.layer).or_default().push((s.x0, s.y0, s.x1, s.y1));
                }

                let mut c = Collected::default();
                for (layer, rects) in &by_layer {
                    let (area, perim) = union_area_perimeter(rects);
                    c.cumulative_area += area;
                    c.cumulative_perimeter += perim;
                    if *layer == stage {
                        c.layer_area = area;
                        c.layer_perimeter = perim;
                    }
                }
                (stage, c)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metal(layer: i64, x0: i32, x1: i32) -> WireShape {
        WireShape { layer, x0, y0: 0, x1, y1: 10, is_via: false, via_bottom: -1, via_top: -1 }
    }
    fn via(bot: i64, top: i64, x: i32) -> WireShape {
        WireShape {
            layer: -1,
            x0: x,
            y0: 0,
            x1: x + 2,
            y1: 2,
            is_via: true,
            via_bottom: bot,
            via_top: top,
        }
    }

    /// Two branches on layer 1 that only meet through layer 2. Before that stage each gate sees
    /// its own branch; at the joining stage both see everything. This is the whole reason the
    /// walk exists.
    #[test]
    fn branches_join_only_when_the_upper_layer_arrives() {
        let g = WireGraph::new(vec![
            metal(1, 0, 100),   // branch A
            metal(1, 200, 300), // branch B — disjoint from A on layer 1
            via(1, 2, 90),
            via(1, 2, 205),
            metal(2, 80, 220), // the layer-2 bridge over both vias
        ]);
        assert_eq!(g.layers, vec![1, 2]);

        let a = g.anchor(50, 5).expect("anchored in branch A");
        let by_stage = g.collected_by_stage(a);

        // Stage 1: only branch A's own metal (100 x 10).
        assert_eq!(by_stage[0].0, 1);
        assert_eq!(by_stage[0].1.layer_area, 1000, "branch B must not count yet");

        // Stage 2: the bridge is cut, so both branches and the bridge are one conductor.
        assert_eq!(by_stage[1].0, 2);
        assert_eq!(by_stage[1].1.cumulative_area, 1000 + 1000 + 140 * 10);
    }

    /// A via above the current stage has not been cut, so it must not connect anything.
    #[test]
    fn uncut_vias_do_not_connect() {
        let g = WireGraph::new(vec![metal(1, 0, 100), via(1, 5, 50), metal(5, 0, 100)]);
        let a = g.anchor(50, 5).unwrap();
        let s1 = g.collected_by_stage(a)[0];
        assert_eq!(s1.0, 1);
        assert_eq!(s1.1.cumulative_area, 1000, "layer 5 metal is not connected at stage 1");
    }

    /// Abutting shapes are one conductor: sharing an edge is an electrical connection, and
    /// treating it as a gap would split routed wires into fictitious pieces.
    ///
    /// The perimeter is the giveaway. Two 50x10 rectangles summed separately give 240; the
    /// single 100x10 wire they actually form has a perimeter of 220. That 20 is the shared
    /// edge counted twice — an edge no physical wire has.
    #[test]
    fn abutting_shapes_are_one_conductor() {
        let g = WireGraph::new(vec![metal(1, 0, 50), metal(1, 50, 100)]);
        let a = g.anchor(10, 5).unwrap();
        let c = g.collected_by_stage(a)[0].1;
        assert_eq!(c.layer_area, 1000, "100 x 10");
        assert_eq!(c.layer_perimeter, 220, "2*(100+10) — not 240, which counts the join twice");
    }

    // ---- union geometry ---------------------------------------------------------------

    /// One rectangle is its own union.
    #[test]
    fn union_of_a_single_rect() {
        assert_eq!(union_area_perimeter(&[(0, 0, 10, 4)]), (40, 28));
    }

    /// The case the fix exists for: a wire delivered as many abutting pieces must measure as
    /// the wire, not as the pieces.
    #[test]
    fn abutting_run_measures_as_one_wire() {
        let pieces: Vec<_> = (0..10).map(|i| (i * 10, 0, i * 10 + 10, 4)).collect();
        let (area, perim) = union_area_perimeter(&pieces);
        assert_eq!(area, 400, "100 x 4");
        assert_eq!(perim, 208, "2*(100+4); summing pieces would give 280");
    }

    /// Overlap is counted once, in both area and perimeter.
    #[test]
    fn overlapping_rects_are_counted_once() {
        // Two 10x10 squares overlapping in a 5x10 strip -> 15x10 union.
        let (area, perim) = union_area_perimeter(&[(0, 0, 10, 10), (5, 0, 15, 10)]);
        assert_eq!(area, 150);
        assert_eq!(perim, 50, "2*(15+10)");
    }

    /// Disjoint pieces keep their own boundaries — the union of two islands is two islands.
    #[test]
    fn disjoint_rects_add_their_perimeters() {
        let (area, perim) = union_area_perimeter(&[(0, 0, 10, 10), (100, 100, 110, 110)]);
        assert_eq!(area, 200);
        assert_eq!(perim, 40 + 40);
    }

    /// An L shape: the reflex corner must not lose or gain edge length.
    #[test]
    fn l_shape_perimeter_is_exact() {
        // Vertical 10x30 plus horizontal 30x10 sharing the bottom-left 10x10 corner.
        let (area, perim) = union_area_perimeter(&[(0, 0, 10, 30), (0, 0, 30, 10)]);
        assert_eq!(area, 300 + 300 - 100);
        // Traversing the boundary: 30 up, 10 right, 20 down, 20 right, 10 down, 30 left.
        assert_eq!(perim, 30 + 10 + 20 + 20 + 10 + 30);
    }

    /// A fully enclosed hole keeps its inner boundary — it is real edge, not an artefact.
    #[test]
    fn a_ring_keeps_its_inner_boundary() {
        // 30x30 ring, 10 wide, leaving a 10x10 hole in the middle.
        let (area, perim) = union_area_perimeter(&[
            (0, 0, 30, 10),
            (0, 20, 30, 30),
            (0, 0, 10, 30),
            (20, 0, 30, 30),
        ]);
        assert_eq!(area, 900 - 100);
        assert_eq!(perim, 120 + 40, "outer 4*30 plus the hole's 4*10");
    }

    /// Degenerate and empty inputs measure nothing rather than panicking.
    #[test]
    fn degenerate_rects_measure_nothing() {
        assert_eq!(union_area_perimeter(&[]), (0, 0));
        assert_eq!(union_area_perimeter(&[(5, 5, 5, 10)]), (0, 0), "zero width");
        assert_eq!(union_area_perimeter(&[(5, 5, 10, 5)]), (0, 0), "zero height");
    }

    /// A pin lands on the lowest metal covering it — not on a higher layer merely passing over.
    #[test]
    fn anchor_picks_the_lowest_covering_layer() {
        let g = WireGraph::new(vec![metal(3, 0, 100), metal(1, 0, 100)]);
        let a = g.anchor(50, 5).unwrap();
        assert_eq!(g.shapes[a].layer, 1);
    }

    /// A pin that lands just off the metal still anchors — the router stops at an access
    /// point inside the pin rectangle, not at the centroid odb reports, so requiring
    /// containment leaves nearly every real gate unchecked.
    #[test]
    fn a_pin_just_off_the_metal_anchors_to_the_nearest() {
        let g = WireGraph::new(vec![metal(1, 0, 100)]);
        let a = g.anchor(105, 5).expect("just past the end of the wire");
        assert_eq!(g.shapes[a].layer, 1);
    }

    /// With nothing routed there is nothing to anchor to.
    #[test]
    fn a_net_with_no_routing_has_no_anchor() {
        let g = WireGraph::new(vec![]);
        assert_eq!(g.anchor(0, 0), None);
    }

    /// Nearest wins over lower-layer when the pin is genuinely closer to a higher shape;
    /// containment still takes precedence when it applies.
    #[test]
    fn containment_beats_proximity() {
        // A layer-3 shape covering the point, and a layer-1 shape far away.
        let g = WireGraph::new(vec![metal(3, 0, 100), metal(1, 900, 1000)]);
        let a = g.anchor(50, 5).unwrap();
        assert_eq!(g.shapes[a].layer, 3, "the covering shape wins even though it is higher");
    }
}
