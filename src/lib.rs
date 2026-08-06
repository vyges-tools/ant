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

pub mod graph;

use graph::NetGraph;
use serde::Serialize;
use vyges_opendb::{DiffCurve, Db, LayerBox};

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

/// One conductor at one manufacturing stage, and the gates sharing it.
///
/// The ratio is charged **per region, not per gate**. The charge a conductor collects is shared
/// by every gate connected to it, so the denominator is the region's summed gate area. Measured
/// against OpenROAD on one net: region metal of 1603.33 µm² over its three gates
/// (0.4347 + 0.4347 + 0.126 = 0.9954) gives 1610.7 against their 1611.2 — 0.03%. Charging each
/// gate its own area instead gave 7.9× too high.
#[derive(Debug, Clone, Serialize)]
pub struct RegionExposure {
    pub layer: String,
    /// odb layer number — the stage order, ascending.
    pub layer_number: i64,
    /// The gates on this conductor. They all see the same metal, so they share one verdict;
    /// naming them keeps the report actionable rather than anonymous.
    pub pins: Vec<String>,
    /// Sum of the gate areas on this conductor — the ratio's denominator.
    pub gate_area_um2: f64,
    /// Diffusion area on **this conductor** — the index into the diff-ratio curves.
    ///
    /// Per conductor, not per net. Diffusion elsewhere on the net has not yet drained into this
    /// one at this stage, and a net-wide total is never smaller, so using it sets the limit too
    /// high and lets violations slip under. Matches OpenROAD's per-node `iterm_diff_area`.
    pub diff_area_um2: f64,
    /// Metal on this layer alone, reachable within the region (the PAR numerator).
    pub area_um2: f64,
    /// Perimeter × layer thickness. Zero when the LEF states no thickness — in which case the
    /// side-area ratios are *unavailable*, not zero, and are skipped rather than passed.
    pub side_area_um2: f64,
    /// Metal on this layer and every layer below, within the region (the CAR numerator).
    pub cum_area_um2: f64,
    pub cum_side_area_um2: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetAntenna {
    pub net: String,
    /// Diffusion over the whole net (µm²). **Informational only** — the limit is indexed by
    /// each conductor's own diffusion ([`RegionExposure::diff_area_um2`]), since diffusion
    /// elsewhere on the net has not drained into this conductor yet. Kept because the
    /// difference between the two is exactly what `explain` exists to show.
    pub diff_area_um2: f64,
    /// Every (region, stage) pair carrying at least one terminal.
    pub regions: Vec<RegionExposure>,
    /// Pins with a gate area that could not be anchored to the routing (unplaced, or sitting
    /// over no metal). Reported rather than dropped: an unanchored gate is unchecked, and
    /// silently skipping it would look identical to passing.
    pub gates_unanchored: usize,
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
    /// Gates that could not be anchored to the routing (unplaced pin, or a pin sitting over no
    /// metal). Each is a gate that went unchecked, so this is reported rather than dropped —
    /// silently skipping one looks exactly like passing it.
    pub gates_unanchored: usize,
    pub violations: Vec<Violation>,
}

/// Read one net's antenna inputs, converted to µm².
///
/// Returns `None` for a net with no routed metal — not an error; an unrouted net collects
/// nothing. The DBU→µm conversion happens here, once, because the LEF states gate area in µm²
/// while the database states geometry in DBU: comparing them raw is a ~10⁶ error on sky130.
pub fn read_net(db: &Db, net: &str, dbu: f64) -> Option<NetAntenna> {
    let boxes = db.net_wire_boxes(net);
    if boxes.is_empty() {
        return None; // unrouted: collects nothing, not an error
    }

    // Terminals first: the graph cannot be built without their pin metal, because pins cut it.
    struct Term {
        name: String,
        gate: f64,
        diff: f64,
        boxes: Vec<LayerBox>,
    }
    let mut terms: Vec<Term> = Vec::new();
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
        let diff = db.mterm_antenna_diff_area(&master, pin);
        diff_area_um2 += diff;
        // Every terminal's metal cuts the wire, whether or not it is a gate — a diode's pin
        // bounds an antenna region exactly as a gate's does.
        let pin_boxes: Vec<LayerBox> = db
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
            .collect();
        terms.push(Term { name: iterm.clone(), gate, diff, boxes: pin_boxes });
    }

