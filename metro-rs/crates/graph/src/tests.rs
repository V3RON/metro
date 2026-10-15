//! Tests of the internals. The behaviour is pinned by the JavaScript suites
//! (Graph-test, the black-box tests) and by `METRO_RUST_VERIFY=delta-core`,
//! which compares every traversal with the JavaScript implementation; these
//! cover what has no JavaScript equivalent: the consistency of the state, the
//! stack depth limit and the wire format.

use crate::wire::{self, InEdge, Input, UpdatedModule};
use crate::{Core, Id, ASY_NONE, NONE};
use std::collections::HashMap;

fn key(from: Id, to: Id) -> u32 {
    from * 1000 + to
}

fn module(id: Id, deps: &[Id]) -> UpdatedModule {
    UpdatedModule {
        id,
        key_null: false,
        key_eq: false,
        edges: deps
            .iter()
            .map(|&d| InEdge {
                key: key(id, d),
                target: d,
                asy: ASY_NONE,
                sig: 0,
                has_ctx: false,
            })
            .collect(),
    }
}

fn input(modified: &[Id], errors: &[Id], updated: Vec<UpdatedModule>) -> Input {
    Input {
        lazy: false,
        shallow: false,
        modified: modified.to_vec(),
        errors: errors.to_vec(),
        updated,
    }
}

/// Every edge of a module in the graph has its inverse dependency, and the
/// other way around, and the roots and colors only name modules in the graph.
fn check(core: &Core) {
    let mut expected: HashMap<Id, HashMap<Id, u32>> = HashMap::new();
    for (path, &o) in core.modules.iter() {
        let ob = core.obj(o);
        assert_eq!(ob.path, path);
        expected.entry(path).or_default();
        for (_, e) in ob.edges.iter() {
            if e.target != NONE && e.asy == ASY_NONE {
                *expected
                    .entry(e.target)
                    .or_default()
                    .entry(path)
                    .or_default() += 1;
            }
        }
    }
    for (path, &o) in core.modules.iter() {
        let actual: HashMap<Id, u32> = core
            .inv(core.obj(o).inv)
            .map
            .iter()
            .map(|(k, v)| (k, *v))
            .collect();
        assert_eq!(actual, expected[&path], "inverse dependencies of {path}");
    }
    for (root, _) in core.roots.iter() {
        assert!(core.modules.contains(&root));
    }
}

fn status(out: &[u32]) -> u32 {
    out[0]
}

#[test]
fn builds_a_graph_and_releases_what_is_no_longer_referenced() {
    let mut core = Core::new(vec![0]);
    let out = core.initial(&input(
        &[0],
        &[],
        vec![
            module(0, &[1, 2]),
            module(1, &[3]),
            module(2, &[3]),
            module(3, &[]),
        ],
    ));
    assert_eq!(status(&out), wire::STATUS_OK);
    assert_eq!(core.module_count(), 4);
    check(&core);

    // 0 stops importing 1 and 2: both are released, and 3 with them.
    let out = core.incremental(&input(&[0], &[], vec![module(0, &[])]));
    assert_eq!(status(&out), wire::STATUS_OK);
    assert_eq!(core.module_count(), 1);
    check(&core);
    assert!(core.deleted.contains(&1) && core.deleted.contains(&2) && core.deleted.contains(&3));
}

#[test]
fn collects_a_cycle_that_lost_its_last_reference() {
    let mut core = Core::new(vec![0]);
    core.initial(&input(
        &[0],
        &[],
        vec![module(0, &[1]), module(1, &[2]), module(2, &[1])],
    ));
    check(&core);
    let out = core.incremental(&input(&[0], &[], vec![module(0, &[])]));
    assert_eq!(status(&out), wire::STATUS_OK);
    assert_eq!(core.module_count(), 1);
    check(&core);
}

#[test]
fn keeps_a_cycle_that_is_still_referenced() {
    let mut core = Core::new(vec![0]);
    core.initial(&input(
        &[0],
        &[],
        vec![
            module(0, &[1, 3]),
            module(1, &[2]),
            module(2, &[1]),
            module(3, &[2]),
        ],
    ));
    let out = core.incremental(&input(&[0], &[], vec![module(0, &[3])]));
    assert_eq!(status(&out), wire::STATUS_OK);
    assert_eq!(core.module_count(), 4);
    check(&core);
}

#[test]
fn rolls_back_when_an_added_module_has_an_error() {
    let mut core = Core::new(vec![0]);
    core.initial(&input(&[0], &[], vec![module(0, &[1]), module(1, &[])]));
    let before: Vec<Id> = core.modules.iter().map(|(k, _)| k).collect();
    // 0 now imports 1 and 5, and 5 fails.
    let out = core.incremental(&input(&[0], &[5], vec![module(0, &[1, 5])]));
    assert_eq!(status(&out), wire::STATUS_MODULE);
    assert_eq!(out[1], 5);
    assert_eq!(out[2], 1, "the rolled back state is returned");
    let after: Vec<Id> = core.modules.iter().map(|(k, _)| k).collect();
    assert_eq!(before, after);
    check(&core);
    assert_eq!(core.obj(*core.modules.get(&0).unwrap()).edges.len(), 1);
}

#[test]
fn a_deep_chain_works_and_a_chain_beyond_the_limit_fails_instead_of_overflowing() {
    // The caller's stack is small (a worker thread's); traversals bring their own.
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(|| {
            for depth in [7000u32, 60000] {
                let mut core = Core::new(vec![0]);
                let modules = (0..depth)
                    .map(|i| {
                        let deps: Vec<Id> = if i + 1 < depth { vec![i + 1] } else { vec![] };
                        module(i, &deps)
                    })
                    .collect();
                let out = core.initial(&input(&[0], &[], modules));
                if depth <= 7000 {
                    assert_eq!(status(&out), wire::STATUS_OK);
                } else {
                    assert_eq!(status(&out), wire::STATUS_DEPTH);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn decodes_the_input_layout() {
    let words = [
        1, // lazy
        2,
        7,
        8, // modified
        1,
        9, // errors
        1, // one updated module
        7,
        2,
        2, // id, flags, two edges
        10,
        8,
        0,
        0,
        0, //
        11,
        u32::MAX,
        2,
        5,
        1,
    ];
    let input = wire::decode_input(&words).unwrap();
    assert!(input.lazy && !input.shallow);
    assert_eq!(input.modified, [7, 8]);
    assert_eq!(input.errors, [9]);
    let m = &input.updated[0];
    assert!(!m.key_null && m.key_eq);
    assert_eq!(m.edges[1].target, NONE);
    assert!(m.edges[1].has_ctx);
    assert!(wire::decode_input(&words[..words.len() - 1]).is_none());
}
