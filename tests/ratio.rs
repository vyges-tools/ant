// SPDX-License-Identifier: Apache-2.0
//! The ratio logic, tested without a database.
//!
//! `check_net` is pure over `(NetAntenna, rules)`, so the decisions that matter — cumulative
//! accumulation, undeclared limits, missing denominators, per-gate attribution — are testable
//! directly rather than inferred from a design that happens to exercise them.
//!
//! The graph walk that *produces* these inputs is tested separately in `src/graph.rs`.

use std::collections::BTreeMap;
use vyges_ant::{
    check_net, GateExposure, LayerRules, NetAntenna, Pwl, Ratio, StageMetal, Violation,
};

/// A stage: metal this gate has collected by the time `layer` is deposited.
fn stage(layer: &str, n: i64, area: f64, side: f64, cum: f64, cum_side: f64) -> StageMetal {
    StageMetal {
        layer: layer.into(),
        layer_number: n,
        area_um2: area,
        side_area_um2: side,
        cum_area_um2: cum,
        cum_side_area_um2: cum_side,
    }
}

fn gate(pin: &str, gate_area: f64, stages: Vec<StageMetal>) -> GateExposure {
    GateExposure { pin: pin.into(), gate_area_um2: gate_area, stages }
}

fn net(gates: Vec<GateExposure>, diff: f64) -> NetAntenna {
    NetAntenna { net: "n".into(), diff_area_um2: diff, gates, gates_unanchored: 0 }
}

fn rules(pairs: Vec<(&str, LayerRules)>) -> BTreeMap<String, LayerRules> {
    pairs.into_iter().map(|(n, r)| (n.to_string(), r)).collect()
}

/// Only the ratios a rule declares are tested. `par: 0.0` means "the LEF states no PAR limit",
/// and must not be read as a limit of zero — which would fail every net carrying any metal.
fn m1_par_only() -> LayerRules {
    LayerRules { valid: true, par: 10.0, ..Default::default() }
}

fn check(n: &NetAntenna, r: &BTreeMap<String, LayerRules>) -> Vec<Violation> {
    let mut v = Vec::new();
    check_net(n, r, &mut v);
    v
}

#[test]
fn par_flags_only_above_the_limit() {
    let r = rules(vec![("met1", m1_par_only())]);

    // 100 µm² over a 10 µm² gate = 10.0, exactly at the limit. `>` not `>=`: a gate at the
    // stated limit is legal, and rounding a boundary case into a violation is a false alarm.
    let at = net(vec![gate("u1/A", 10.0, vec![stage("met1", 1, 100.0, 0.0, 100.0, 0.0)])], 0.0);
    assert!(check(&at, &r).is_empty(), "a gate exactly at the limit must pass");

    let over = net(vec![gate("u1/A", 10.0, vec![stage("met1", 1, 100.1, 0.0, 100.1, 0.0)])], 0.0);
    let v = check(&over, &r);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].ratio, Ratio::Par);
    assert_eq!(v[0].layer, "met1");
    assert!((v[0].value - 10.01).abs() < 1e-9, "value was {}", v[0].value);
}

/// The reason CAR exists: each layer passes PAR, and the accumulated stack still violates.
#[test]
fn car_catches_what_par_cannot() {
    let r = rules(vec![
        ("met1", LayerRules { valid: true, par: 10.0, car: 15.0, ..Default::default() }),
        ("met2", LayerRules { valid: true, par: 10.0, car: 15.0, ..Default::default() }),
    ]);
    let n = net(
        vec![gate(
            "u1/A",
            1.0,
            vec![
                stage("met1", 1, 9.0, 0.0, 9.0, 0.0),  // 9x partial, 9x cumulative
                stage("met2", 2, 9.0, 0.0, 18.0, 0.0), // 9x partial, 18x cumulative
            ],
        )],
        0.0,
    );
    let v = check(&n, &r);
    assert_eq!(v.len(), 1, "expected exactly the CAR violation, got {v:?}");
    assert_eq!(v[0].ratio, Ratio::Car);
    assert_eq!(v[0].layer, "met2", "CAR is charged at the layer that tips it over");
    assert!((v[0].metal_area_um2 - 18.0).abs() < 1e-9);
}

/// A layer with no rule is skipped for comparison, but its metal is already inside the
/// cumulative figures the walk produced — dropping it would under-report what layers above
/// inherit, the exact error the cumulative ratio exists to catch.
#[test]
fn unruled_layers_still_count_toward_car_above_them() {
    let r = rules(vec![
        // met1 carries no rule at all.
        ("met2", LayerRules { valid: true, par: 100.0, car: 15.0, ..Default::default() }),
    ]);
    let n = net(
        vec![gate(
            "u1/A",
            1.0,
            vec![
                stage("met1", 1, 10.0, 0.0, 10.0, 0.0),
                stage("met2", 2, 8.0, 0.0, 18.0, 0.0), // met1's 10 is inside this 18
            ],
        )],
        0.0,
    );
    let v = check(&n, &r);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].ratio, Ratio::Car);
    assert!((v[0].metal_area_um2 - 18.0).abs() < 1e-9, "got {}", v[0].metal_area_um2);
}