    let all_pins: Vec<LayerBox> = terms.iter().flat_map(|t| t.boxes.iter().copied()).collect();
    let graph = NetGraph::build(&boxes, &all_pins);
    if graph.is_empty() || graph.layers.is_empty() {
        return None;
    }

    let layer_info: Vec<(String, f64)> = graph
        .layers
        .iter()
        .map(|&n| {
            let name = db.layer_name_by_number(n);
            let thick = db.layer_thickness(&name) as f64;
            (name, thick)
        })
        .collect();

    // Attach each terminal to the conductors its own metal touches.
    let mut attach: Vec<Vec<usize>> = Vec::new();
    let mut kept: Vec<&Term> = Vec::new();
    let mut gates_unanchored = 0usize;
    for t in &terms {
        if t.gate <= 0.0 && t.diff <= 0.0 {
            continue; // contributes to neither side of the ratio
        }
        let touched = graph.touched_by(&t.boxes);
        if touched.is_empty() {
            // Only a lost GATE means a gate went unchecked; lost diffusion is a missing relief,
            // which is conservative rather than silent.
            if t.gate > 0.0 {
                gates_unanchored += 1;
            }
            continue;
        }
        attach.push(touched);
        kept.push(t);
    }

    let dbu2 = dbu * dbu;
    let mut regions: Vec<RegionExposure> = Vec::new();
    if !attach.is_empty() {
        for (i, &stage) in graph.layers.iter().enumerate() {
            let (name, thick) = &layer_info[i];
            for (idxs, c) in graph.regions_at(stage, &attach) {
                // Report only the gates — a pin with diffusion and no gate is not at risk, it
                // is what protects the ones that are.
                let gate_pins: Vec<String> = idxs
                    .iter()
                    .filter(|&&t| kept[t].gate > 0.0)
                    .map(|&t| kept[t].name.clone())
                    .collect();
                if gate_pins.is_empty() {
                    continue; // a conductor with no gate on it has nothing to damage
                }
                regions.push(RegionExposure {
                    layer: name.clone(),
                    layer_number: stage,
                    gate_area_um2: idxs.iter().map(|&t| kept[t].gate).sum(),
                    diff_area_um2: idxs.iter().map(|&t| kept[t].diff).sum(),
                    pins: gate_pins,
                    area_um2: c.layer_area as f64 / dbu2,
                    // Thickness of 0 means the LEF stated none; the product is then 0, which
                    // `check_net` treats as "unavailable" and skips rather than passing.
                    side_area_um2: (c.layer_perimeter as f64 * thick) / dbu2,
                    cum_area_um2: c.cumulative_area as f64 / dbu2,
                    cum_side_area_um2: (c.cumulative_perimeter as f64 * thick) / dbu2,
                });
            }
        }
    }

    Some(NetAntenna { net: net.to_string(), diff_area_um2, regions, gates_unanchored })
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
    /// Interpolates within the stated range and **extrapolates past the last point along the
    /// final segment's slope** — matching OpenROAD's `getPwlFactor`, which this exists to
    /// correlate against.
    ///
    /// We would rather clamp: a LEF table states the limit over the diffusion areas the foundry
    /// characterised, and extrapolating past the end raises the permitted ratio without
    /// evidence, which is the unsafe direction. Matching the reference wins here because a
    /// checker that disagrees with the incumbent is not usable as a cross-check, and on real
    /// data the diffusion areas sit far inside the table. Recorded as a deliberate difference
    /// from our own preference, not an oversight.
    pub fn eval(&self, x: f64) -> Option<f64> {
        let p = &self.0;
        match p.len() {
            0 => None,
            1 => Some(p[0].1), // odb: a single point is a constant, not a curve
            _ => {
                if x <= p[0].0 {
                    return Some(p[0].1);
                }
                for w in p.windows(2) {
                    let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                    if x >= x0 && x < x1 {
                        let span = x1 - x0;
                        // Coincident indices would divide by zero; take the left value, which
                        // is what a step at that point means.
                        return Some(if span == 0.0 { y0 } else { y0 + (y1 - y0) * (x - x0) / span });
                    }
                }
                // Past the last point: continue along the final segment.
                let ((x0, y0), (x1, y1)) = (p[p.len() - 2], p[p.len() - 1]);
                let span = x1 - x0;
                Some(if span == 0.0 { y1 } else { y1 + (y1 - y0) * (x - x1) / span })
            }
        }
    }
}

