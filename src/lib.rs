// SPDX-License-Identifier: Apache-2.0
//! `vyges-ant` — antenna ratio sign-off over the **routed** OpenDB database.
//!
//! During manufacture a metal shape connected to a gate but not yet to a diffusion path acts as
//! a charge collector; if it is large relative to the gate it damages the oxide. The check is a
//! ratio — collected metal over gate area — evaluated against limits the LEF states per layer.
//!
//! # Why the routed database, and not a GDS
//!
//! `vyges-drc` already computes an antenna ratio, over GDS polygons, post-stream. This engine
//! computes it over `dbWire` routing, which is where OpenROAD's `ant` computes it and — the
//! point — the stage at which a violation can still be repaired by inserting a diode. A GDS
//! answer arrives after the last chance to act on it. Same ratio, different substrate, different
//! job; neither replaces the other.
//!
//! # The model
//!
//! For each net, walking routing layers bottom-up:
//!
//! - **PAR** (partial area ratio) — metal on *this* layer / gate area
//! - **CAR** (cumulative area ratio) — metal on this layer *and every layer below* / gate area
//! - **PSR** / **CSR** — the same two over *side* area (perimeter × layer thickness)
//!
//! Cumulative matters: a net legal on each layer taken alone can still violate CAR, because the
//! charge a gate sees is what the whole connected stack collected, not the worst single layer.
//!
//! # Maturity — measured against OpenROAD, not claimed
//!
//! Correlated against `check_antennas` on a routed sky130 block (~2500 nets, same `.odb`,
//! 2026-08-05): **61 of 73 violating nets found (84%), with ~2400 added that OpenROAD does not
//! confirm.** A screen, not a sign-off gate. Do not gate a tapeout on it.
//!
//! # The metal-attribution gap (why the false positives)
//!
//! At the stage where layer *L* is deposited, the metal connected to a gate is the routing
//! sub-graph reachable from that gate over layers ≤ *L*. Two gates on one net can sit on
//! different branches and collect very different metal until a higher layer joins them.
//!
//! This engine sums the net's metal per layer and charges all of it to every gate. Measured on
//! one net: 5685.6 for every pin where OpenROAD gives 720.2 for four and 2786.0 for the fifth.
//! Closing it needs a routing-graph walk from each gate pin rather than a per-layer sum. The
//! diffusion area is likewise applied net-wide, where the real limit moves per layer as the
//! path to diffusion completes (the same pin requiring 400.00 on met1 and 3119.36 on met2).
//!
//! # Other stated bounds
//!
//! 1. **Metal area double-counts overlap.** Shapes are summed as raw rectangles, not unioned
//!    (see `vyges-opendb`'s `net_wire_area_on_layer`) — perimeter worse than area, since
//!    interior junction edges count too.
//! 2. **Layer order is routing level.** Ordering is by `dbTechLayer` routing level rather than
//!    by a manufacturing step model, which is the standard approximation for CAR.
//! 3. **Cut layers are not checked** — routing layers only.
//!
//! # Two forms of limit, and why both are needed
//!
//! LEF states antenna limits two ways, and a checker that reads only one finds nothing on
//! technologies that use the other:
//!
//! - **Plain ratios** (`ANTENNAAREARATIO` …) — a constant per layer.
//! - **Diffusion-dependent PWL ratios** (`ANTENNADIFFAREARATIO` …) — the limit as a
//!   piecewise-linear function of the diffusion area connected to the net. More diffusion
//!   permits a higher ratio, which is precisely how a protection diode earns relief.
//!
//! Both are implemented. Where a technology states a diff curve, it wins — that is the limit
//! the foundry characterised for a net carrying that much diffusion; the plain ratio is the
//! fallback.
//!
//! **This matters more than it sounds.** Measured on sky130 (2026-08-05, not assumed): every
//! routing layer carries a `dbTechLayerAntennaRule`, yet `isValid()` is false on all of them —
//! and that predicate is exactly "does any *plain* ratio exceed zero":
//!
//! ```text
//! return par_area_val_.ratio_ > 0 || cum_area_val_.ratio_ > 0
//!     || par_sidearea_val_.ratio_ > 0 || cum_sidearea_val_.ratio_ > 0;
//! ```
//!
//! sky130 states exactly one antenna limit, `DiffPSR`, as a 4-point curve identical on met1–3:
//! `(0, 400) (0.0125, 400) (0.0225, 2609) (22.5, 11600)`. A plain-ratios-only checker sees no
//! limits there at all — which is what this engine reported before the curves were bridged.
//!
//! # When a verdict is not a verdict
//!
//! Two situations are deliberately not reported as clean, because nothing was checked: a
//! technology stating no antenna limit in *either* form ([`Report::no_rules_found`]), and a net
//! with no gate area ([`Report::nets_no_gate`]) — with no denominator there is no ratio.

