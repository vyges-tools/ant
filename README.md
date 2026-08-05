# vyges-ant

Antenna ratio sign-off over the **routed** design database. Reads a `.odb`, computes PAR / CAR /
PSR / CSR per net per routing layer against the limits the LEF states, and emits a verdict.

```sh
vyges-ant check routed.odb          # 0 clean · 1 violations · 2 error
vyges-ant check routed.odb -o antenna.json
vyges-ant --describe                # machine-readable contract
```

## What it checks

During manufacture, a metal shape connected to a gate but not yet to a diffusion path collects
charge; if it is large relative to the gate it damages the oxide. The check is a ratio — collected
metal over gate area — evaluated per layer, bottom-up:

| Ratio | Numerator |
| --- | --- |
| **PAR** | metal on this layer alone |
| **CAR** | metal on this layer *and every layer below* |
| **PSR** | side area (perimeter × thickness) on this layer |
| **CSR** | side area cumulative to this layer |

The cumulative forms are not redundant. A net legal on every layer taken individually can still
violate CAR, because the charge a gate sees is what the whole connected stack collected.

## Why the routed database, and not a GDS

`vyges-drc` already computes an antenna ratio — over GDS polygons, post-stream. This engine
computes it over `dbWire` routing, which is where OpenROAD's `ant` computes it and, more to the
point, the stage at which a violation can still be repaired by inserting a diode. A GDS answer
arrives after the last chance to act on it.

Same ratio, different substrate, different job. Neither replaces the other.

## What it does not do

It is a **checker**. It reports; it does not modify a design. Repair is a separate, reviewable
step: a plan is emitted, and an applier replays it — the same split as timing repair, for the same
reason. A checker that quietly became a repairer is a liability, not a convenience.

## Two forms of limit

LEF expresses antenna limits two ways, and a checker reading only one finds nothing on
technologies that use the other:

- **Plain ratios** (`ANTENNAAREARATIO` …) — a constant per layer.
- **Diffusion-dependent PWL ratios** (`ANTENNADIFFAREARATIO` …) — the limit as a
  piecewise-linear function of the diffusion area connected to the net. More diffusion permits a
  higher ratio, which is exactly how a protection diode earns relief.

Both are read. Where a technology states a diff curve it wins, because that is the limit the
foundry characterised for a net carrying that much diffusion; the plain ratio is the fallback.
Outside a curve's stated range the limit is **clamped, not extrapolated** — a LEF table covers
the diffusion areas the foundry characterised, and inventing values past either end would be
manufacturing an answer the technology never gave.

**This is not academic.** Measured on sky130: every routing layer carries an antenna rule object,
yet `dbTechLayerAntennaRule::isValid()` is false for all of them — that predicate is precisely
*"does any plain ratio exceed zero"*. sky130 states exactly one antenna limit, `DiffPSR`, as a
4-point curve identical on met1–met3:

```text
(0, 400)  (0.0125, 400)  (0.0225, 2609)  (22.5, 11600)
```

A plain-ratios-only checker sees no limits there at all.

## Maturity — measured, not claimed

Correlated against OpenROAD `check_antennas` on a routed sky130 block (~2500 nets, the same
`.odb`, 2026-08-05):

| | |
| --- | --- |
| Ratio values within 2% of OpenROAD's | **37 of 38** compared |
| Violations OpenROAD does not confirm | **6** |
| Violations matched exactly (net + pin + layer + ratio) | 37 of 83 |
| Violations missed | 46 |

**Where it produces a number, that number is right.** What it currently lacks is *recall* — it
misses more than half of the real violations. So a flagged net is now strong evidence, and a
clean run is still weak evidence. Not a sign-off gate: run `check_antennas` to gate a tapeout.

### The one remaining error, specified

Diffusion area is accumulated **net-wide**; OpenROAD accumulates it **per conductor**
(`info.iterm_diff_area += diffArea(...)` over the gates of a node). A net-wide total is never
smaller than a region's, so our diffusion-dependent limit comes out too *high* and violations
slip under it — which is exactly the shape of the misses: the limit deltas on disagreeing
records are all positive (352.1, 347.8, 173.9, 2726.0).

Two smaller deltas from the same reading of `AntennaChecker.cc`: the layer `metal_factor` /
`side_metal_factor` multipliers are not applied, and `diff_psr` is computed by a distinct
formula (`− minus_diff_factor × diff_area` in the numerator, `+ plus_diff_protect` in the
denominator, scaled by the `AreaDiffReduce` PWL) — and it is `diff_psr`, not `psr`, that
OpenROAD compares against a diffusion PWL limit.

### How the model was arrived at

Worth stating because two plausible models were wrong first:

1. **Sum gate areas across the whole net.** Hid 68 of 73 violating nets — the denominator was
   inflated by the gate count.
2. **Charge each gate its own area.** Over-reported by the gate count instead: exactly 2.00× on
   two-gate regions, which is what gave the model away.
3. **Charge each conductor, denominator summed over the gates on it.** Confirmed by reading
   `AntennaChecker.cc`, and checked numerically first: region metal of 1603.33 µm² over its
   three gates (0.4347 + 0.4347 + 0.126 = 0.9954) gives 1610.7 against OpenROAD's 1611.2.

A separate hypothesis — that summing rectangle perimeters instead of unioning them was the
dominant error — was implemented exactly and **rejected by measurement**: it changed the result
by one violation. The union is correct and stays; it simply was not the problem.

## Other stated bounds

1. **Layer order is routing level**, not a manufacturing step model. The standard CAR
   approximation.
2. **Gates anchor to the nearest metal** when no shape contains the pin's reported centre. The
   router lands on an access point inside the pin rectangle rather than at the centroid odb
   reports, so requiring containment left almost every gate unchecked; the pin is known to be on
   the net, so the closest metal of that net is the conductor it reaches.

And one gap that is the technology's rather than the tool's: a ratio stated in *neither* LEF form
is not checked. On sky130 that means PAR, CAR and CSR are unlimited and only PSR is evaluated —
which the golden report confirms, every one of its 83 violations being PSR.

## Verdicts that are not verdicts

Two cases are deliberately **not** reported as clean, because nothing was actually checked:

- **No antenna rules in the technology** — exit 2, not 0. A design whose LEF states no antenna
  limits has not passed anything.
- **A net with no gate area** — counted in `nets_no_gate`, not passed. With no denominator there
  is no ratio. A large count here means the library's antenna models are missing and the check is
  covering less than it appears to, which is why the number is in the report rather than swallowed.

## Report

```json
{
  "status": "clean",
  "count": 0,
  "nets_checked": 41,
  "nets_no_gate": 9,
  "nets_unrouted": 2,
  "gates_unanchored": 0,
  "layers_without_rules": [],
  "no_rules_found": false,
  "violations": []
}
```

## Building

Reads the database through [`vyges-opendb`](https://github.com/vyges-tools/opendb), which binds
OpenROAD's OpenDB (`libodb`). A first build compiles libodb and takes a while; later builds do not.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
