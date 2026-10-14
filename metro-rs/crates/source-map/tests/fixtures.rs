//! Runs the language-neutral fixtures in `port/blackbox/fixtures/source-map.json`
//! (the Jest runner `port/blackbox/__tests__/source-map-fixtures-test.js` reads
//! the same file). The expectations were recorded from the JavaScript
//! implementation.
//!
//! The crate does the numeric part of each operation: sources, contents and
//! metadata stay in JavaScript, so the Rust side compares `mappings` and
//! `names` (and positions) only. A case that JavaScript rejects must be
//! rejected here too, or be one that the crate leaves to JavaScript.

use metro_rs_ob1::{add0, add1};
use metro_rs_source_map::compose::{compose, ComposeMap};
use metro_rs_source_map::consumer::ColumnsRef;
use metro_rs_source_map::decode::{
    decode_columns, Columns, NAME, NONE, ORIGINAL_COLUMN, ORIGINAL_LINE, SOURCE, STRIDE,
};
use metro_rs_source_map::encode::{
    Batch, Encoder, INVALID, MODULE_TUPLES, MODULE_UNEXPECTED, MODULE_VLQ, NO_NAME, NO_NAME_SIMPLE,
    TUPLE_STRIDE,
};
use serde_json::Value;
use std::collections::HashMap;

fn fixtures() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../port/blackbox/fixtures/source-map.json"
    );
    let text = std::fs::read_to_string(path).expect("fixtures file");
    serde_json::from_str(&text).expect("fixtures are JSON")
}

fn cases<'a>(data: &'a Value, kind: &str) -> &'a Vec<Value> {
    data[kind].as_array().expect(kind)
}

fn name_of(case: &Value) -> &str {
    case["name"].as_str().unwrap()
}

/// Number of lines of `code` the way JavaScript counts them.
fn count_lines(code: &str) -> i32 {
    let mut lines = 1;
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines += 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => lines += 1,
            _ => {}
        }
    }
    lines
}

/// Flattens the tuples of one module the way the JavaScript side does.
fn push_tuples(
    map: &[Value],
    tuples: &mut Vec<i32>,
    names: &mut Vec<String>,
    index: &mut HashMap<String, i32>,
) -> bool {
    for mapping in map {
        let m = mapping.as_array().unwrap();
        let n = |i: usize| m[i].as_i64().unwrap() as i32;
        match m.len() {
            2 => tuples.extend([n(0), n(1), 0, 0, NO_NAME_SIMPLE]),
            4 => tuples.extend([n(0), n(1), n(2), n(3), NO_NAME]),
            5 => {
                let Some(name) = m[4].as_str() else {
                    return false;
                };
                let next = names.len() as i32;
                let id = *index.entry(name.to_string()).or_insert_with(|| {
                    names.push(name.to_string());
                    next
                });
                tuples.extend([n(0), n(1), n(2), n(3), id]);
            }
            _ => tuples.extend([0, 0, 0, 0, INVALID]),
        }
    }
    true
}

fn encoded_names(encoder: &Encoder) -> Vec<String> {
    encoder.generator().names().to_vec()
}

