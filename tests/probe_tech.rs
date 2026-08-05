// SPDX-License-Identifier: Apache-2.0
//! Probe: does the fixture's technology state antenna rules at all?
//!
//! Distinguishes two very different situations that look identical from a verdict:
//! the accessor is wired wrong, or the technology genuinely carries no rule. Only the
//! second is acceptable, and only if it is reported rather than blessed as clean.

use vyges_opendb::Db;

const FIXTURE: &str = "../vyges-tools-opendb-lib/test/fixtures/counter.odb";

#[test]
fn probe_antenna_rules_present() {
    let Ok(db) = Db::open(FIXTURE) else {
        eprintln!("fixture not present; skipping probe");
        return;
    };
    for l in ["met1", "met2", "met3", "li1", "mcon", "via", "via2"] {
        println!(
            "{l:6} default_rule={:5} oxide2_rule={:5} valid={:5} PAR={:8.3} CAR={:8.3} PSR={:8.3} CSR={:8.3} diff_area_factor={:.4}",
            db.layer_has_default_antenna_rule(l),
            db.layer_has_oxide2_antenna_rule(l),
            db.layerantenna_is_valid(l),
            db.layerantenna_get_p_a_r(l),
            db.layerantenna_get_c_a_r(l),
            db.layerantenna_get_p_s_r(l),
            db.layerantenna_get_c_s_r(l),
            db.layerantenna_get_area_factor(l),
        );
    }
}
