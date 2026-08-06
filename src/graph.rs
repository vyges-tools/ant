// SPDX-License-Identifier: Apache-2.0
//! The conductor graph: what metal each gate is actually exposed to, stage by stage.
//!
//! # The model
//!
//! Transcribed from OpenROAD's `AntennaChecker` after three attempts at inferring it from output
//! comparison failed. The order of operations is the substance:
//!
//! 1. **Vias are decomposed** onto the layers they occupy — a cut, plus an enclosure below and
//!    another above. Not bookkeeping: a net routed at met1 and up has *no li1 wire*, and its only
//!    li1 metal is the enclosure of the li1→met1 via. Standard-cell pins live on li1, so without
//!    decomposition there is nothing there for a pin to touch.
//! 2. **Pin metal is subtracted** from the wire, per layer. Pins are *cut points*: a wire running
//!    past two pins is not one conductor but however many fragments the subtraction leaves. That
//!    is the physical claim — charge collected by a piece of metal reaches the gates on *that*
//!    piece.
//! 3. **Components are labelled per layer**, then linked across one layer step, so a cut layer
//!    carries the vertical connection.
//! 4. **Pins are re-attached** to the fragments their own boxes touch, and short those together.
//!
//! Manufacturing order makes it staged: at stage *L* only layers ≤ *L* exist.
//!
//! # Why this shape and not a simpler one
//!
//! Charging a net's whole per-layer metal to every gate over-reports by the branch structure;
//! charging each gate its own denominator over-reports by the gate count; pooling a net's
//! diffusion lifts limits that should not be lifted. All three were measured against
//! `check_antennas` and all three were wrong. This model reproduced OpenROAD to one decimal on
//! the net that defeated the others — 535.1 against 535.1.

use std::collections::HashMap;
use vyges_opendb::LayerBox;

/// Union-find over conductor indices.
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

/// A coordinate-compressed occupancy grid over one layer.
///
/// Every cell is wholly inside or outside the geometry, so area, perimeter and connectivity are
/// exact integer questions. Subtracting pin metal is just clearing cells — which is what makes
/// the pin cut cheap to do properly rather than approximately.
struct Grid {
    xs: Vec<i32>,
    ys: Vec<i32>,
    nx: usize,
    ny: usize,
    covered: Vec<bool>,
}

impl Grid {
    /// Cells covered by `add`, minus cells covered by `sub`.
    fn build(add: &[(i32, i32, i32, i32)], sub: &[(i32, i32, i32, i32)]) -> Grid {
        let live: Vec<_> = add.iter().copied().filter(|r| r.2 > r.0 && r.3 > r.1).collect();
        if live.is_empty() {
            return Grid { xs: vec![], ys: vec![], nx: 0, ny: 0, covered: vec![] };
        }
        // Subtracted edges join the coordinate set, so a partial overlap cuts exactly rather
        // than being rounded to whole boxes.
        let mut xs: Vec<i32> = Vec::new();
        let mut ys: Vec<i32> = Vec::new();
        for r in live.iter().chain(sub.iter()) {
            xs.push(r.0);
            xs.push(r.2);
            ys.push(r.1);
            ys.push(r.3);
        }
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        let (nx, ny) = (xs.len() - 1, ys.len() - 1);
        let mut g = Grid { xs, ys, nx, ny, covered: vec![false; nx * ny] };
        for r in &live {
            g.paint(*r, true);
        }
        for r in sub {
            g.paint(*r, false);
        }
        g
    }

    fn paint(&mut self, r: (i32, i32, i32, i32), on: bool) {
        let i0 = self.xs.partition_point(|&v| v < r.0);
        let i1 = self.xs.partition_point(|&v| v < r.2);
        let j0 = self.ys.partition_point(|&v| v < r.1);
        let j1 = self.ys.partition_point(|&v| v < r.3);
        for i in i0..i1 {
            for j in j0..j1 {
                self.covered[i * self.ny + j] = on;
            }
        }
    }

    fn cell_size(&self, i: usize, j: usize) -> (i64, i64) {
        ((self.xs[i + 1] - self.xs[i]) as i64, (self.ys[j + 1] - self.ys[j]) as i64)
    }