/// One layer's antenna limits, in both the forms LEF can state them.
///
/// A plain limit of `0.0` means the LEF declares none for that ratio — **not** a limit of zero,
/// which would fail every net carrying any metal at all. That distinction is the difference
/// between a checker and a nuisance, so it lives in one place: [`LayerRules::exceeds`].
#[derive(Debug, Clone, PartialEq)]
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

    // ---- scaling factors (LEF ANTENNA*FACTOR) ----
    // Default to 1.0/0.0 so a technology stating none behaves as a plain ratio.
    /// `ANTENNAAREAFACTOR`, applied when the region has no diffusion.
    pub metal_factor: f64,
    /// Same factor, applied when it does — LEF can restrict it to the diffusion case
    /// (`isAreaFactorDiffUseOnly`), in which case only this one is set.
    pub diff_metal_factor: f64,
    /// `ANTENNASIDEAREAFACTOR`, and its diffusion-restricted twin.
    pub side_metal_factor: f64,
    pub diff_side_metal_factor: f64,
    /// `ANTENNAAREAMINUSDIFF` — subtracted from the numerator, scaled by diffusion area.
    pub minus_diff_factor: f64,
    /// `ANTENNAGATEPLUSDIFF` — added to the denominator, scaled by diffusion area. Superseded
    /// by [`LayerRules::gate_plus_diff`] when that curve is stated.
    pub plus_diff_factor: f64,
    /// PWL form of the gate-plus-diff protection term.
    pub gate_plus_diff: Pwl,
    /// `ANTENNAAREADIFFREDUCEPWL` — scales the numerator down as diffusion grows.
    pub area_diff_reduce: Pwl,
}

impl Default for LayerRules {
    fn default() -> Self {
        Self {
            valid: false,
            par: 0.0,
            car: 0.0,
            psr: 0.0,
            csr: 0.0,
            diff_par: Pwl::default(),
            diff_car: Pwl::default(),
            diff_psr: Pwl::default(),
            diff_csr: Pwl::default(),
            // A technology stating no factors must behave exactly as the bare ratio, so the
            // multiplicative ones default to 1 and the additive ones to 0.
            metal_factor: 1.0,
            diff_metal_factor: 1.0,
            side_metal_factor: 1.0,
            diff_side_metal_factor: 1.0,
            minus_diff_factor: 0.0,
            plus_diff_factor: 0.0,
            gate_plus_diff: Pwl::default(),
            area_diff_reduce: Pwl::default(),
        }
    }
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

