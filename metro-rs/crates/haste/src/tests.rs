//! Unit tests. Expected values come from running the real JS `HastePlugin`
//! (`oracle/gen.js` writes `oracle_data.rs`).

use crate::collate::locale_compare;
use crate::oracle_data::{Expect, Scenario, SCENARIOS};
use crate::*;
use std::collections::HashSet;

fn make(s: &Scenario) -> HasteMap {
    HasteMap::new(HasteOptions {
        enable_haste_packages: s.enable_packages,
        platforms: s.platforms.iter().map(|p| p.to_string()).collect(),
        root_dir: s.root.to_string(),
        fail_validation_on_conflicts: s.fail,
    })
}

fn to_vec(v: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    v.iter()
        .map(|(p, i)| (p.to_string(), i.map(str::to_string)))
        .collect()
}

fn check_state(scn: &Scenario, map: &mut HasteMap, idx: usize) {
    let st = &scn.states[idx];
    let at = format!("{} state {idx}", scn.name);
    for q in st.queries {
        let (name, platform, native, kind, expect) = (q.0, q.1, q.2, q.3, &q.4);
        let got = match kind {
            0 => map.get_module(name, platform, native, None),
            1 => map.get_module(name, platform, native, Some(ModuleType::Package)),
            _ => map.get_package(name, platform, native),
        };
        let ctx = format!("{at}: {name} {platform:?} native={native} kind={kind}");
        match (expect, got) {
            (Expect::None, Ok(None)) => {}
            (Expect::Path(p), Ok(Some(g))) => assert_eq!(*p, g, "{ctx}"),
            (
                Expect::Dup(plat, supports, set, message),
                Err(HasteError::DuplicateHasteCandidates(e)),
            ) => {
                assert_eq!(e.haste_name, name, "{ctx}");
                assert_eq!(e.platform.as_deref(), Some(*plat), "{ctx}");
                assert_eq!(e.supports_native_platform, *supports, "{ctx}");
                let got_set: Vec<(&str, u8)> = e
                    .duplicates_set
                    .iter()
                    .map(|(p, t)| (p.as_str(), *t as u8))
                    .collect();
                assert_eq!(&got_set[..], *set, "{ctx}");
                assert_eq!(e.message, *message, "{ctx}");
            }
            (_, got) => panic!("{ctx}: expected a different result shape, got {got:?}"),
        }
    }

    let conflicts = map.compute_conflicts();
    assert_eq!(
        conflicts.len(),
        st.conflicts.len(),
        "{at}: conflicts {conflicts:?}"
    );
    for (g, e) in conflicts.iter().zip(st.conflicts) {
        assert_eq!(g.id, e.0, "{at}");
        assert_eq!(g.platform.as_deref(), e.1, "{at}");
        assert_eq!(g.r#type.as_str(), e.2, "{at}");
        assert_eq!(g.absolute_paths, e.3, "{at}");
    }

    match (map.assert_valid(), &st.valid) {
        (Ok(()), None) => {}
        (Err(HasteError::HasteConflicts(e)), Some(exp)) => {
            assert_eq!(e.message, exp.0, "{at}");
            assert_eq!(e.get_detailed_message(None), exp.1, "{at}");
            assert_eq!(e.get_detailed_message(Some(scn.root)), exp.2, "{at}");
        }
        (got, exp) => panic!(
            "{at}: assert_valid got {got:?}, expected invalid={}",
            exp.is_some()
        ),
    }

    for (p, expected) in st.names {
        assert_eq!(
            map.get_module_name_by_path(p).as_deref(),
            *expected,
            "{at}: name of {p}"
        );
    }

    assert_eq!(map.take_warnings(), st.warnings, "{at}: warnings");
}

fn run_scenario(scn: &Scenario) {
    let mut map = make(scn);
    map.initialize(scn.initial.iter().copied());
    check_state(scn, &mut map, 0);
    for (i, step) in scn.steps.iter().enumerate() {
        map.on_changed(&HasteDelta {
            added_files: to_vec(step.added),
            modified_files: to_vec(step.modified),
            removed_files: to_vec(step.removed),
        });
        check_state(scn, &mut map, i + 1);
    }
}

fn scenario(name: &str) -> &'static Scenario {
    SCENARIOS.iter().find(|s| s.name == name).expect(name)
}

#[test]
fn platforms() {
    run_scenario(scenario("platforms"));
}

#[test]
fn no_native_platform() {
    run_scenario(scenario("no-native-platform"));
}

#[test]
fn packages() {
    run_scenario(scenario("packages"));
}

#[test]
fn packages_disabled() {
    run_scenario(scenario("packages-disabled"));
}

#[test]
fn duplicates_and_recovery() {
    run_scenario(scenario("duplicates-recovery"));
}

#[test]
fn conflicts_and_assert_valid() {
    run_scenario(scenario("conflicts"));
}

#[test]
fn conflicts_without_fail_validation() {
    run_scenario(scenario("conflicts-not-failing"));
}

#[test]
fn paths_outside_root() {
    run_scenario(scenario("outside-root"));
}

#[test]
fn change_edge_cases() {
    run_scenario(scenario("changes-edge-cases"));
}

#[test]
fn every_scenario_is_covered() {
    let names: HashSet<&str> = SCENARIOS.iter().map(|s| s.name).collect();
    assert_eq!(names.len(), 9);
    for s in SCENARIOS {
        assert_eq!(s.states.len(), s.steps.len() + 1, "{}", s.name);
    }
}

#[test]
fn platform_extensions() {
    let platforms: HashSet<String> = ["ios", "android", "native"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (file, expected) in crate::oracle_data::PLATFORM_EXTENSIONS {
        assert_eq!(
            get_platform_extension(file, &platforms),
            *expected,
            "{file:?}"
        );
    }
}

#[test]
fn collation_matches_locale_compare() {
    let sorted = crate::oracle_data::COLLATION_SORTED;
    let mut shuffled: Vec<&str> = sorted.iter().rev().copied().collect();
    shuffled.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(shuffled, sorted);
}

#[test]
#[should_panic(expected = "has not been initialized")]
fn name_by_path_requires_initialize() {
    let map = HasteMap::new(HasteOptions {
        enable_haste_packages: false,
        platforms: vec![],
        root_dir: "/root".to_string(),
        fail_validation_on_conflicts: false,
    });
    map.get_module_name_by_path("a.js");
}
