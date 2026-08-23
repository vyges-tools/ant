# vyges-ant

Antenna ratio sign-off over the **routed** design database. Reads a `.odb`, computes PAR / CAR /
PSR / CSR per net per routing layer against the limits the LEF states, and emits a verdict.

```sh
vyges-ant check routed.odb          # 0 clean · 1 violations · 2 vacuous or error
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

Correlated against OpenROAD `check_antennas`, **re-measured 2026-08-23** against a freshly
generated reference on a build carrying OpenROAD PR #11125.

⚠️ **A number here means nothing without the build and the database that produced it** — the
reference's own answer moves between OpenROAD builds. Both are named:

| | |
| --- | --- |
| reference | `check_antennas` at OpenROAD `945a9f4` |
| engine | `vyges-ant` `802e66b` |
| database | a **detail-routed** sky130 block, 10918 nets — 9677 checked, 751 with no gate, 490 unrouted |

| | violations | matched | missed | added | values within 2% |
| --- | ---: | ---: | ---: | ---: | ---: |
| `check_antennas` @ `945a9f4` | 44 | **43** | 1 | **0** | **43 of 43** |

Both sides are deterministic: three engine runs and two reference runs on the same `.odb` return
byte-identical output, so these are measurements rather than one draw each.

**One missed violation, and no false positives.** An earlier measurement (2026-08-07, against a
build predating #11125) showed 10 violations this engine reported that the reference did not, and
argued they were inherited from a stale reference rather than generated here. Against a current
reference there are none.

⚠️ **Give it a detail-routed database.** On a global-route `.odb` OpenROAD synthesises wires from
the routing guides; this engine reads the routed database, finds no routing, and refuses the
verdict as vacuous rather than reporting clean. The two are not checking the same thing there.

**Treat this as a strong screen, not a sign-off gate.** Run `check_antennas` for sign-off, and if
the two disagree, check which build you are comparing against before assuming either is wrong.
The engine is deterministic — the same `.odb` returns a byte-identical violation set across runs
— which was **not** true before 2026-08-06.

### What is still divergent

**A single missed violation**, on `met3` PSR, where the reference measures 5192.82 against a limit
of 2773.88 and this engine reports nothing. Every matched value agrees within 2% — 43 of 43 — so
the standing difference is one violation found, not a spread of values.

**Cut layers are not evaluated.** OpenROAD checks `mcon`/`via`/`via2` against their own ratios
(`calculateViaPar`); this engine builds cut-layer geometry, because connectivity needs it, but
only evaluates routing layers. No violation in the golden report is on a cut layer.

**CAR/CSR composition.** OpenROAD keeps separate cumulative chains for wires and vias. Not
exercised by sky130, which states no cumulative limit.

### One OpenROAD behaviour reproduced deliberately

When a gate belongs to several conductors on one layer, OpenROAD sums their ratios but **not**
their diffusion areas — `NodeInfo::operator+=` accumulates six fields and leaves
`iterm_gate_area` and `iterm_diff_area` behind — so the PWL limit is indexed by whichever
conductor was recorded first. This engine reproduces that, because a checker that disagrees with
the incumbent is not usable as a cross-check.

Whether it is intended is an open question upstream:
**[OpenROAD #11082](https://github.com/The-OpenROAD-Project/OpenROAD/issues/11082)**. If it turns
out to be deliberate this note becomes the documentation; if not, this engine follows the fix.

### How the model was arrived at

Worth stating because two plausible models were wrong first:

1. **Sum gate areas across the whole net.** Hid 68 of 73 violating nets — the denominator was
   inflated by the gate count.
2. **Charge each gate its own area.** Over-reported by the gate count instead: exactly 2.00× on
   two-gate regions, which is what gave the model away.
3. **Charge each conductor, denominator summed over the gates on it.** Confirmed by reading
   `AntennaChecker.cc`, and checked numerically first: region metal of 1603.33 µm² over its
   three gates (0.4347 + 0.4347 + 0.126 = 0.9954) gives 1610.7 against OpenROAD's 1611.2.
4. **Index the limit by each conductor's own diffusion, not the net's total.** A net-wide total
   is never smaller, so the bar sat too high and real violations slipped under. Fixing it took
   exact matches from 37 to 65 and removed *every* limit disagreement.
5. **Implement the exact factor and diffusion-branch formulas.** Changed the measured result by
   nothing on sky130, where every factor is identity — which is how we learned the residual was
   not in the model at all.

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
which the reference confirms, all 44 of its violations being PSR (`Side area`).

## Verdicts that are not verdicts

Three cases are deliberately **not** reported as clean, because nothing was actually checked. The
first two set `"status": "vacuous"` and exit 2:

- **No antenna rules in the technology** (`no_rules_found`) — a design whose LEF states no antenna
  limits has not passed anything.
- **No routed metal in the database** (`no_routing_found`) — usually a **global-route** `.odb`
  handed to a checker that reads routed geometry. OpenROAD synthesises wires from the routing
  guides there; this engine does not, so it finds nothing and says so rather than reporting clean.
- **A net with no gate area** — counted in `nets_no_gate`, not passed. With no denominator there
  is no ratio. A large count here means the library's antenna models are missing and the check is
  covering less than it appears to, which is why the number is in the report rather than swallowed.

🔑 **`vacuous` is not `clean`, and that distinction is the point of the status field.** A run that
consulted no rule found no violation, so a two-valued status would report it as a pass — and the
descriptor's declared assertion (`status == "clean"`) would pass with it. That is the one way an
unverified design could be signed off by a machine, so vacuity outranks the violation count.

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
  "no_routing_found": false,
  "violations": []
}
```

## Building

Reads the database through [`vyges-opendb`](https://github.com/vyges-tools/opendb), which binds
OpenROAD's OpenDB (`libodb`). A first build compiles libodb and takes a while; later builds do not.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