    /// The computed ratio and the limit it is judged against, for one conductor.
    ///
    /// This is OpenROAD's `calculateWirePar` + `checkPSR`/`checkPAR` decision, transcribed:
    ///
    /// - A **diffusion** form is used when the region has diffusion **or** the technology states
    ///   no plain ratio. On sky130 the plain PSR is 0, so the diffusion form is always the one
    ///   in play — which is why an implementation without it finds nothing there.
    /// - With diffusion present, the numerator loses `minus_diff_factor × diff` and the
    ///   denominator gains a protection term (`plus_diff_factor × diff`, or the
    ///   `gate_plus_diff` curve where stated). That is a diode's relief expressed twice over.
    /// - `area_diff_reduce` scales the numerator down as diffusion grows.
    ///
    /// Returns `(value, limit)`; a limit of 0.0 means none is declared and nothing is decided.
    pub fn evaluate(&self, ratio: Ratio, metal: f64, gate_area: f64, diff: f64) -> (f64, f64) {
        if gate_area <= 0.0 {
            return (0.0, 0.0); // no denominator, no ratio
        }
        let is_side = matches!(ratio, Ratio::Psr | Ratio::Csr);
        let (curve, plain) = match ratio {
            Ratio::Par => (&self.diff_par, self.par),
            Ratio::Car => (&self.diff_car, self.car),
            Ratio::Psr => (&self.diff_psr, self.psr),
            Ratio::Csr => (&self.diff_csr, self.csr),
        };
        let (factor, diff_factor) = if is_side {
            (self.side_metal_factor, self.diff_side_metal_factor)
        } else {
            (self.metal_factor, self.diff_metal_factor)
        };
        let reduce = self.area_diff_reduce.eval(diff).unwrap_or(1.0);

        if diff != 0.0 || plain == 0.0 {
            let value = if diff != 0.0 {
                let protect = if self.gate_plus_diff.is_empty() {
                    self.plus_diff_factor * diff
                } else {
                    self.gate_plus_diff.eval(diff).unwrap_or(0.0)
                };
                (diff_factor * metal * reduce - self.minus_diff_factor * diff)
                    / (gate_area + protect)
            } else {
                factor * metal * reduce / gate_area
            };
            (value, curve.eval(diff).unwrap_or(0.0))
        } else {
            (factor * metal / gate_area, plain)
        }
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
    let area_f = db.layerantenna_get_area_factor(layer);
    let side_f = db.layerantenna_get_side_area_factor(layer);
    // LEF can restrict a factor to the diffusion case. When it does, the non-diffusion form
    // keeps its default of 1.0 — applying the factor there would scale a ratio the technology
    // never scaled.
    let area_diff_only = db.layerantenna_is_area_factor_diff_use_only(layer);
    let side_diff_only = db.layerantenna_is_side_area_factor_diff_use_only(layer);
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
        metal_factor: if area_diff_only { 1.0 } else { area_f },
        diff_metal_factor: area_f,
        side_metal_factor: if side_diff_only { 1.0 } else { side_f },
        diff_side_metal_factor: side_f,
        minus_diff_factor: db.layerantenna_get_area_minus_diff_factor(layer),
        plus_diff_factor: db.layerantenna_get_gate_plus_diff_factor(layer),
        gate_plus_diff: pwl(DiffCurve::GatePlusDiff),
        area_diff_reduce: pwl(DiffCurve::AreaDiffReduce),
    }
}

/// Evaluate one net against the layer rules, appending any violations.
///
/// Pure over its inputs — no database — so the ratio logic is testable without building a
/// design. `rules` maps layer name to limits; a layer missing from the map is treated as
/// having no rule.
///
/// One verdict per **region**, echoed for each gate on it, because every gate on a conductor
/// sees the same collected charge. A layer with no rule still contributes to the cumulative
/// totals (the walk already accumulated it) but is never itself compared: omitting its metal
/// from CAR would under-report the charge that layers above it inherit.
pub fn check_net(
    net: &NetAntenna,
    rules: &std::collections::BTreeMap<String, LayerRules>,
    out: &mut Vec<Violation>,
) {
    for rg in &net.regions {
        let Some(r) = rules.get(&rg.layer).filter(|r| r.has_any_limit()) else { continue };
        let mut test = |ratio: Ratio, area: f64| {
            // Value and limit together: which of them applies depends on whether THIS conductor
            // carries diffusion, so they cannot be computed independently.
            let (value, limit) = r.evaluate(ratio, area, rg.gate_area_um2, rg.diff_area_um2);
            if limit > 0.0 && value > limit {
                for pin in &rg.pins {
                    out.push(Violation {
                        net: net.net.clone(),
                        pin: pin.clone(),
                        layer: rg.layer.clone(),
                        ratio,
                        value,
                        limit,
                        gate_area_um2: rg.gate_area_um2,
                        diff_area_um2: rg.diff_area_um2,
                        metal_area_um2: area,
                    });
                }
            }
        };
        test(Ratio::Par, rg.area_um2);
        test(Ratio::Car, rg.cum_area_um2);
        // Side-area ratios need a stated thickness; without one the side area is unknown, and
        // testing it as 0 would report a pass we have not earned.
        if rg.side_area_um2 > 0.0 {
            test(Ratio::Psr, rg.side_area_um2);
            test(Ratio::Csr, rg.cum_side_area_um2);
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
        gates_unanchored: 0,
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
        r.gates_unanchored += na.gates_unanchored;
        for rg in &na.regions {
            let entry = rules
                .entry(rg.layer.clone())
                .or_insert_with(|| read_layer_rules(db, &rg.layer));
            if entry.has_any_limit() {
                r.no_rules_found = false;
            } else if !r.layers_without_rules.contains(&rg.layer) {
                r.layers_without_rules.push(rg.layer.clone());
            }
        }
        if na.regions.is_empty() {
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
