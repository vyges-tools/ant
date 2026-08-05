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
| Violations matched exactly (net + pin + layer + ratio) | **51 of 83** |
| Violations this engine adds | **~935** |
| Ratio values within 2% of OpenROAD's | **36 of 109** compared |

**Read the third row before trusting a verdict.** It finds most real violations and reports many
that are not real. Useful today as a *screen* — a flagged net is worth looking at, a clean run is
weak evidence — and **not a sign-off gate**. Run OpenROAD's `check_antennas` to gate a tapeout.

The trend is the useful part: attributing metal per gate by walking the routing (rather than
summing the net's metal per layer) cut false positives from ~2400 to ~935 and lifted value
agreement from 5% to 33%.

### What is still wrong

**Perimeter double-counts at junctions.** Shapes are summed as raw rectangles rather than
unioned. For *area* that only matters where metal overlaps; for *perimeter* every junction
between collinear segments contributes two fictitious edges — and on sky130 the only stated
antenna limit is `DiffPSR`, whose numerator is perimeter × thickness. This is the leading
suspect for the remaining over-reporting.

**Diffusion is applied net-wide, not per stage.** The real limit moves as the path to diffusion
completes: OpenROAD's own report shows one pin requiring 400.00 on met1 and 3119.36 on met2.
Applying one net-wide diffusion area to every layer sets the bar wrong in both directions.

**Cut layers are not checked.** OpenROAD evaluates `mcon`/`via`/`via2` against their own ratios;
this engine evaluates routing layers only.

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
