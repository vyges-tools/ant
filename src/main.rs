// SPDX-License-Identifier: Apache-2.0
//! `vyges-ant` CLI — antenna sign-off over a routed `.odb`.
//!
//! Exit status is the verdict: 0 clean, 1 violations, 2 usage/read error. A caller can gate on
//! the exit code without parsing prose, which is the contract every Loom engine keeps.

use std::process::ExitCode;
use vyges_ant::check_design;
use vyges_opendb::Db;

const USAGE: &str = "\
vyges-ant — antenna ratio sign-off over the routed design database

USAGE:
  vyges-ant check <design.odb> [-o FILE] [--json]
  vyges-ant explain <design.odb> --net NAME
  vyges-ant --describe
  vyges-ant --help

OPTIONS:
  --net NAME            (explain) dump one net's per-gate, per-stage attribution
  -o FILE               write the report to FILE instead of stdout
  --json                emit JSON (the default for `check`)
  --describe            print a machine-readable JSON description of the command

EXIT STATUS:
  0  clean          no violation found
  1  violations     at least one net exceeds a LEF antenna limit
  2  error          usage error, unreadable database, or no DBU scale
";

const DESCRIBE: &str = r#"{
  "schema": "vyges-tool-descriptor/1.1",
  "name": "ant",
  "summary": "antenna ratio sign-off (PAR/CAR/PSR/CSR) over the routed design database",
  "maturity": "structured",
  "provenance_limitations": [
      "input_hash covers the argument vector, not the content of the .odb it names.",
      "Both the plain and the diffusion-dependent (PWL) LEF ratio forms are read; where a technology states a diff curve it takes precedence, and outside the curve's stated range the limit is clamped rather than extrapolated.",
      "A ratio the technology states in neither form is not checked. On sky130 only DiffPSR is stated, so PAR, CAR and CSR are unlimited there; layers_without_rules and no_rules_found report this rather than leaving it implied by the exit code.",
      "Correlated against OpenROAD check_antennas on a routed sky130 block: 76 of 83 violations matched exactly, 1 unconfirmed, 76 of 77 compared values within 2%, and every shared record agreeing on the limit. 7 real violations are still missed, so this is not yet a formal sign-off gate -- run check_antennas before a tapeout -- but the two engines agree to the decimal on what they both see.",
      "The ratio is charged per CONDUCTOR: metal reachable from the gates over layers at or below the one being deposited, divided by the summed gate area of the gates on that conductor. Measured as the exact union of the rectangles, so overlap and abutment count once.",
      "The diffusion-dependent limit is indexed by each conductor's own diffusion, matching OpenROAD's per-node iterm_diff_area. Every terminal is anchored to a conductor, not only the gates, since a diode pin carries diffusion without carrying a gate.",
      "The conductor graph follows AntennaChecker: vias decomposed onto the layers they occupy, pin metal subtracted so pins cut the wire into antenna regions, components labelled per layer, layers joined through the cut between them, and terminals attached to the fragments their own pin boxes touch.",
      "Cut layers (mcon/via/via2) are not checked; routing layers only.",
      "Diffusion area is applied net-wide, where the real limit varies per layer as the path to diffusion completes.",
      "A terminal whose pin metal touches no routing is attached to nothing and, if it is a gate, counted in gates_unanchored rather than silently skipped.",
      "Layer accumulation order is dbTechLayer routing level, not a manufacturing step model."
  ],
  "invocation": {
    "args_template": ["check", "{odb}"],
    "optional": [ { "arg": "out", "flag": "-o" } ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "required": ["odb"],
    "properties": {
      "odb": { "type": "string", "description": "path to the routed design database (.odb)" },
      "out": { "type": "string", "description": "write the report to FILE instead of stdout" }
    }
  },
  "consumes": ["odb"],
  "artifacts": [ { "role": "antenna_report", "field": "report_path" } ],
  "assertion": {
    "id": "antenna-clean",
    "field": "status",
    "pass_when": { "eq": "clean" }
  }
}
"#;

