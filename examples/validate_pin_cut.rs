// SPDX-License-Identifier: Apache-2.0
//! Prototype of the OpenROAD-shaped conductor graph, on one net, before rewriting `graph.rs`.
//!
//! Builds what `AntennaChecker` builds: decomposed via geometry on every layer, pin metal
//! subtracted, per-layer connected components, cross-layer links through the cut layers, and
//! pins attached to what their own boxes touch.
//!
//! **The question.** On `tl_cpu_h2d[77]` at stage met1, does `_0711_/A` end up on a conductor
//! *without* its protection diode — gate area 0.99 and diffusion 0.0 rather than 1.4247 and
//! 0.4347 — giving a ratio near OpenROAD's 535.1 against a limit of 400?
//!
//! Run: `cargo run --release --example validate_pin_cut -- <design.odb> <net> <stage-layer>`

use std::collections::BTreeMap;
use vyges_opendb::{Db, LayerBox};

/// Union-find over box indices.
struct Uf(Vec<u32>);
impl Uf {
    fn new(n: usize) -> Self {
        Uf((0..n as u32).collect())
    }
    fn find(&mut self, mut i: u32) -> u32 {
        while self.0[i as usize] != i {
            self.0[i as usize] = self.0[self.0[i as usize] as usize];
            i = self.0[i as usize];
        }
        i
    }
    fn union(&mut self, a: u32, b: u32) {
        let (x, y) = (self.find(a), self.find(b));
        if x != y {
            self.0[x as usize] = y;
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (odb, net, stage_name) = (a[0].clone(), a[1].clone(), a[2].clone());
    let db = Db::open(&odb).expect("open odb");
    let dbu = db.dbu_per_micron() as f64;

    let boxes = db.net_wire_boxes(&net);
    let stage = boxes
        .iter()
        .map(|b| b.layer)
        .find(|&n| db.layer_name_by_number(n) == stage_name)
        .expect("stage layer not on this net");

    // Terminals, with their own pin metal.
    struct Term {
        name: String,
        gate: f64,
        diff: f64,
        boxes: Vec<LayerBox>,
    }
    let terms: Vec<Term> = db
        .net_iterms(&net)
        .into_iter()
        .filter_map(|it| {
            let (inst, pin) = it.rsplit_once('/')?;
            let m = db.inst_master(inst);
            Some(Term {
                gate: db.mterm_antenna_gate_area(&m, pin),
                diff: db.mterm_antenna_diff_area(&m, pin),
                // Pin boxes come back as WireShape; the only fields used here are layer + rect.
                boxes: db
                    .iterm_pin_boxes(inst, pin)
                    .into_iter()
                    .map(|w| LayerBox {
                        layer: w.layer,
                        x0: w.x0,
                        y0: w.y0,
                        x1: w.x1,
                        y1: w.y1,
                        is_routing: true,
                        from_via: false,
                    })
                    .collect(),
                name: it.clone(),
            })
        })
        .collect();

    // Everything that exists by this stage — cut layers included, since a via's cut is how a
    // conductor reaches the layer above.
    let live: Vec<(usize, LayerBox)> =
        boxes.iter().copied().enumerate().filter(|(_, b)| b.layer <= stage).collect();
    println!("net {net}, stage {stage_name}: {} boxes live", live.len());

    // Conductors: same-layer contact, plus contact across ONE layer step. The odb layer stack is
    // consecutive (li1, mcon, met1, via, met2 …), so a routing layer and the cut above it differ
    // by one — that adjacency is what makes the graph three-dimensional.
    let mut uf = Uf::new(live.len());
    for i in 0..live.len() {
        for j in i + 1..live.len() {
            let (a, b) = (&live[i].1, &live[j].1);
            if (a.layer - b.layer).abs() <= 1 && a.touches(b) {
                uf.union(i as u32, j as u32);
            }
        }
    }

    // Pins CUT the metal: OpenROAD subtracts pin polygons before building nodes. Approximated
    // here by dropping boxes wholly covered by a pin box — enough to see whether the cut is what
    // separates the gate from its diode.
    let covered_by_pin: Vec<bool> = live
        .iter()
        .map(|(_, b)| {
            terms.iter().flat_map(|t| t.boxes.iter()).any(|p| {
                p.layer == b.layer && p.x0 <= b.x0 && p.y0 <= b.y0 && b.x1 <= p.x1 && b.y1 <= p.y1
            })
        })
        .collect();
    println!("boxes wholly inside a pin (would be cut away): {}", covered_by_pin.iter().filter(|&&c| c).count());

    // Attach each terminal to the conductors its own boxes touch, and short them.
    let mut per_root: BTreeMap<u32, (f64, f64, Vec<String>)> = BTreeMap::new();
    for t in &terms {
        let mut hits: Vec<usize> = Vec::new();
        for pb in &t.boxes {
            for (k, (_, b)) in live.iter().enumerate() {
                if (pb.layer - b.layer).abs() <= 1 && pb.touches(b) && !hits.contains(&k) {
                    hits.push(k);
                }
            }
        }
        for w in hits.windows(2) {
            uf.union(w[0] as u32, w[1] as u32);
        }
        if let Some(&first) = hits.first() {
            let r = uf.find(first as u32);
            let e = per_root.entry(r).or_insert((0.0, 0.0, Vec::new()));
            e.0 += t.gate;
            e.1 += t.diff;
            e.2.push(t.name.clone());
        }
        println!("  {:44} gate={:.4} diff={:.4} touches {} boxes", t.name, t.gate, t.diff, hits.len());
    }

    // Per conductor: metal on the stage layer only (the PSR numerator).
    let thick = db.layer_thickness(&stage_name) as f64;
    let dbu2 = dbu * dbu;
    println!("\nconductor  gate      diff      side_area   psr        pins");
    let roots: Vec<u32> = per_root.keys().copied().collect();
    for r in roots {
        let rects: Vec<(i32, i32, i32, i32)> = live
            .iter()
            .enumerate()
            .filter(|(k, (_, b))| {
                b.layer == stage && b.is_routing && uf.find(*k as u32) == r && !covered_by_pin[*k]
            })
            .map(|(_, (_, b))| (b.x0, b.y0, b.x1, b.y1))
            .collect();
        let (_, perim) = vyges_ant::graph::union_area_perimeter(&rects);
        let side = (perim as f64 * thick) / dbu2;
        let (gate, diff, pins) = &per_root[&r];
        let psr = if *gate > 0.0 { side / gate } else { 0.0 };
        println!("{r:9}  {gate:<9.4} {diff:<9.4} {side:<11.2} {psr:<10.1} {pins:?}");
    }
}
