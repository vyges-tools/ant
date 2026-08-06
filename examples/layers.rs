// SPDX-License-Identifier: Apache-2.0
//! Does via decomposition produce geometry on the layer the pins live on?
//! Run: `cargo run --release --example layers -- <design.odb> <net>`
use std::collections::BTreeMap;
use vyges_opendb::Db;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let db = Db::open(&a[0]).expect("open");
    let net = &a[1];

    let mut before: BTreeMap<String, usize> = BTreeMap::new();
    for s in db.net_wire_shapes(net).iter().filter(|s| !s.is_via) {
        *before.entry(db.layer_name_by_number(s.layer)).or_default() += 1;
    }
    println!("wire-only geometry (net_wire_shapes): {before:?}");

    let mut after: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for b in db.net_wire_boxes(net) {
        let e = after.entry(format!("{}{}", db.layer_name_by_number(b.layer),
                                    if b.is_routing { "" } else { " (cut)" })).or_default();
        e.0 += 1;
        if b.from_via { e.1 += 1; }
    }
    println!("decomposed (net_wire_boxes)  [total, of which from vias]:");
    for (l, (n, v)) in &after {
        println!("   {l:14} {n:5}  {v:5}");
    }

    let mut pin_layers: BTreeMap<String, usize> = BTreeMap::new();
    for it in db.net_iterms(net) {
        if let Some((i, p)) = it.rsplit_once('/') {
            for b in db.iterm_pin_boxes(i, p) {
                *pin_layers.entry(db.layer_name_by_number(b.layer)).or_default() += 1;
            }
        }
    }
    println!("pin boxes live on: {pin_layers:?}");
    for l in pin_layers.keys() {
        let has = after.keys().any(|k| k.split(' ').next() == Some(l.as_str()));
        println!("   {l:8} has net geometry after decomposition: {has}");
    }
}
