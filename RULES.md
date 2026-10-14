# Rules for the Metro to Rust port

Every agent and person working on the Rust port follows these rules. The plan behind them (component inventory, port order, test review, performance study) is in the port study artifact; the machine-readable phase plan is `port/phases.json`.

The short version: work on one phase at a time, run `./gate.sh` until it passes, produce the phase report with `./gate.sh --report`, open a PR, and stop. Szymon decides whether the next phase starts.

## 1. The gate decides when work is done

`./gate.sh` is the only definition of "done". Work is not finished while it fails, whatever else looks green.

The gates run in this order and stop at the first failure:

| Gate | What it runs | Fails when |
|---|---|---|
| 1. compile | `cargo fmt --check`, `cargo clippy -D warnings`, the napi-rs build with the fast `dev-fast` profile (`yarn --cwd metro-rs build:fast`), a check that the binding loads in Node, `flow check`, `eslint` | Any compiler, type or lint error. Fix what the compiler says; never silence a warning with `allow` without a comment explaining why. |
| 2. tests | `cargo test`; the full Jest suite with `METRO_RUST=off`; the full Jest suite with the phase's components in Rust (minus the JS-only list); the differential bundle check | Any failing test, or any byte of difference between JS-built and Rust-built bundles or source maps. |
| 3. perf | the release napi build (`yarn --cwd metro-rs build`, LTO), then `port/bench/compare.js`: pure-JS Metro at `baselineRef` against this checkout, same machine, interleaved runs | A scenario misses the phase's target, regresses beyond tolerance (5% or the measured noise, whichever is larger), or peak memory grows over 10%. |

Usage:

```sh
./gate.sh                 # all gates for the current phase (the first one not yet approved)
./gate.sh --quick         # inner loop: one perf run per scenario, no cold builds once caches exist
./gate.sh --only compile  # one gate: compile, tests or perf
./gate.sh --report        # full run with 5 perf runs; writes port/reports/phase-<n>.md
```

Exit codes: `0` passed, `10` compile, `20` tests, `30` perf, `2` blocked (for example, the previous phase is not approved). On failure the gate prints the end of the failing step's output. Full logs are in `port/.gate/<gate>.log` and a summary is in `port/.gate/result.json`.

Use `--quick` and `--only` while iterating. A phase is only complete after a full `./gate.sh --report` on a clean, committed tree.

## 2. Phases and approval

Phases are listed in `port/phases.json`, in this order:

0. Foundations
1. Leaf libraries
2. Resolver
3. File map and glue
4. Incremental graph
5. Output and server
6. Optional transform pass

Each phase lists its components, its exit criteria and its performance targets.

- **Work only on the current phase.** It is the first phase without an approval file, as reported by `node port/lib/phase.js current`. The gate refuses to run a later phase (exit code 2).
- **Finish a phase this way:**
  1. Run `./gate.sh --report`.
  2. Commit `port/reports/phase-<n>.md` and `.json`.
  3. Open a PR titled `Phase <n>: <title>` with the report as its description.
  4. Stop. Do not start the next phase, not even "just the scaffolding".
- **Only Szymon approves.** He approves phase n by adding `port/approvals/phase-<n>.md`, which states whether the phase is approved and any conditions. Agents never create, edit or delete files in `port/approvals/`.
- **Agents never change the yardstick.** They do not edit `baselineRef`, `maxRegression` or `perf` in `port/phases.json`, `port/js-only-tests.json`, the bench app in `port/bench/app/`, or the scenarios in `port/bench/run.js`. If one of them looks wrong, say so in the PR and let Szymon decide.

## 3. Tests: written first, in JavaScript, run against both sides

**Do not rewrite a component's tests in Rust before porting it.** Parity is proven by running the *same* tests against both implementations. A Rust translation of a JavaScript test is a second reading of the spec. If the translated test and the Rust code share a misunderstanding, both pass while behaviour has changed, and nothing catches it.

For each component, in this order:

1. **Black-box tests exist and pass on JS first.** They call only the component's public API or Metro's public API and use real temporary directories. They never use `jest.mock('fs')`, metro-memory-fs, mocked sibling modules or private methods.
   - 118 of Metro's 149 test files already qualify, and `port/blackbox/` adds seven suites (119 tests).
   - If the component has gaps, write the missing tests in JavaScript, make them pass against the JavaScript implementation, and commit them *before* writing Rust.