/// Dump one net's attribution: every gate, and what metal it has collected at each stage.
///
/// A verdict says a net failed; this says why — which gate, on which layer, against how much
/// metal and how much gate area. It is also how the engine is correlated against another
/// checker, since disagreement is only actionable once both sides show their working.
fn explain(args: &[String]) -> ExitCode {
    let mut odb_path: Option<&str> = None;
    let mut net: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--net" => {
                i += 1;
                match args.get(i) {
                    Some(n) => net = Some(n),
                    None => {
                        eprintln!("vyges-ant: --net needs a NAME");
                        return ExitCode::from(2);
                    }
                }
            }
            a if a.starts_with('-') => {
                eprintln!("vyges-ant: unknown option `{a}`");
                return ExitCode::from(2);
            }
            a => odb_path = Some(a),
        }
        i += 1;
    }
    let (Some(odb_path), Some(net)) = (odb_path, net) else {
        eprintln!("vyges-ant: `explain` needs a .odb and --net NAME\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let db = match Db::open(odb_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("vyges-ant: cannot read {odb_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let dbu = db.dbu_per_micron() as f64;
    if dbu <= 0.0 {
        eprintln!("vyges-ant: no DBU scale");
        return ExitCode::from(2);
    }
    match vyges_ant::read_net(&db, net, dbu) {
        Some(na) => {
            println!("{}", serde_json::to_string_pretty(&na).unwrap_or_default());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("vyges-ant: {net} has no routed metal (or does not exist)");
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--describe") {
        print!("{DESCRIBE}");
        return ExitCode::SUCCESS;
    }
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return if args.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
    }
    if args[0] == "explain" {
        return explain(&args[1..]);
    }
    if args[0] != "check" {
        eprintln!("vyges-ant: unknown command `{}`\n\n{USAGE}", args[0]);
        return ExitCode::from(2);
    }

    let mut odb_path: Option<&str> = None;
    let mut out_path: Option<&str> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(p) => out_path = Some(p),
                    None => {
                        eprintln!("vyges-ant: -o needs a FILE");
                        return ExitCode::from(2);
                    }
                }
            }
            "--json" => {} // the only format today; accepted so callers can be explicit
            a if a.starts_with('-') => {
                eprintln!("vyges-ant: unknown option `{a}`");
                return ExitCode::from(2);
            }
            a => odb_path = Some(a),
        }
        i += 1;
    }

    let Some(odb_path) = odb_path else {
        eprintln!("vyges-ant: `check` needs a path to a .odb\n\n{USAGE}");
        return ExitCode::from(2);
    };

    let db = match Db::open(odb_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("vyges-ant: cannot read {odb_path}: {e}");
            return ExitCode::from(2);
        }
    };

    let report = check_design(&db);
    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("vyges-ant: cannot serialize report: {e}");
            return ExitCode::from(2);
        }
    };

    match out_path {
        Some(p) => {
            if let Err(e) = std::fs::write(p, format!("{json}\n")) {
                eprintln!("vyges-ant: cannot write {p}: {e}");
                return ExitCode::from(2);
            }
        }
        None => println!("{json}"),
    }

    // A vacuous pass is not a pass. If the technology carried no antenna rule at all there was
    // nothing to check, and reporting "clean" would be an assertion we never made.
    if report.no_rules_found {
        eprintln!(
            "vyges-ant: no layer in this technology states an antenna rule — \
             the verdict is vacuous, not clean"
        );
        return ExitCode::from(2);
    }
    if report.status == "error" {
        return ExitCode::from(2);
    }
    if report.count > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod describe_tests {
    use super::DESCRIBE;

    /// The descriptor must satisfy `vyges-tool-descriptor/1.1`, the schema baked into vyges-mcp.
    ///
    /// Both failures pinned here shipped in v0.1.28, because this engine was absent from
    /// verify-coverage.sh's list and nothing validated it:
    ///
    ///   * `consumes` held objects; the schema says an array of role STRINGS.
    ///   * `pass_when` used `equals`, which is not a predicate the schema defines. The cost is
    ///     spelled out in the schema itself — "an unrecognized spec is not a usable assertion and
    ///     is dropped, so the result resolves `unknown` rather than being guessed into a pass".
    ///     A malformed assertion does not fail loudly; the verdict silently stops existing.
    ///
    /// Checked here rather than only in the release gate so the loop closes at `cargo test`,
    /// where whoever edits the descriptor is standing.
    #[test]
    fn the_descriptor_matches_the_schema_contract() {
        let d: serde_json::Value = serde_json::from_str(DESCRIBE).expect("descriptor is valid JSON");

        assert_eq!(d["schema"], "vyges-tool-descriptor/1.1");

        // consumes: role strings, never objects
        let consumes = d["consumes"].as_array().expect("consumes is an array");
        assert!(
            consumes.iter().all(|c| c.is_string()),
            "consumes must be role STRINGS, got {consumes:?}"
        );

        // assertion: exactly one recognised predicate
        let pw = d["assertion"]["pass_when"]
            .as_object()
            .expect("pass_when is an object");
        assert_eq!(pw.len(), 1, "exactly one predicate, got {pw:?}");
        let key = pw.keys().next().unwrap().as_str();
        assert!(
            matches!(key, "is_true" | "eq" | "lte"),
            "`{key}` is not a predicate the schema defines (is_true | eq | lte) — an \
             unrecognised one is dropped and the verdict resolves `unknown`"
        );

        // the field the assertion reads, and the values it is asserted against, must be real
        assert_eq!(d["assertion"]["field"], "status");
        assert_eq!(
            pw["eq"], "clean",
            "Report::status is \"clean\" or \"violations\" (see lib.rs)"
        );
        let limits = d["provenance_limitations"]
            .as_array()
            .expect("provenance_limitations is an array");
        assert!(!limits.is_empty(), "the schema requires provenance_limitations");
    }
}
