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

    /// What the gate anchored at `anchor` has collected at each stage.
    ///
    /// Returns one entry per routing layer in [`WireGraph::layers`], ascending.
    pub fn collected_by_stage(&self, anchor: usize) -> Vec<(i64, Collected)> {
        self.layers
            .iter()
            .map(|&stage| {
                let mut uf = self.components_at(stage);
                let root = uf.find(anchor as u32);
                let mut c = Collected::default();
                for (i, s) in self.shapes.iter().enumerate() {
                    if s.is_via || s.layer < 0 || s.layer > stage {
                        continue;
                    }
                    if uf.find(i as u32) != root {
                        continue; // a different conductor at this stage
                    }
                    let (dx, dy) = ((s.x1 - s.x0) as i64, (s.y1 - s.y0) as i64);
                    if dx <= 0 || dy <= 0 {
                        continue;
                    }
                    let (area, perim) = (dx * dy, 2 * (dx + dy));
                    c.cumulative_area += area;
                    c.cumulative_perimeter += perim;
                    if s.layer == stage {
                        c.layer_area += area;
                        c.layer_perimeter += perim;
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
    #[test]
    fn abutting_shapes_are_one_conductor() {
        let g = WireGraph::new(vec![metal(1, 0, 50), metal(1, 50, 100)]);
        let a = g.anchor(10, 5).unwrap();
        assert_eq!(g.collected_by_stage(a)[0].1.layer_area, 500 + 500);
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