2. **Pure components get language-neutral fixtures.** Put their cases in `port/blackbox/fixtures/<component>.json`, with input and expected output as JSON, plus a Jest runner. The resolver already works this way (`resolver.json`, `UPDATE_FIXTURES=1` records cases from JS).
3. **The Rust crate reads the same fixtures in `cargo test`.** This gives a fast inner loop without Node. It also has its own unit tests for internals that have no JS equivalent.
4. **Implement the component behind the switch.** The gate then runs the whole Jest suite with that component in Rust.

The files in `port/js-only-tests.json` (31 files) only test the JavaScript implementation, because they mock `fs` or sibling modules, or call private methods. They are skipped in Rust mode only while one of their listed components is in Rust. Do not add entries: if a test fails in Rust mode, fix the Rust code. If you believe a test is genuinely implementation-specific, explain why in the PR and leave the decision to Szymon.

Never skip, delete, weaken or `.only` a test to get green. Never update a snapshot or fixture to match Rust output. If JS and Rust disagree, JS is right unless Szymon decides otherwise. Behaviours the black-box suites pin as quirks stay as they are until he decides otherwise. Examples:

- a new platform file is not picked up until its importer changes
- change detection is based on mtime
- the metro-cache backfill uses Buffer identity

## 4. The switch and the shell

- **The Rust core lives in `metro-rs/`.** It is a Cargo workspace (`Cargo.toml`, a committed `Cargo.lock`, one crate per component) plus a napi-rs package. `metro-rs/package.json` has a `build` script that produces the native binary. `require('./metro-rs')` exports `version()` and one binding per ported component.
- **Choose per component, at runtime.** Use the `METRO_RUST` environment variable (`off`, `all`, or comma-separated component ids from `port/phases.json`, e.g. `resolver,file-map`), with `unstable_rust: {resolver: true}` in Metro config as the persistent form. The environment variable wins. Unset means JavaScript.
- **Switch at the component's public boundary.** For example, metro-resolver's `resolve` and metro-file-map's default export. That way every existing caller and test goes through the switch. The JavaScript implementation stays in place as the fallback until Szymon decides to remove it.
- **Keep the public JS API frozen.** `runServer`, `runBuild`, `createConnectMiddleware`, `loadConfig`, every config hook and their signatures stay unchanged, so React Native CLI and Expo work as they do now.
- **Byte-identical output is the bar.** Bundles and source maps from JS and Rust must match exactly. The only exception is the minifier in phase 6, where bundles are compared by executing them. The differential check enforces this and prints the first differing module.

## 5. Performance

- The baseline is pure-JS Metro at `baselineRef`, checked out under `port/.cache/` with its own `node_modules`. The candidate is this checkout with the phase's components in Rust. Targets in `port/phases.json` are speed-ups against that baseline, and they add up across phases.
- **Scenarios:**
  - `cold-build`: empty caches, dominated by Babel until phase 6.
  - `cold-filemap`: warm transform cache, empty file-map cache.
  - `warm-build`: restarting Metro.
  - `edit`: dev server and a leaf edit; the first edit after start is reported too.
  - `micro:<component>`
- **Every phase whose target names a micro-benchmark adds `port/bench/micro/<component>.js` first.** It measures the component through its public API and runs against the JavaScript baseline before any Rust exists. The gate fails while a target's micro-benchmark is missing.
- **Run `--report` on an otherwise idle machine,** with the same machine for the whole phase. The report records CPU, cores, memory, OS and Node. Quick runs are never valid for a report.
- **Make Metro faster, not the benchmark.** Special-casing the bench app, its paths or the benchmark scripts is forbidden.

## 6. Working practices

- **One phase is one branch** (`port/phase-<n>`) and one PR into `main` of V3RON/metro. Never open PRs against facebook/metro.
- **Commit early with small commits.** Each commit message says which component it touches and whether the gate passed.
- **Check new Rust crates before adding them.** Prefer what the study already chose: napi-rs, oxc or swc for parsing, `sourcemap`, `notify`, Meta's `watchman_client`. Name every new crate in the PR.
- **Leave alone what the port does not need.** Do not reformat, restructure or "clean up" JavaScript outside the component you are porting.
- **When you are blocked, write it down.** Examples: a test that cannot pass without a behaviour change, a target that looks unreachable, or a decision only Szymon can make. Put it in the PR with what you tried and what you need, then stop.
