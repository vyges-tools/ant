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
| Violating nets OpenROAD found | 73 |
| …that this engine also found | **61 (84%)** |
| Violations this engine adds | **~2400** |

**Read that second number before trusting a verdict.** It finds most real violations and reports
many that are not real. It is useful today as a *screen* — a clean run is weak evidence and a
flagged net is worth looking at — and it is **not yet a sign-off gate**. Do not gate a tapeout on
it; run OpenROAD's `check_antennas` for that.

### Why the false positives — the metal-attribution model

The gap is understood, and it is a model gap rather than a bug. At the manufacturing stage where
layer *L* is deposited, the metal connected to a given gate is the routing sub-graph reachable
from that gate over layers ≤ *L*. Two gates on one net can sit on different branches and collect
very different amounts of metal until a higher layer joins them.

This engine sums the net's metal per layer and charges all of it to every gate on the net. So on
a net whose gates are on separate branches it over-attributes, sometimes by a lot — measured
against OpenROAD on one net: 5685.6 for every pin, where OpenROAD gives 720.2 for four of them
and 2786.0 for the fifth.

Closing it needs a walk of the routing graph from each gate pin rather than a per-layer sum.
Likewise the diffusion area is applied net-wide here, where the real limit varies per layer as
the path to diffusion completes — visible in OpenROAD's own report as the same pin requiring
400.00 on met1 and 3119.36 on met2.

## Other stated bounds

1. **Metal area double-counts overlap.** Shapes are summed as raw rectangles, not unioned, so
   metal that overlaps itself on a layer is counted twice — and perimeter, which drives the
   side-area ratios, suffers worse than area because interior junction edges are counted too.
2. **Layer order is routing level**, not a manufacturing step model. The standard CAR
   approximation.
3. **Cut layers are not checked.** OpenROAD evaluates `mcon`/`via`/`via2`… against their own
   ratios; this engine evaluates routing layers only.

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