use serde::Serialize;
use vyges_opendb::{DiffCurve, Db};

/// Which ratio a violation is against. The four are checked independently: a net can pass PAR
/// and fail CAR, which is the whole reason the cumulative form exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ratio {
    /// Partial area ratio — this layer alone.
    Par,
    /// Cumulative area ratio — this layer and everything below it.
    Car,
    /// Partial side-area ratio.
    Psr,
    /// Cumulative side-area ratio.
    Csr,
}

impl Ratio {
    pub fn as_str(self) -> &'static str {
        match self {
            Ratio::Par => "PAR",
            Ratio::Car => "CAR",
            Ratio::Psr => "PSR",
            Ratio::Csr => "CSR",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub net: String,
    /// The gate pin the ratio was charged against (`inst/pin`). The verdict is per gate, not
    /// per net, so a net can appear more than once.
    pub pin: String,
    pub layer: String,
    pub ratio: Ratio,
    /// The computed ratio (dimensionless: µm² / µm²).
    pub value: f64,
    /// The LEF limit it exceeded.
    pub limit: f64,
    /// Gate area of THIS pin, µm² — carried so a report reads as a diagnosis rather than a
    /// number: a tiny denominator and a huge numerator are very different problems.
    pub gate_area_um2: f64,
    /// Diffusion area on the net, µm². It selects the limit on a diff-ratio curve, so a report
    /// that omitted it would show a limit no one could reproduce from the LEF.
    pub diff_area_um2: f64,
    /// Metal area charged against that limit, µm² (partial or cumulative to match `ratio`).
    pub metal_area_um2: f64,
}

/// Per-layer metal on one net, in µm², ordered bottom-up by routing level.
#[derive(Debug, Clone, Serialize)]
pub struct LayerMetal {
    pub layer: String,
    pub routing_level: i32,
    pub area_um2: f64,
    /// Perimeter × layer thickness. Zero when the LEF states no thickness — in which case the
    /// side-area ratios are *unavailable*, not zero, and are skipped rather than passed.
    pub side_area_um2: f64,
}

/// One gate pin on a net, with its own gate area.
///
/// The ratio is evaluated **per gate pin**, never against the net's total. The antenna
/// protects an individual transistor gate: the charge collected by the metal reaches every
/// gate on the net, so each is exposed to all of it. Summing the gate areas of a net's pins
/// would inflate the denominator by the pin count and silently under-report — measured against
/// OpenROAD's `check_antennas`, that error hid 68 of 73 violating nets on a real block.
#[derive(Debug, Clone, Serialize)]
pub struct PinGate {
    pub pin: String,
    pub gate_area_um2: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetAntenna {
    pub net: String,
    /// Every pin on the net that carries a gate area — each evaluated separately.
    pub gates: Vec<PinGate>,
    /// Diffusion area on the net (µm²) — the index into the diff-ratio curves. More diffusion
    /// means a higher permitted ratio, which is exactly how a protection diode earns relief.
    pub diff_area_um2: f64,
    pub layers: Vec<LayerMetal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `clean` when no violation was found, `violations` otherwise.
    pub status: &'static str,
    pub count: usize,
    /// Nets that had both routed metal and a gate — the ones actually evaluated.
    pub nets_checked: usize,
    /// Nets with routed metal but no gate area on any connected pin. Not applicable rather
    /// than passing: with no denominator there is no ratio. A large number here means the
    /// library's antenna models are missing, and the check is quietly covering less than it
    /// appears to — which is why it is reported, not swallowed.
    pub nets_no_gate: usize,
    /// Nets with no routed metal (unrouted, or fully abstracted away).
    pub nets_unrouted: usize,
    /// Layers whose LEF states no antenna rule at all. Metal there is accumulated into CAR
    /// but never itself compared, because there is no limit to compare it to.
    pub layers_without_rules: Vec<String>,
    /// True when no layer in the design carried a usable antenna rule — the verdict is then
    /// vacuous and must not be read as "clean".
    pub no_rules_found: bool,
    pub violations: Vec<Violation>,
}

/// Read one net's antenna inputs, converted to µm².
///
/// Returns `None` for a net with no routed metal — not an error; an unrouted net collects
/// nothing. The DBU→µm conversion happens here, once, because the LEF states gate area in µm²
/// while the database states geometry in DBU: comparing them raw is a ~10⁶ error on sky130.
pub fn read_net(db: &Db, net: &str, dbu: f64) -> Option<NetAntenna> {
    let n_layers = db.num_net_wire_layers(net);
    if n_layers == 0 {
        return None;
    }
    let dbu2 = dbu * dbu;

    let mut layers: Vec<LayerMetal> = (0..n_layers)
        .map(|i| db.nth_net_wire_layer(net, i))
        .filter(|l| !l.is_empty())
        .map(|layer| {
            let area = db.net_wire_area_on_layer(net, &layer) as f64 / dbu2;
            let perim = db.net_wire_perimeter_on_layer(net, &layer) as f64;
            let thick = db.layer_thickness(&layer) as f64;
            LayerMetal {
                routing_level: db.layer_get_routing_level(&layer),
                // Thickness of 0 means the LEF stated none; the product is then 0, which
                // `check_net` treats as "unavailable" and skips rather than passing.
                side_area_um2: (perim * thick) / dbu2,
                area_um2: area,
                layer,
            }
        })
        .collect();
    // CAR/CSR are only meaningful bottom-up: the charge a gate sees accumulated in
    // manufacturing order, so the accumulation must follow routing level, not name order.
    layers.sort_by_key(|l| l.routing_level);

    // The denominator: gate area of every pin on the net that has an antenna model. Output
    // pins have no gate, so they contribute nothing and are naturally excluded by the model
    // lookup returning 0.0.
    // Gate areas stay PER PIN (each is its own denominator); diffusion is summed over the net,
    // because the charge drains through whatever diffusion the net reaches.
    let mut gates: Vec<PinGate> = Vec::new();
    let mut diff_area_um2 = 0.0f64;
    for iterm in db.net_iterms(net) {
        // iterms are "inst/pin"; hierarchical instance names contain slashes, so split from
        // the RIGHT — splitting at the first slash silently picks the wrong instance.
        let Some((inst, pin)) = iterm.rsplit_once('/') else { continue };
        let master = db.inst_master(inst);
        if master.is_empty() {
            continue;
        }
        let gate = db.mterm_antenna_gate_area(&master, pin);
        if gate > 0.0 {
            gates.push(PinGate { pin: iterm.clone(), gate_area_um2: gate });
        }
        diff_area_um2 += db.mterm_antenna_diff_area(&master, pin);
    }

    Some(NetAntenna { net: net.to_string(), gates, diff_area_um2, layers })
}

/// A piecewise-linear antenna limit: `(diffusion area µm², ratio limit)` points, ascending.
///
/// odb's conventions, which this preserves: **no points** means the limit is unset; **one
/// point** is a single constant ratio rather than a curve.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pwl(pub Vec<(f64, f64)>);

impl Pwl {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The limit at diffusion area `x`, or `None` when the curve is unset.
    ///
    /// Outside the stated range the curve is **clamped**, not extrapolated: a LEF table states
    /// the limit over the diffusion areas the foundry characterised, and inventing values past
    /// either end would be manufacturing an answer the technology never gave.
    pub fn eval(&self, x: f64) -> Option<f64> {
        let p = &self.0;
        match p.len() {
            0 => None,
            1 => Some(p[0].1), // odb: a single point is a constant, not a curve
            _ => {
                if x <= p[0].0 {
                    return Some(p[0].1);
                }
                if x >= p[p.len() - 1].0 {
                    return Some(p[p.len() - 1].1);
                }
                let i = p.windows(2).position(|w| x >= w[0].0 && x <= w[1].0)?;
                let ((x0, y0), (x1, y1)) = (p[i], p[i + 1]);
                let span = x1 - x0;
                // Coincident indices would divide by zero; take the left value, which is what
                // a step at that point means.
                Some(if span == 0.0 { y0 } else { y0 + (y1 - y0) * (x - x0) / span })
            }
        }
    }
}

/// One layer's antenna limits, in both the forms LEF can state them.
///
/// A plain limit of `0.0` means the LEF declares none for that ratio — **not** a limit of zero,
/// which would fail every net carrying any metal at all. That distinction is the difference
/// between a checker and a nuisance, so it lives in one place: [`LayerRules::exceeds`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerRules {
    /// Whether the layer states any *plain* ratio (odb's `isValid`, which tests exactly that).
    pub valid: bool,
    pub par: f64,
    pub car: f64,
    pub psr: f64,
    pub csr: f64,
    /// Diffusion-dependent forms. On sky130 these are the only limits stated.
    pub diff_par: Pwl,
    pub diff_car: Pwl,
    pub diff_psr: Pwl,
    pub diff_csr: Pwl,
}

impl LayerRules {
    /// `Some(ratio_value)` when `area / gate` exceeds `limit`; `None` when it passes, when the
    /// limit is undeclared, or when the gate area is not positive.
    pub fn exceeds(limit: f64, area_um2: f64, gate_area_um2: f64) -> Option<f64> {
        if limit <= 0.0 || gate_area_um2 <= 0.0 {
            return None;
        }
        let value = area_um2 / gate_area_um2;
        (value > limit).then_some(value)
    }