    /// Total length of this cell's edges facing something outside `member`.
    fn exposed_edges(&self, i: usize, j: usize, member: &dyn Fn(usize) -> bool) -> i64 {
        let (dx, dy) = self.cell_size(i, j);
        let inside = |a: isize, b: isize| -> bool {
            a >= 0
                && b >= 0
                && (a as usize) < self.nx
                && (b as usize) < self.ny
                && member(a as usize * self.ny + b as usize)
        };
        let (i, j) = (i as isize, j as isize);
        let mut p = 0;
        if !inside(i - 1, j) { p += dy; }
        if !inside(i + 1, j) { p += dy; }
        if !inside(i, j - 1) { p += dx; }
        if !inside(i, j + 1) { p += dx; }
        p
    }
}

/// Area and perimeter of the **union** of axis-aligned rectangles, in DBU.
///
/// Summing rectangles individually double-counts: overlap inflates area, and every junction
/// between abutting collinear segments contributes two interior edges no physical wire has.
/// Exact — no floating point, no tolerance.
pub fn union_area_perimeter(rects: &[(i32, i32, i32, i32)]) -> (i64, i64) {
    let g = Grid::build(rects, &[]);
    let (mut area, mut perim) = (0i64, 0i64);
    for i in 0..g.nx {
        for j in 0..g.ny {
            if !g.covered[i * g.ny + j] {
                continue;
            }
            let (dx, dy) = g.cell_size(i, j);
            area += dx * dy;
            perim += g.exposed_edges(i, j, &|k| g.covered[k]);
        }
    }
    (area, perim)
}

/// One connected piece of metal (or cut) on one layer, after pins have cut it.
#[derive(Debug, Clone)]
pub struct Conductor {
    pub layer: i64,
    pub is_routing: bool,
    /// Exact union area, DBU².
    pub area: i64,
    /// Exact union perimeter, DBU.
    pub perimeter: i64,
    /// The cells it occupies, for touch tests against pins and across layers.
    cells: Vec<(i32, i32, i32, i32)>,
}

impl Conductor {
    fn touches_rect(&self, r: (i32, i32, i32, i32), halo: i32) -> bool {
        self.cells.iter().any(|c| {
            c.0 <= r.2 + halo && r.0 - halo <= c.2 && c.1 <= r.3 + halo && r.1 - halo <= c.3
        })
    }
    fn touches(&self, o: &Conductor) -> bool {
        o.cells.iter().any(|c| self.touches_rect(*c, 0))
    }
}

/// Metal collected by one conductor group at one stage, in DBU units.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Collected {
    /// On the stage layer itself — the partial (PAR/PSR) numerator.
    pub layer_area: i64,
    pub layer_perimeter: i64,
    /// On the stage layer and every routing layer below — the cumulative (CAR/CSR) numerator.
    pub cumulative_area: i64,
    pub cumulative_perimeter: i64,
}

/// One net's routing, cut at its pins and labelled into conductors.
pub struct NetGraph {
    conductors: Vec<Conductor>,
    /// Routing layers present, ascending — the stages to evaluate.
    pub layers: Vec<i64>,
    /// Cross-layer contacts: conductor pairs exactly one layer step apart that touch.
    links: Vec<(u32, u32)>,
}

