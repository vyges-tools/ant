// SPDX-License-Identifier: Apache-2.0
//! The ratio logic, tested without a database.
//!
//! `check_net` is pure over `(NetAntenna, rules)`, so the decisions that matter — cumulative
//! accumulation, undeclared limits, missing denominators — are testable directly rather than
//! inferred from a design that happens to exercise them.

use std::collections::BTreeMap;
use vyges_ant::{check_net, LayerMetal, LayerRules, NetAntenna, PinGate, Pwl, Ratio, Violation};

fn layer(name: &str, level: i32, area: f64, side: f64) -> LayerMetal {
    LayerMetal { layer: name.into(), routing_level: level, area_um2: area, side_area_um2: side }
}

fn rules(pairs: Vec<(&str, LayerRules)>) -> BTreeMap<String, LayerRules> {
    pairs.into_iter().map(|(n, r)| (n.to_string(), r)).collect()
}

/// Only the ratios a rule declares are tested. `par: 0.0` means "the LEF states no PAR limit",
/// and must not be read as a limit of zero — which would fail every net carrying any metal.
fn m1_par_only() -> LayerRules {
    LayerRules { valid: true, par: 10.0, ..Default::default() }
}

fn check(net: &NetAntenna, r: &BTreeMap<String, LayerRules>) -> Vec<Violation> {
    let mut v = Vec::new();
    check_net(net, r, &mut v);
    v
}

#[test]
fn par_flags_only_above_the_limit() {
    let r = rules(vec![("met1", m1_par_only())]);

    // 100 µm² over a 10 µm² gate = 10.0, exactly at the limit. `>` not `>=`: a net at the
    // stated limit is legal, and rounding a boundary case into a violation is a false alarm.
    let at = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 10.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 100.0, 0.0)],
    };
    assert!(check(&at, &r).is_empty(), "a net exactly at the limit must pass");

    let over = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 10.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 100.1, 0.0)],
    };
    let v = check(&over, &r);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].ratio, Ratio::Par);
    assert_eq!(v[0].layer, "met1");
    assert!((v[0].value - 10.01).abs() < 1e-9, "value was {}", v[0].value);
}

/// The reason CAR exists: every layer passes PAR, and the stack still violates.
#[test]
fn car_catches_what_par_cannot() {
    let r = rules(vec![
        ("met1", LayerRules { valid: true, par: 10.0, car: 15.0, ..Default::default() }),
        ("met2", LayerRules { valid: true, par: 10.0, car: 15.0, ..Default::default() }),
    ]);
    // 9x and 9x individually — both under PAR 10 — but 18x cumulative, over CAR 15.
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 9.0, 0.0), layer("met2", 2, 9.0, 0.0)],
    };
    let v = check(&net, &r);
    assert_eq!(v.len(), 1, "expected exactly the CAR violation, got {v:?}");
    assert_eq!(v[0].ratio, Ratio::Car);
    assert_eq!(v[0].layer, "met2", "CAR is charged at the layer that tips it over");
    assert!((v[0].metal_area_um2 - 18.0).abs() < 1e-9);
}

/// A layer with no rule is still accumulated. Dropping its metal would under-report the charge
/// that layers above it inherit — the exact error the cumulative ratio exists to catch.
#[test]
fn unruled_layers_still_accumulate_into_car() {
    let r = rules(vec![
        // met1 carries no rule at all.
        ("met2", LayerRules { valid: true, par: 100.0, car: 15.0, ..Default::default() }),
    ]);
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 10.0, 0.0), layer("met2", 2, 8.0, 0.0)],
    };
    let v = check(&net, &r);
    assert_eq!(v.len(), 1, "met1's 10 µm² must count toward met2's CAR");
    assert_eq!(v[0].ratio, Ratio::Car);
    assert!((v[0].metal_area_um2 - 18.0).abs() < 1e-9, "got {}", v[0].metal_area_um2);
}

/// `valid: false` means the layer states no antenna rule; its declared numbers are not limits.
#[test]
fn invalid_rules_are_not_applied() {
    let r = rules(vec![("met1", LayerRules { valid: false, par: 0.001, ..Default::default() })]);
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 1000.0, 0.0)],
    };
    assert!(check(&net, &r).is_empty());
}

/// No gate on the net means no denominator. That is *not applicable*, not a pass — and it must
/// never divide by zero and emit an infinite ratio.
#[test]
fn no_gate_area_yields_no_verdict() {
    let r = rules(vec![("met1", m1_par_only())]);
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 0.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 1e6, 0.0)],
    };
    assert!(check(&net, &r).is_empty());
    assert_eq!(LayerRules::exceeds(10.0, 1e6, 0.0), None);
}

/// Side-area ratios are only evaluated when a thickness was stated. A side area of 0 means
/// "unknown", and passing it would report a check we never ran.
#[test]
fn side_ratios_skipped_when_thickness_unknown() {
    let r = rules(vec![(
        "met1",
        LayerRules { valid: true, psr: 1.0, csr: 1.0, ..Default::default() },
    )]);
    let unknown = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 5.0, 0.0)], // thickness unstated -> side area 0
    };
    assert!(check(&unknown, &r).is_empty(), "must not pass a ratio it could not compute");

    let known = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 5.0, 2.0)],
    };
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