    /// The limit for one ratio at a given diffusion area.
    ///
    /// The diffusion-dependent curve **wins when the technology states one**, because that is
    /// the limit the foundry characterised for a net carrying that much diffusion; the plain
    /// ratio is the fallback for technologies that state only it. Returns 0.0 when neither
    /// exists, which [`LayerRules::exceeds`] reads as "no limit declared".
    pub fn limit(&self, ratio: Ratio, diff_area_um2: f64) -> f64 {
        let (pwl, plain) = match ratio {
            Ratio::Par => (&self.diff_par, self.par),
            Ratio::Car => (&self.diff_car, self.car),
            Ratio::Psr => (&self.diff_psr, self.psr),
            Ratio::Csr => (&self.diff_csr, self.csr),
        };
        pwl.eval(diff_area_um2).unwrap_or(plain)
    }

    /// Whether this layer states any usable antenna limit, in either form. Used to tell a
    /// genuinely clean design from one the technology gave us nothing to check.
    pub fn has_any_limit(&self) -> bool {
        self.valid
            || !self.diff_par.is_empty()
            || !self.diff_car.is_empty()
            || !self.diff_psr.is_empty()
            || !self.diff_csr.is_empty()
    }
}

/// Read one layer's antenna limits from the technology, in both forms.
pub fn read_layer_rules(db: &Db, layer: &str) -> LayerRules {
    let pwl = |c| Pwl(db.layerantenna_diff_pwl(layer, c));
    LayerRules {
        valid: db.layerantenna_is_valid(layer),
        par: db.layerantenna_get_p_a_r(layer),
        car: db.layerantenna_get_c_a_r(layer),
        psr: db.layerantenna_get_p_s_r(layer),
        csr: db.layerantenna_get_c_s_r(layer),
        diff_par: pwl(DiffCurve::Par),
        diff_car: pwl(DiffCurve::Car),
        diff_psr: pwl(DiffCurve::Psr),
        diff_csr: pwl(DiffCurve::Csr),
    }
}

/// Evaluate one net against the layer rules, appending any violations.
///
/// Pure over its inputs — no database — so the ratio logic is testable without building a
/// design. `rules` maps layer name to limits; a layer missing from the map is treated as
/// having no rule.
///
/// A layer with no rule still contributes to the cumulative totals but is never itself
/// compared: omitting its metal from CAR would under-report the charge that layers above it
/// inherit, which is precisely the error the cumulative ratio exists to catch.
pub fn check_net(
    net: &NetAntenna,
    rules: &std::collections::BTreeMap<String, LayerRules>,
    out: &mut Vec<Violation>,
) {
    if net.gates.is_empty() {
        return; // no gate: the ratio has no denominator, so there is nothing to decide
    }
    let (mut cum_area, mut cum_side) = (0.0f64, 0.0f64);
    for l in &net.layers {
        cum_area += l.area_um2;
        cum_side += l.side_area_um2;
        let Some(r) = rules.get(&l.layer).filter(|r| r.has_any_limit()) else { continue };

        // One verdict per GATE PIN. All of this layer's metal is charged against each gate
        // individually, because the collected charge reaches all of them.
        for g in &net.gates {
            let mut test = |ratio: Ratio, area: f64| {
                // The limit depends on the net's diffusion area when the technology states a
                // diff-ratio curve — which is how a protection diode raises the bar rather
                // than being invisible to the check.
                let limit = r.limit(ratio, net.diff_area_um2);
                if let Some(value) = LayerRules::exceeds(limit, area, g.gate_area_um2) {
                    out.push(Violation {
                        net: net.net.clone(),
                        pin: g.pin.clone(),
                        layer: l.layer.clone(),
                        ratio,
                        value,
                        limit,
                        gate_area_um2: g.gate_area_um2,
                        diff_area_um2: net.diff_area_um2,
                        metal_area_um2: area,
                    });
                }
            };
            test(Ratio::Par, l.area_um2);
            test(Ratio::Car, cum_area);
            // Side-area ratios need a stated thickness; without one the side area is unknown,
            // and testing it as 0 would report a pass we have not earned.
            if l.side_area_um2 > 0.0 {
                test(Ratio::Psr, l.side_area_um2);
                test(Ratio::Csr, cum_side);
            }
        }
    }
}

/// Check every net in the design.
pub fn check_design(db: &Db) -> Report {
    let dbu = db.dbu_per_micron() as f64;
    let mut r = Report {
        status: "clean",
        count: 0,
        nets_checked: 0,
        nets_no_gate: 0,
        nets_unrouted: 0,
        layers_without_rules: Vec::new(),
        no_rules_found: true,
        violations: Vec::new(),
    };
    if dbu <= 0.0 {
        // Without a DBU scale every ratio is meaningless; refuse rather than emit numbers
        // that look plausible and are off by orders of magnitude.
        r.status = "error";
        return r;
    }

    // Rules are per layer, not per net — read them once. Doing it inside the net loop would
    // re-cross the FFI boundary for every net on every layer, for an answer that cannot change.
    let mut rules: std::collections::BTreeMap<String, LayerRules> = Default::default();

    for net in db.net_names() {
        let Some(na) = read_net(db, &net, dbu) else {
            r.nets_unrouted += 1;
            continue;
        };
        for l in &na.layers {
            let entry = rules
                .entry(l.layer.clone())
                .or_insert_with(|| read_layer_rules(db, &l.layer));
            if entry.has_any_limit() {
                r.no_rules_found = false;
            } else if !r.layers_without_rules.contains(&l.layer) {
                r.layers_without_rules.push(l.layer.clone());
            }
        }
        if na.gates.is_empty() {
            r.nets_no_gate += 1;
            continue;
        }
        r.nets_checked += 1;
        check_net(&na, &rules, &mut r.violations);
    }

    r.layers_without_rules.sort();
    r.count = r.violations.len();
    r.status = if r.count == 0 { "clean" } else { "violations" };
    r
}