impl NetGraph {
    /// Build from a net's decomposed boxes, cut by every terminal's pin metal.
    pub fn build(boxes: &[LayerBox], pin_boxes: &[LayerBox]) -> NetGraph {
        let mut by_layer: HashMap<i64, (Vec<(i32, i32, i32, i32)>, bool)> = HashMap::new();
        for b in boxes {
            let e = by_layer.entry(b.layer).or_insert((Vec::new(), b.is_routing));
            e.0.push((b.x0, b.y0, b.x1, b.y1));
        }
        let mut pins_by_layer: HashMap<i64, Vec<(i32, i32, i32, i32)>> = HashMap::new();
        for p in pin_boxes {
            pins_by_layer.entry(p.layer).or_default().push((p.x0, p.y0, p.x1, p.y1));
        }

        let mut conductors = Vec::new();
        let empty: Vec<(i32, i32, i32, i32)> = Vec::new();
        for (layer, (rects, is_routing)) in by_layer {
            let subs = pins_by_layer.get(&layer).unwrap_or(&empty);
            let g = Grid::build(&rects, subs);
            for comp in label_components(&g) {
                let member = |k: usize| comp.binary_search(&k).is_ok();
                let (mut area, mut perimeter) = (0i64, 0i64);
                let mut cells = Vec::with_capacity(comp.len());
                for &k in &comp {
                    let (i, j) = (k / g.ny, k % g.ny);
                    let (dx, dy) = g.cell_size(i, j);
                    area += dx * dy;
                    perimeter += g.exposed_edges(i, j, &member);
                    cells.push((g.xs[i], g.ys[j], g.xs[i + 1], g.ys[j + 1]));
                }
                conductors.push(Conductor { layer, is_routing, area, perimeter, cells });
            }
        }

        let mut layers: Vec<i64> =
            conductors.iter().filter(|c| c.is_routing).map(|c| c.layer).collect();
        layers.sort_unstable();
        layers.dedup();

        // Vertical contact. The odb layer stack is consecutive (li1, mcon, met1, via, met2 …),
        // so a routing layer and the cut above it differ by one — that adjacency is the whole
        // reason the graph is three-dimensional rather than a stack of unrelated pictures.
        let mut links = Vec::new();
        for i in 0..conductors.len() {
            for j in i + 1..conductors.len() {
                if (conductors[i].layer - conductors[j].layer).abs() == 1
                    && conductors[i].touches(&conductors[j])
                {
                    links.push((i as u32, j as u32));
                }
            }
        }
        NetGraph { conductors, layers, links }
    }

    pub fn is_empty(&self) -> bool {
        self.conductors.is_empty()
    }

    /// One conductor, by index — for diagnosis of what a terminal attached to.
    pub fn conductor(&self, i: usize) -> &Conductor {
        &self.conductors[i]
    }

    /// Conductors a pin's own metal touches — its own layer, or one step either way.
    ///
    /// A 1-DBU halo so abutment counts, matching OpenROAD's `findNodesWithIntersection`, which
    /// expands the pin polygon before intersecting. **No proximity fallback**: a pin touching
    /// nothing is attached to nothing, because guessing merges conductors that are separate.
    pub fn touched_by(&self, pin_boxes: &[LayerBox]) -> Vec<usize> {
        let mut hits = Vec::new();
        for pb in pin_boxes {
            for (i, c) in self.conductors.iter().enumerate() {
                if (c.layer - pb.layer).abs() <= 1
                    && c.touches_rect((pb.x0, pb.y0, pb.x1, pb.y1), 1)
                    && !hits.contains(&i)
                {
                    hits.push(i);
                }
            }
        }
        hits
    }

    /// Group terminals into the conductors they share at `stage`, with each group's metal.
    ///
    /// `attach[t]` is the conductor set terminal `t` touches. A terminal shorts what it touches,
    /// so two fragments meeting at one pin are one conductor from that stage on.
    pub fn regions_at(&self, stage: i64, attach: &[Vec<usize>]) -> Vec<(Vec<usize>, Collected)> {
        let live = |i: usize| self.conductors[i].layer <= stage;
        let mut uf = UnionFind::new(self.conductors.len());
        for &(a, b) in &self.links {
            if live(a as usize) && live(b as usize) {
                uf.union(a, b);
            }
        }
        for touched in attach {
            let here: Vec<usize> = touched.iter().copied().filter(|&i| live(i)).collect();
            for w in here.windows(2) {
                uf.union(w[0] as u32, w[1] as u32);
            }
        }

        let mut by_root: HashMap<u32, Vec<usize>> = HashMap::new();
        for (t, touched) in attach.iter().enumerate() {
            if let Some(&first) = touched.iter().find(|&&i| live(i)) {
                by_root.entry(uf.find(first as u32)).or_default().push(t);
            }
        }

        let mut metal: HashMap<u32, Collected> = HashMap::new();
        for i in 0..self.conductors.len() {
            let c = &self.conductors[i];
            if !c.is_routing || !live(i) {
                continue; // cut layers carry connection, not antenna metal
            }
            let root = uf.find(i as u32);
            if !by_root.contains_key(&root) {
                continue; // a conductor with no terminal on it collects for nobody
            }
            let e = metal.entry(root).or_default();
            e.cumulative_area += c.area;
            e.cumulative_perimeter += c.perimeter;
            if c.layer == stage {
                e.layer_area += c.area;
                e.layer_perimeter += c.perimeter;
            }
        }

        by_root
            .into_iter()
            .map(|(root, terms)| (terms, metal.get(&root).copied().unwrap_or_default()))
            .collect()
    }
}