// ---- diffusion-dependent limits ------------------------------------------------------------

/// The real sky130 curve, as read from the routed fixture: identical on met1–met3, and the
/// only antenna limit sky130 states. Pinned here so a change in how curves are read shows up
/// as a test failure rather than as a quietly different verdict.
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
    // Flat segment.
    assert_eq!(c.eval(0.005), Some(400.0));
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
    // No curve for PAR, so the plain value stands.
    assert_eq!(r.limit(Ratio::Par, 0.0), 0.0, "undeclared stays undeclared");
}

/// More diffusion buys a higher permitted ratio — which is how a protection diode earns
/// relief, and the reason the limit cannot be read without the net's diffusion area.
#[test]
fn diffusion_raises_the_limit_and_can_clear_a_violation() {
    let r = rules(vec![(
        "met1",
        LayerRules { valid: true, diff_psr: sky130_diff_psr(), ..Default::default() },
    )]);
    // Side-area ratio of 1000: over the 400 limit at zero diffusion.
    let bare = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 0.0, 1000.0)],
    };
    let v = check(&bare, &r);
    // Exactly one violation, PSR — *not* two. sky130 states no CSR limit in either form, so
    // the cumulative side-area ratio is genuinely unchecked there, and firing on it would be
    // inventing a limit. This asserts that gap rather than papering over it.
    assert_eq!(v.len(), 1, "only PSR is limited on this layer: {v:?}");
    assert_eq!(v[0].ratio, Ratio::Psr);
    assert!((v[0].limit - 400.0).abs() < 1e-9);
    assert_eq!(v[0].diff_area_um2, 0.0);

    // The same metal on a net with 22.5 µm² of diffusion is under the 11600 limit.
    let diode = NetAntenna { diff_area_um2: 22.5, ..bare.clone() };
    assert!(
        check(&diode, &r).is_empty(),
        "diffusion must raise the limit enough to clear this net"
    );
}

/// A layer stating only a diff curve is still checkable — the case that matters, since it is
/// every sky130 routing layer. Before the curves were bridged this layer looked ruleless.
#[test]
fn a_layer_with_only_a_diff_curve_is_checked() {
    let only_curve =
        LayerRules { valid: false, diff_psr: sky130_diff_psr(), ..Default::default() };
    assert!(only_curve.has_any_limit(), "isValid() is false, yet there IS a limit here");

    let r = rules(vec![("met1", only_curve)]);
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 1.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 0.0, 500.0)],
    };
    assert!(!check(&net, &r).is_empty(), "500 > 400 must be caught");
}

/// A layer with genuinely nothing stated is still not checkable, and must not look like one
/// that is — otherwise `no_rules_found` stops meaning anything.
#[test]
fn a_layer_with_nothing_stated_has_no_limit() {
    assert!(!LayerRules::default().has_any_limit());
}

/// **The regression this exists to prevent.** The ratio is charged against each gate pin
/// separately, never against the net's summed gate area.
///
/// Physically: the charge collected by the metal reaches every gate on the net, so each gate is
/// exposed to *all* of it. Summing the denominators asks "is this metal large relative to all
/// the silicon it feeds", which no gate experiences.
///
/// Measured cost of getting this wrong: against OpenROAD `check_antennas` on a routed block,
/// the summed-denominator version reported 5 of 73 violating nets — it hid 68, silently, in the
/// direction that matters.
#[test]
fn the_ratio_is_per_gate_pin_not_per_net() {
    let r = rules(vec![("met1", m1_par_only())]); // PAR limit 10.0
    // Four pins of 1 µm² each. Summed, the denominator is 4 µm² and 30 µm² of metal gives a
    // ratio of 7.5 — under the limit. Per pin it is 30.0, three times over.
    let net = NetAntenna {
        net: "n".into(),
        gates: (0..4)
            .map(|i| PinGate { pin: format!("u{i}/A"), gate_area_um2: 1.0 })
            .collect(),
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 30.0, 0.0)],
    };
    let v = check(&net, &r);
    assert_eq!(v.len(), 4, "every exposed gate is its own verdict: {v:?}");
    for x in &v {
        assert!((x.value - 30.0).abs() < 1e-9, "each gate sees all 30 µm², got {}", x.value);
        assert_eq!(x.gate_area_um2, 1.0, "the denominator is one pin's gate, not the sum");
    }
    // Every pin named, so a report says which gate is at risk rather than only which net.
    let pins: Vec<&str> = v.iter().map(|x| x.pin.as_str()).collect();
    assert_eq!(pins, vec!["u0/A", "u1/A", "u2/A", "u3/A"]);
}

/// A pin with no gate area is not a gate; it must not become a zero-denominator verdict.
#[test]
fn pins_without_gate_area_are_not_evaluated() {
    let r = rules(vec![("met1", m1_par_only())]);
    let net = NetAntenna {
        net: "n".into(),
        gates: vec![PinGate { pin: "u1/A".into(), gate_area_um2: 0.0 }],
        diff_area_um2: 0.0,
        layers: vec![layer("met1", 1, 1e6, 0.0)],
    };
    assert!(check(&net, &r).is_empty());
}
