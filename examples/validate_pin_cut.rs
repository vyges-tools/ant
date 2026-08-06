// SPDX-License-Identifier: Apache-2.0
//! Throwaway validation of one hypothesis, before rewriting the graph on the strength of it.
//!
//! **The question.** OpenROAD subtracts every terminal's pin metal from the wire polygons before
//! building nodes (`avoidPinIntersection`), which makes pins *cut points* in the metal. Does that
//! subtraction actually separate `_0711_/A` from its protection diode on `tl_cpu_h2d[77]` met1 —
//! the net where we currently pass at 371.6 and OpenROAD violates at 535.1?
//!
//! **The prediction, if the hypothesis holds.** The gate's fragment carries gate area 0.99 (its
//! own, not 1.4247 pooled with the diode's) and diffusion 0.0 (not 0.4347), so the limit is the
//! curve at zero diffusion — 400 — and the ratio lands near 535.
//!
//! Run: `cargo run --release --example validate_pin_cut -- <design.odb> <net> <layer>`

use std::collections::HashMap;
use vyges_opendb::{Db, WireShape};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (odb, net, want_layer) = match args.as_slice() {
        [a, b, c] => (a.clone(), b.clone(), c.clone()),
        _ => {
            eprintln!("usage: validate_pin_cut <design.odb> <net> <layer>");
            std::process::exit(2);
        }
    };
    let db = Db::open(&odb).expect("open odb");
    let dbu = db.dbu_per_micron() as f64;

    // Resolve the layer by name, and pull the net's wire rectangles on it.
    let shapes = db.net_wire_shapes(&net);
    let layer_num = shapes
        .iter()
        .map(|s| if s.is_via { s.via_top } else { s.layer })
        .filter(|&n| n >= 0)
        .find(|&n| db.layer_name_by_number(n) == want_layer)
        .unwrap_or_else(|| panic!("net has nothing on {want_layer}"));
    let wires: Vec<WireShape> =
        shapes.iter().copied().filter(|s| !s.is_via && s.layer == layer_num).collect();

    // Every terminal's pin metal on that layer, plus its antenna areas.
    struct Term {
        name: String,
        gate: f64,
        diff: f64,
        boxes: Vec<WireShape>,
    }
    let terms: Vec<Term> = db
        .net_iterms(&net)
        .into_iter()
        .filter_map(|it| {
            let (inst, pin) = it.rsplit_once('/')?;
            let master = db.inst_master(inst);
            Some(Term {
                gate: db.mterm_antenna_gate_area(&master, pin),
                diff: db.mterm_antenna_diff_area(&master, pin),
                boxes: db
                    .iterm_pin_boxes(inst, pin)
                    .into_iter()
                    .filter(|b| b.layer == layer_num)
                    .collect(),
                name: it.clone(),
            })
        })
        .collect();

    println!("net {net} on {want_layer}: {} wire rects, {} terminals", wires.len(), terms.len());

    // --- the grid: wire cells, minus pin cells -------------------------------------------
    let mut xs: Vec<i32> = Vec::new();
    let mut ys: Vec<i32> = Vec::new();
    for s in wires.iter().chain(terms.iter().flat_map(|t| t.boxes.iter())) {
        xs.push(s.x0);
        xs.push(s.x1);
        ys.push(s.y0);
        ys.push(s.y1);
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let (nx, ny) = (xs.len().saturating_sub(1), ys.len().saturating_sub(1));
    if nx == 0 || ny == 0 {
        println!("no geometry");
        return;
    }
    let idx = |i: usize, j: usize| i * ny + j;
    let mut cell = vec![false; nx * ny];
    let mut mark = |s: &WireShape, on: bool, cell: &mut Vec<bool>| {
        let i0 = xs.partition_point(|&v| v < s.x0);
        let i1 = xs.partition_point(|&v| v < s.x1);
        let j0 = ys.partition_point(|&v| v < s.y0);
        let j1 = ys.partition_point(|&v| v < s.y1);
        for i in i0..i1 {
            for j in j0..j1 {
                cell[idx(i, j)] = on;
            }
        }
    };
    for w in &wires {
        mark(w, true, &mut cell);
    }
    let covered_before = cell.iter().filter(|&&c| c).count();
    // THE HYPOTHESIS: pin metal is removed from the wire, cutting it.
    for t in &terms {
        for b in &t.boxes {
            mark(b, false, &mut cell);
        }
    }
    println!(
        "cells covered by wire: {covered_before} -> {} after subtracting pin metal",
        cell.iter().filter(|&&c| c).count()
    );

    // --- connected components over surviving cells ---------------------------------------
    let mut comp = vec![usize::MAX; nx * ny];
    let mut ncomp = 0usize;
    for start in 0..nx * ny {
        if !cell[start] || comp[start] != usize::MAX {
            continue;
        }
        let mut stack = vec![start];
        comp[start] = ncomp;
        while let Some(c) = stack.pop() {
            let (i, j) = (c / ny, c % ny);
            // 4-connectivity: cells sharing an edge are one conductor; a diagonal touch is not.
            let mut push = |i: usize, j: usize, stack: &mut Vec<usize>, comp: &mut Vec<usize>| {
                let k = idx(i, j);
                if cell[k] && comp[k] == usize::MAX {
                    comp[k] = ncomp;
                    stack.push(k);
                }
            };
            if i > 0 { push(i - 1, j, &mut stack, &mut comp); }
            if i + 1 < nx { push(i + 1, j, &mut stack, &mut comp); }
            if j > 0 { push(i, j - 1, &mut stack, &mut comp); }
            if j + 1 < ny { push(i, j + 1, &mut stack, &mut comp); }
        }
        ncomp += 1;
    }
    println!("fragments after the cut: {ncomp}");

    // --- which fragment does each terminal touch? ----------------------------------------
    // A pin's own footprint is now empty, so it attaches to what abuts it (1 DBU halo, the
    // same tolerance OpenROAD's findNodesWithIntersection uses).
    let mut per_comp: HashMap<usize, (f64, f64, Vec<String>)> = HashMap::new();
    for t in &terms {
        let mut hits: Vec<usize> = Vec::new();
        for b in &t.boxes {
            for i in 0..nx {
                for j in 0..ny {
                    let k = idx(i, j);
                    if !cell[k] {
                        continue;
                    }
                    let touch = xs[i] <= b.x1 + 1
                        && b.x0 - 1 <= xs[i + 1]
                        && ys[j] <= b.y1 + 1
                        && b.y0 - 1 <= ys[j + 1];
                    if touch && !hits.contains(&comp[k]) {
                        hits.push(comp[k]);
                    }
                }
            }
        }
        for &h in &hits {
            let e = per_comp.entry(h).or_insert((0.0, 0.0, Vec::new()));
            e.0 += t.gate;
            e.1 += t.diff;
            e.2.push(t.name.clone());
        }
        println!("  {:46} gate={:.4} diff={:.4} -> fragments {hits:?}", t.name, t.gate, t.diff);
    }

    // --- area / perimeter per fragment, and the resulting ratio ---------------------------
    let thick = db.layer_thickness(&want_layer) as f64;
    let dbu2 = dbu * dbu;
    println!("\nfragment  gate      diff      side_area   psr        pins");
    let mut ids: Vec<usize> = per_comp.keys().copied().collect();
    ids.sort();
    for c in ids {
        let (gate, diff, pins) = &per_comp[&c];
        let mut perim = 0i64;
        for i in 0..nx {
            for j in 0..ny {
                if comp[idx(i, j)] != c {
                    continue;
                }
                let (dx, dy) = ((xs[i + 1] - xs[i]) as i64, (ys[j + 1] - ys[j]) as i64);
                let cov = |a: isize, b: isize| -> bool {
                    a >= 0
                        && b >= 0
                        && (a as usize) < nx
                        && (b as usize) < ny
                        && comp[idx(a as usize, b as usize)] == c
                };
                let (i, j) = (i as isize, j as isize);
                if !cov(i - 1, j) { perim += dy; }
                if !cov(i + 1, j) { perim += dy; }
                if !cov(i, j - 1) { perim += dx; }
                if !cov(i, j + 1) { perim += dx; }
            }
        }
        let side = (perim as f64 * thick) / dbu2;
        let psr = if *gate > 0.0 { side / gate } else { 0.0 };
        println!("{c:8}  {gate:<9.4} {diff:<9.4} {side:<11.2} {psr:<10.1} {pins:?}");
    }
}