/// `valid: false` means the layer states no antenna rule; its declared numbers are not limits.
#[test]
fn invalid_rules_are_not_applied() {
    let r = rules(vec![("met1", LayerRules { valid: false, par: 0.001, ..Default::default() })]);
    let n = net(vec![gate("u1/A", 1.0, vec![stage("met1", 1, 1000.0, 0.0, 1000.0, 0.0)])], 0.0);
    assert!(check(&n, &r).is_empty());
}

/// No gate area means no denominator. That is *not applicable*, not a pass — and it must never
/// divide by zero and emit an infinite ratio.
#[test]
fn no_gate_area_yields_no_verdict() {
    let r = rules(vec![("met1", m1_par_only())]);
    let n = net(vec![gate("u1/A", 0.0, vec![stage("met1", 1, 1e6, 0.0, 1e6, 0.0)])], 0.0);
    assert!(check(&n, &r).is_empty());
    assert_eq!(LayerRules::exceeds(10.0, 1e6, 0.0), None);
}

/// Side-area ratios are only evaluated when a thickness was stated. A side area of 0 means
/// "unknown", and passing it would report a check we never ran.
#[test]
fn side_ratios_skipped_when_thickness_unknown() {
    let r =
        rules(vec![("met1", LayerRules { valid: true, psr: 1.0, csr: 1.0, ..Default::default() })]);
    let unknown = net(vec![gate("u1/A", 1.0, vec![stage("met1", 1, 5.0, 0.0, 5.0, 0.0)])], 0.0);
    assert!(check(&unknown, &r).is_empty(), "must not pass a ratio it could not compute");

    let known = net(vec![gate("u1/A", 1.0, vec![stage("met1", 1, 5.0, 2.0, 5.0, 2.0)])], 0.0);
    let v = check(&known, &r);
    assert_eq!(v.len(), 2, "PSR and CSR both exceed 1.0: {v:?}");
    assert!(v.iter().any(|x| x.ratio == Ratio::Psr));
    assert!(v.iter().any(|x| x.ratio == Ratio::Csr));
}

/// An undeclared limit (0.0) is not a limit of zero.
#[test]
fn undeclared_limits_never_fire() {
    assert_eq!(LayerRules::exceeds(0.0, 1e9, 1.0), None);
    assert_eq!(LayerRules::exceeds(10.0, 100.0, 10.0), None, "exactly at the limit passes");
    assert_eq!(LayerRules::exceeds(10.0, 101.0, 10.0), Some(10.1));
}

// ---- per-gate attribution ------------------------------------------------------------------

/// **The regression this exists to prevent**, in both halves.
///
/// Each gate is judged on its OWN denominator and its OWN collected metal. Correlating against
/// OpenROAD `check_antennas` caught both errors: summing the net's gate areas hid 68 of 73
/// violating nets, and charging the net's whole per-layer metal to every gate invented
/// thousands that were not real.
#[test]
fn each_gate_is_judged_on_its_own_metal_and_its_own_area() {
    let r = rules(vec![("met1", m1_par_only())]); // PAR limit 10.0
    // Two gates on one net, on different branches: one collected 200 µm², the other 20.
    let n = net(
        vec![
            gate("big/A", 10.0, vec![stage("met1", 1, 200.0, 0.0, 200.0, 0.0)]),
            gate("small/A", 10.0, vec![stage("met1", 1, 20.0, 0.0, 20.0, 0.0)]),
        ],
        0.0,
    );
    let v = check(&n, &r);
    assert_eq!(v.len(), 1, "only the gate on the big branch violates: {v:?}");
    assert_eq!(v[0].pin, "big/A");
    assert!((v[0].value - 20.0).abs() < 1e-9, "200/10 — this gate's metal, not the net's total");

    // Same collected metal, a smaller gate: the denominator is that pin's own.
    let n2 = net(vec![gate("tiny/A", 1.0, vec![stage("met1", 1, 20.0, 0.0, 20.0, 0.0)])], 0.0);
    let v2 = check(&n2, &r);
    assert_eq!(v2.len(), 1);
    assert!((v2[0].value - 20.0).abs() < 1e-9);
    assert_eq!(v2[0].gate_area_um2, 1.0);
}

// ---- diffusion-dependent limits ------------------------------------------------------------

