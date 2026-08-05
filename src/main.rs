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
  vyges-ant --describe
  vyges-ant --help

OPTIONS:
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
      "NOT A SIGN-OFF GATE YET. Correlated against OpenROAD check_antennas on a routed sky130 block: it found 61 of 73 violating nets (84%) and added ~2400 it does not confirm. Useful as a screen; run check_antennas to gate a tapeout.",
      "Metal is attributed per layer across the whole net and charged to every gate on it. The real model attributes to each gate only the routing reachable from it over layers at or below the current one, so gates on separate branches are over-charged here — the main source of the false positives above.",
      "Diffusion area is applied net-wide, where the real limit varies per layer as the path to diffusion completes.",
      "Cut layers (mcon/via/via2) are not checked; routing layers only.",
      "Metal area is a raw rectangle sum, so overlapping shapes on a layer are double-counted; perimeter suffers worse than area because interior junction edges are counted too.",
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
  "consumes": [ { "role": "odb", "field": "odb" } ],
  "artifacts": [ { "role": "antenna_report", "field": "report_path" } ],
  "assertion": {
    "id": "antenna-clean",
    "field": "status",
    "pass_when": { "equals": "clean" }
  }
}
"#;

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
