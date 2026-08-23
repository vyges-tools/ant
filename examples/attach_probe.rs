// SPDX-License-Identifier: Apache-2.0
//! Which conductors does each terminal attach to, and on what layers?
//! Run: `cargo run --release --example attach_probe -- <odb> <net>`
use vyges_ant::graph::NetGraph;
use vyges_opendb::{Db, LayerBox};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let db = Db::open(&a[0]).expect("open");
    let net = &a[1];
    let boxes = db.net_wire_boxes(net);

    let terms: Vec<(String, Vec<LayerBox>)> = db
        .net_iterms(net)
        .into_iter()
        .filter_map(|it| {
            let (i, p) = it.rsplit_once('/')?;
            Some((
                it.clone(),
                db.iterm_pin_boxes(i, p)
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
            ))
        })
        .collect();
    let pins: Vec<LayerBox> = terms.iter().flat_map(|t| t.1.iter().copied()).collect();
    let g = NetGraph::build(&boxes, &pins);

    for (name, pb) in &terms {
        let pl: Vec<String> = pb
            .iter()
            .map(|b| db.layer_name_by_number(b.layer))
            .collect();
        let hits = g.touched_by(pb);
        let desc: Vec<String> = hits
            .iter()
            .map(|&i| {
                let c = g.conductor(i);
                format!("{}#{} a={}", db.layer_name_by_number(c.layer), i, c.area)
            })
            .collect();
        println!("{name:44} pins on {pl:?} -> {desc:?}");
    }
}