fn run_from_raw_mappings(kind: &str) {
    let data = fixtures();
    for case in cases(&data, kind) {
        let name = name_of(case);
        let mut tuples = Vec::new();
        let mut names = Vec::new();
        let mut index = HashMap::new();
        let mut modules = Vec::new();
        let mut vlq_mappings: Vec<String> = Vec::new();
        let mut vlq_names: Vec<Vec<String>> = Vec::new();
        let mut carry_over = case["offsetLines"].as_i64().unwrap_or(0) as i32;
        let mut supported = true;
        for module in case["modules"].as_array().unwrap() {
            let map = &module["map"];
            if let Some(list) = map.as_array() {
                let start = (tuples.len() / TUPLE_STRIDE) as i32;
                supported &= push_tuples(list, &mut tuples, &mut names, &mut index);
                let end = (tuples.len() / TUPLE_STRIDE) as i32;
                modules.extend([MODULE_TUPLES, start, end, carry_over]);
            } else if map.is_object() && map["mappings"].is_string() {
                modules.extend([MODULE_VLQ, vlq_mappings.len() as i32, 0, carry_over]);
                vlq_mappings.push(map["mappings"].as_str().unwrap().to_string());
                vlq_names.push(
                    map["names"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_str().unwrap().to_string())
                        .collect(),
                );
            } else if !map.is_null() {
                modules.extend([MODULE_UNEXPECTED, 0, 0, 0]);
            }
            carry_over += count_lines(module["code"].as_str().unwrap());
        }
        if !supported {
            continue;
        }
        let batch = Batch {
            tuples: &tuples,
            names: &names,
            modules: &modules,
            vlq_mappings: &vlq_mappings,
            vlq_names: &vlq_names,
        };
        let mut encoder = Encoder::new();
        let result = encoder.add_batch(&batch);
        if case.get("expectError").is_some() {
            assert!(result.is_err(), "{kind}: {name}: JavaScript throws");
        } else if result.is_err() {
            // Left to JavaScript (a VLQ module the crate does not take).
            continue;
        } else {
            let map = &case["expect"]["map"];
            let mappings = String::from_utf8(encoder.generator().mappings().to_vec()).unwrap();
            assert_eq!(
                mappings,
                map["mappings"].as_str().unwrap(),
                "{kind}: {name}"
            );
            let expected: Vec<&str> = map["names"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap())
                .collect();
            assert_eq!(encoded_names(&encoder), expected, "{kind}: {name}");
        }
    }
}

#[test]
fn from_raw_mappings() {
    run_from_raw_mappings("fromRawMappings");
}