/// Connected components of a grid's covered cells, 4-connected.
///
/// Edge-sharing only: a diagonal touch is not an electrical connection.
fn label_components(g: &Grid) -> Vec<Vec<usize>> {
    let mut seen = vec![false; g.nx * g.ny];
    let mut out = Vec::new();
    for start in 0..g.nx * g.ny {
        if !g.covered[start] || seen[start] {
            continue;
        }
        let mut comp = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(k) = stack.pop() {
            comp.push(k);
            let (i, j) = (k / g.ny, k % g.ny);
            let mut visit = |i: usize, j: usize, stack: &mut Vec<usize>, seen: &mut Vec<bool>| {
                let n = i * g.ny + j;
                if g.covered[n] && !seen[n] {
                    seen[n] = true;
                    stack.push(n);
                }
            };
            if i > 0 { visit(i - 1, j, &mut stack, &mut seen); }
            if i + 1 < g.nx { visit(i + 1, j, &mut stack, &mut seen); }
            if j > 0 { visit(i, j - 1, &mut stack, &mut seen); }
            if j + 1 < g.ny { visit(i, j + 1, &mut stack, &mut seen); }
        }
        comp.sort_unstable();
        out.push(comp);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(layer: i64, x0: i32, x1: i32, routing: bool) -> LayerBox {
        LayerBox { layer, x0, y0: 0, x1, y1: 10, is_routing: routing, from_via: false }
    }

    // ---- union geometry ---------------------------------------------------------------

    #[test]
    fn union_of_a_single_rect() {
        assert_eq!(union_area_perimeter(&[(0, 0, 10, 4)]), (40, 28));
    }

    /// A wire delivered as many abutting pieces must measure as the wire, not the pieces.
    #[test]
    fn abutting_run_measures_as_one_wire() {
        let pieces: Vec<_> = (0..10).map(|i| (i * 10, 0, i * 10 + 10, 4)).collect();
        assert_eq!(union_area_perimeter(&pieces), (400, 208), "not 280, which counts the joins");
    }

    #[test]
    fn overlapping_rects_are_counted_once() {
        assert_eq!(union_area_perimeter(&[(0, 0, 10, 10), (5, 0, 15, 10)]), (150, 50));
    }

    #[test]
    fn l_shape_perimeter_is_exact() {
        let (area, perim) = union_area_perimeter(&[(0, 0, 10, 30), (0, 0, 30, 10)]);
        assert_eq!(area, 500);
        assert_eq!(perim, 30 + 10 + 20 + 20 + 10 + 30);
    }

    /// A hole is real boundary, not an artefact.
    #[test]
    fn a_ring_keeps_its_inner_boundary() {
        let (area, perim) = union_area_perimeter(&[
            (0, 0, 30, 10),
            (0, 20, 30, 30),
            (0, 0, 10, 30),
            (20, 0, 30, 30),
        ]);
        assert_eq!(area, 800);
        assert_eq!(perim, 120 + 40);
    }

    #[test]
    fn degenerate_rects_measure_nothing() {
        assert_eq!(union_area_perimeter(&[]), (0, 0));
        assert_eq!(union_area_perimeter(&[(5, 5, 5, 10)]), (0, 0));
    }

    // ---- the pin cut ------------------------------------------------------------------

    /// **The mechanism the whole model rests on.** A pin cuts the metal it sits on, so a wire
    /// running past a terminal is two conductors, not one.
    #[test]
    fn a_pin_cuts_the_wire_it_sits_on() {
        let uncut = NetGraph::build(&[bx(5, 0, 100, true)], &[]);
        assert_eq!(uncut.conductors.len(), 1, "no pin, one conductor");

        let g = NetGraph::build(&[bx(5, 0, 100, true)], &[bx(5, 40, 60, true)]);
        assert_eq!(g.conductors.len(), 2, "the pin splits it in two");
        assert_eq!(g.conductors.iter().map(|c| c.area).sum::<i64>(), 800, "the pin's 200 is gone");
    }

    /// Subtraction is exact, not whole-box: a pin overlapping part of a box removes that part.
    #[test]
    fn a_partial_overlap_cuts_exactly() {
        let g = NetGraph::build(&[bx(5, 0, 100, true)], &[bx(5, 90, 200, true)]);
        assert_eq!(g.conductors.len(), 1);
        assert_eq!(g.conductors[0].area, 900, "90x10 left — not 0, and not 1000");
    }

    // ---- vertical structure -------------------------------------------------------------

    /// Two routing layers are one conductor only through the cut between them — which is why
    /// via decomposition is a prerequisite rather than a refinement.
    #[test]
    fn layers_join_only_through_the_cut_between_them() {
        let boxes = vec![
            bx(3, 0, 20, true),  // li1 — in practice a via enclosure
            bx(4, 5, 15, false), // mcon, the cut
            bx(5, 0, 100, true), // met1
        ];
        let g = NetGraph::build(&boxes, &[]);
        let touched = g.touched_by(&[bx(3, 0, 5, true)]);
        assert!(!touched.is_empty(), "the li1 enclosure is there to be touched");

        let regions = g.regions_at(5, &[touched.clone()]);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].1.cumulative_area, 200 + 1000, "li1 + met1; the cut is not metal");
        assert_eq!(regions[0].1.layer_area, 1000, "met1 alone at the met1 stage");

        // At stage 3 only li1 exists yet.
        assert_eq!(g.regions_at(3, &[touched])[0].1.cumulative_area, 200);
    }

    /// Cut layers connect but never contribute antenna metal.
    #[test]
    fn cut_layers_carry_connection_not_metal() {
        let g = NetGraph::build(&[bx(4, 0, 50, false), bx(5, 0, 50, true)], &[]);
        let touched = g.touched_by(&[bx(5, 0, 10, true)]);
        assert_eq!(g.regions_at(5, &[touched])[0].1.cumulative_area, 500, "the cut's 500 is out");
    }

    // ---- attachment ---------------------------------------------------------------------

    /// A pin touching no metal attaches to nothing — no proximity fallback.
    #[test]
    fn a_pin_touching_nothing_attaches_to_nothing() {
        let g = NetGraph::build(&[bx(5, 0, 100, true)], &[]);
        assert!(g.touched_by(&[bx(5, 500, 520, true)]).is_empty());
    }

    /// A terminal shorts what it touches: the pieces either side of it are one conductor again.
    #[test]
    fn a_terminal_shorts_the_pieces_it_bridges() {
        let pin = bx(5, 40, 60, true);
        let g = NetGraph::build(&[bx(5, 0, 100, true)], &[pin]);
        assert_eq!(g.conductors.len(), 2);
        let touched = g.touched_by(&[pin]);
        assert_eq!(touched.len(), 2, "abuts both pieces");
        let regions = g.regions_at(5, &[touched]);
        assert_eq!(regions.len(), 1, "the terminal rejoins them");
        assert_eq!(regions[0].1.layer_area, 800);
    }

    /// **The case that defeated every earlier model.** Two terminals on a wire cut between them
    /// stay on separate conductors, so neither inherits the other's gate area or diffusion.
    #[test]
    fn terminals_separated_by_a_cut_stay_separate() {
        // A wire cut into three by two pins; a third pin sits on each end piece only.
        let cut_a = bx(5, 30, 40, true);
        let cut_b = bx(5, 60, 70, true);
        let g = NetGraph::build(&[bx(5, 0, 100, true)], &[cut_a, cut_b]);
        assert_eq!(g.conductors.len(), 3, "left, middle, right");

        // Terminals wholly inside the left and right pieces touch one conductor each.
        let left = g.touched_by(&[bx(5, 5, 10, true)]);
        let right = g.touched_by(&[bx(5, 85, 90, true)]);
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_ne!(left[0], right[0], "different conductors");

        let regions = g.regions_at(5, &[left, right]);
        assert_eq!(regions.len(), 2, "two conductors, two verdicts");
        for (terms, c) in &regions {
            assert_eq!(terms.len(), 1, "neither terminal pools with the other");
            assert_eq!(c.layer_area, 300, "each sees only its own 30x10 piece");
        }
    }
}