/// The real sky130 curve, as read from a routed design: identical on met1–met3, and the only
/// antenna limit sky130 states. Pinned here so a change in how curves are read shows up as a
/// test failure rather than as a quietly different verdict.
fn sky130_diff_psr() -> Pwl {
    Pwl(vec![(0.0, 400.0), (0.0125, 400.0), (0.0225, 2609.0), (22.5, 11600.0)])
}

#[test]
fn pwl_interpolates_between_points_and_clamps_outside() {
    let c = sky130_diff_psr();
    // Clamped below the first index and above the last: a LEF table states the limit over the
    // diffusion areas the foundry characterised, and extrapolating past either end would be
    // inventing an answer the technology never gave.
    assert_eq!(c.eval(-1.0), Some(400.0));
    assert_eq!(c.eval(0.0), Some(400.0));
    assert_eq!(c.eval(1000.0), Some(11600.0));
    assert_eq!(c.eval(0.005), Some(400.0), "flat segment");
    // Midway across (0.0125, 400) -> (0.0225, 2609): 400 + (2609-400)/2 = 1504.5
    let mid = c.eval(0.0175).unwrap();
    assert!((mid - 1504.5).abs() < 1e-9, "interpolated {mid}, expected 1504.5");
}

#[test]
fn pwl_edge_cases() {
    assert_eq!(Pwl(vec![]).eval(1.0), None, "an unset curve has no limit");
    // odb's convention: one point is a constant, not a curve.
    assert_eq!(Pwl(vec![(5.0, 42.0)]).eval(0.0), Some(42.0));
    assert_eq!(Pwl(vec![(5.0, 42.0)]).eval(1e9), Some(42.0));
    // Coincident indices (a step) must not divide by zero.
    assert_eq!(Pwl(vec![(1.0, 10.0), (1.0, 20.0)]).eval(1.0), Some(10.0));
}

/// The diff curve wins where the technology states one: it is the limit the foundry
/// characterised for a net carrying that much diffusion.
#[test]
fn diff_curve_overrides_the_plain_ratio() {
    let r = LayerRules {
        valid: true,
        psr: 50.0, // plain limit, used only if no curve exists
        diff_psr: sky130_diff_psr(),
        ..Default::default()
    };
    assert_eq!(r.limit(Ratio::Psr, 0.0), 400.0, "curve wins over the plain 50.0");
    assert_eq!(r.limit(Ratio::Psr, 22.5), 11600.0);
    assert_eq!(r.limit(Ratio::Par, 0.0), 0.0, "undeclared stays undeclared");
}

/// More diffusion buys a higher permitted ratio — how a protection diode earns relief, and the
/// reason the limit cannot be read without the net's diffusion area.
#[test]
fn diffusion_raises_the_limit_and_can_clear_a_violation() {
    let r = rules(vec![(
        "met1",
        LayerRules { valid: true, diff_psr: sky130_diff_psr(), ..Default::default() },
    )]);
    let stages = vec![stage("met1", 1, 0.0, 1000.0, 0.0, 1000.0)];

    let bare = net(vec![gate("u1/A", 1.0, stages.clone())], 0.0);
    let v = check(&bare, &r);
    // Exactly one violation, PSR — *not* two. sky130 states no CSR limit in either form, so the
    // cumulative side-area ratio is genuinely unchecked there, and firing on it would be
    // inventing a limit. This asserts that gap rather than papering over it.
    assert_eq!(v.len(), 1, "only PSR is limited on this layer: {v:?}");
    assert_eq!(v[0].ratio, Ratio::Psr);
    assert!((v[0].limit - 400.0).abs() < 1e-9);
    assert_eq!(v[0].diff_area_um2, 0.0);

    // The same metal on a net with 22.5 µm² of diffusion is under the 11600 limit.
    let diode = net(vec![gate("u1/A", 1.0, stages)], 22.5);
    assert!(check(&diode, &r).is_empty(), "diffusion must raise the limit enough to clear this");
}

/// A layer stating only a diff curve is still checkable — the case that matters, since it is
/// every sky130 routing layer. Before the curves were bridged this layer looked ruleless.
#[test]
fn a_layer_with_only_a_diff_curve_is_checked() {
    let only_curve = LayerRules { valid: false, diff_psr: sky130_diff_psr(), ..Default::default() };
    assert!(only_curve.has_any_limit(), "isValid() is false, yet there IS a limit here");

    let r = rules(vec![("met1", only_curve)]);
    let n = net(vec![gate("u1/A", 1.0, vec![stage("met1", 1, 0.0, 500.0, 0.0, 500.0)])], 0.0);
    assert!(!check(&n, &r).is_empty(), "500 > 400 must be caught");
}

/// A layer with genuinely nothing stated is still not checkable, and must not look like one
/// that is — otherwise `no_rules_found` stops meaning anything.
#[test]
fn a_layer_with_nothing_stated_has_no_limit() {
    assert!(!LayerRules::default().has_any_limit());
}