#[test]
fn vlq_map_from_tuples() {
    let data = fixtures();
    for case in cases(&data, "vlqMapFromTuples") {
        let name = name_of(case);
        let mut tuples = Vec::new();
        let mut names = Vec::new();
        let mut index = HashMap::new();
        let list = case["tuples"].as_array().unwrap();
        assert!(push_tuples(list, &mut tuples, &mut names, &mut index));
        let mut encoder = Encoder::new();
        encoder.reset_name_table(names.len());
        let result = encoder.add_tuples(&tuples, &names, 0);
        if case.get("expectError").is_some() {
            assert!(result.is_err(), "{name}: JavaScript throws");
        } else {
            result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let expect = &case["expect"];
            let mappings = String::from_utf8(encoder.generator().mappings().to_vec()).unwrap();
            assert_eq!(mappings, expect["mappings"].as_str().unwrap(), "{name}");
            let expected: Vec<&str> = expect["names"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap())
                .collect();
            assert_eq!(encoded_names(&encoder), expected, "{name}");
        }
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn consumer() {
    let data = fixtures();
    for case in cases(&data, "consumer") {
        let name = name_of(case);
        let map = &case["map"];
        if map.get("sections").is_some() || map["version"] != 3 {
            continue; // index maps and versions are handled in JavaScript
        }
        let sources = map["sources"].as_array().unwrap().len() as i32;
        let names = strings(&map["names"]);
        let decoded = decode_columns(map["mappings"].as_str().unwrap().as_bytes());
        if case.get("expectError").is_some() {
            assert!(decoded.is_err(), "{name}: JavaScript throws");
            continue;
        }
        let columns = decoded.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let in_range = |c: &Columns| {
            c.data.chunks(STRIDE).all(|m| {
                (m[SOURCE] == NONE || (0..sources).contains(&m[SOURCE]))
                    && (m[NAME] == NONE || (0..names.len() as i32).contains(&m[NAME]))
            })
        };
        if !in_range(&columns) {
            continue; // JavaScript reads undefined entries; not part of the crate
        }
        let expect = &case["expect"];
        let generated = expect["generatedMappings"].as_array().unwrap();
        assert_eq!(generated.len(), columns.len(), "{name}: mapping count");
        for (i, (m, e)) in columns.data.chunks(STRIDE).zip(generated).enumerate() {
            assert_eq!(
                e["generatedColumn"].as_i64().unwrap() as i32,
                m[0],
                "{name}: mapping {i} column"
            );
            assert_eq!(
                e["source"].is_null(),
                m[SOURCE] == NONE,
                "{name}: {i} source"
            );
            if m[SOURCE] != NONE {
                assert_eq!(e["originalLine"].as_i64().unwrap() as i32, m[ORIGINAL_LINE]);
                assert_eq!(
                    e["originalColumn"].as_i64().unwrap() as i32,
                    m[ORIGINAL_COLUMN]
                );
            }
            match e["name"].as_str() {
                Some(n) => assert_eq!(names[m[NAME] as usize], n, "{name}: {i} name"),
                None => assert_eq!(m[NAME], NONE, "{name}: {i} name"),
            }
        }
        let view = ColumnsRef {
            data: &columns.data,
            line_starts: &columns.line_starts,
        };
        let lookups = case["lookups"].as_array().unwrap();
        let expected = expect["lookups"].as_array().unwrap();
        for (l, e) in lookups.iter().zip(expected) {
            let (ln, col) = (
                l["line"].as_i64().unwrap() as i32,
                l["column"].as_i64().unwrap() as i32,
            );
            let found = view.original_position_for(add1(ln - 1), add0(col));
            match found {
                Some(i) if view.has_source(i) => {
                    assert_eq!(
                        e["line"].as_i64().unwrap() as i32,
                        view.field(i, ORIGINAL_LINE)
                    );
                    assert_eq!(
                        e["column"].as_i64().unwrap() as i32,
                        view.field(i, ORIGINAL_COLUMN)
                    );
                    assert_eq!(
                        e["name"].is_null(),
                        !view.has_name(i),
                        "{name}: lookup name"
                    );
                }
                _ => assert!(e["source"].is_null(), "{name}: lookup {l} finds nothing"),
            }
        }
    }
}

#[test]
fn compose_source_maps() {
    let data = fixtures();
    for case in cases(&data, "composeSourceMaps") {
        let name = name_of(case);
        if case.get("expectError").is_some() {
            continue; // argument checks happen in JavaScript
        }
        let maps = case["maps"].as_array().unwrap();
        if maps.iter().any(|m| m.get("sections").is_some()) {
            continue;
        }
        // Intern sources and names the way composeSourceMaps.js does.
        let mut global_sources: Vec<String> = Vec::new();
        let mut global_names: Vec<String> = Vec::new();
        let mut source_ids = HashMap::new();
        let mut name_ids = HashMap::new();
        let mut decoded = Vec::new();
        let mut tables = Vec::new();
        for map in maps {
            let columns = decode_columns(map["mappings"].as_str().unwrap().as_bytes())
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let sources: Vec<i32> = strings(&map["sources"])
                .into_iter()
                .map(|s| {
                    if s.is_empty() {
                        -1
                    } else {
                        let next = global_sources.len() as i32;
                        *source_ids.entry(s.clone()).or_insert_with(|| {
                            global_sources.push(s);
                            next
                        })
                    }
                })
                .collect();
            let names: Vec<i32> = strings(&map["names"])
                .into_iter()
                .map(|s| {
                    let next = global_names.len() as i32;
                    *name_ids.entry(s.clone()).or_insert_with(|| {
                        global_names.push(s);
                        next
                    })
                })
                .collect();
            decoded.push(columns);
            tables.push((sources, names));
        }
        // composeSourceMaps puts the last map first.
        let list: Vec<ComposeMap> = decoded
            .iter()
            .zip(&tables)
            .rev()
            .map(|(c, (s, n))| ComposeMap {
                columns: ColumnsRef {
                    data: &c.data,
                    line_starts: &c.line_starts,
                },
                sources: s,
                names: n,
            })
            .collect();
        let Ok(composed) = compose(&list) else {
            continue; // left to JavaScript
        };
        let expect = &case["expect"];
        assert_eq!(
            String::from_utf8(composed.mappings).unwrap(),
            expect["mappings"].as_str().unwrap(),
            "{name}: mappings"
        );
        let used_sources: Vec<&str> = composed
            .sources
            .iter()
            .map(|&i| global_sources[i as usize].as_str())
            .collect();
        let used_names: Vec<&str> = composed
            .names
            .iter()
            .map(|&i| global_names[i as usize].as_str())
            .collect();
        let want = |key: &str| -> Vec<String> { strings(&expect[key]) };
        assert_eq!(used_names, want("names"), "{name}: names");
        assert_eq!(used_sources, want("sources"), "{name}: sources");
    }
}
