# Evaluation after phase 5: full JS against the hybrid

**Status: plan, not run.** It runs once phase 5 is approved.

On 2026-10-15 Szymon decided to stop after phase 5 and decide whether the port paid off. Phase 5 is the last phase that moves Metro's own code; phase 6 would hand the transform to swc. The decision is whether to continue, stop and keep the hybrid, or stop and keep JS.

This evaluation is separate from the phase gates. It doesn't change the phase 5 yardstick: `port/phases.json`, the bench app, `port/bench/run.js` and `port/js-only-tests.json` stay as they are. It reuses the same tools and adds measurements around them.

## What is compared

- **JS:** pure-JS Metro at `baselineRef`, the same baseline the gates use.
- **Hybrid:** this repository at the approved phase 5 head, with `METRO_RUST=all`.
- **Also reported, for context:** Metro at `baselineRef` with `@swc/core`-based transformation (the bench app already depends on it), to show how much of a cold build the transform alone accounts for. That is what phase 6 would go after.

## Machines

- **Main numbers:** the cloud runner the gates use (4 × Xeon at 2.10 GHz, 16 GB, Linux), medians of 5 or more interleaved runs.
- **A developer laptop as a second data point:** Szymon's MacBook (Apple Silicon).
  - File opens from non-Apple binaries are serialised on that machine, likely by an endpoint-security agent. Rust-side IO looks slower there than it is.
  - Mac numbers are reported with that caveat. If the agent can be paused for a run, they are reported both ways.
- **CPU, cores, memory, OS and Node** are recorded for every run, as `compare.js` already does.

## Apps

1. **The bench app** (`port/bench/app`): synthetic, 1,500 components, about 2,700 modules in the graph. Every gate number so far comes from it.
2. **At least one real React Native app.** **Szymon to choose (open question).** What matters in the choice:
   - a real dependency tree (node_modules with Haste-free packages, `exports` maps, symlinks from a monorepo);
   - assets;
   - a size larger than the bench app.

   Candidates: an internal app, or a public one such as the RN tester app or an Expo example.
3. **A monorepo layout** (workspaces, symlinked packages), if neither app has one. Resolution and the file map behave differently there.

## Scenarios

| Scenario | What it shows | Source |
|---|---|---|
| cold-build | first build on a new machine or after a cache reset | `run.js` |
| cold-filemap | warm transform cache, file map rebuilt (branch switch, CI) | `run.js` |
| warm-build | restarting Metro: the everyday start | `run.js` |
| edit and first edit | Fast Refresh latency, from the file write to the HMR message | `run.js` |
| time to first bundle request served | `runServer` → `GET /index.bundle` returns, warm caches: what a developer waits for | new, in `port/eval/` |
| a branch switch | `git checkout` of about 200 changed files with the server running, until the next bundle is served | new, in `port/eval/` |
| memory | RSS after a forced GC and peak RSS, for a server kept for 30 minutes of scripted edits | new, in `port/eval/` |

The new scenarios go in `port/eval/`, not `port/bench/`, so that the gates' yardstick stays as it is.

## Correctness

- **The bundle and HMR differentials** run on every app above, not only the bench app. Every byte of the bundles and maps must be identical.
- **The whole Jest suite** passes in both modes, which the gate already requires.
- **A real-device smoke test** on the real app: the app loads, Fast Refresh works, a red box symbolicates. Szymon runs it.

## Costs

The speed-up is weighed against what the hybrid costs to keep:

- **Code:**
  - lines of Rust (about 33,000 in `metro-rs/crates` after phase 4);
  - lines of JS glue (`rust.js` / `rustGraph.js` and the switches);
  - JS code kept twice: the fallback stays until Szymon removes it.
- **Build and release:**
  - the native binary per platform: macOS arm64 and x64, Linux gnu and musl, Windows;
  - binary size (about 2.5 MB on macOS arm64 after phase 4);
  - the CI matrix;
  - the time to release a version;
  - how upstream changes to Metro's JS get mirrored in Rust.
- **Install:** what a user installs (prebuilt binaries through optional dependencies, as napi-rs packages do), and what happens without a matching binary: Metro falls back to JS.
- **Debugging:** stack traces and logs across the boundary, verify mode, the switch.

## Output

`port/reports/evaluation.md` contains:

- a table per app and machine;
- a section per cost;
- a recommendation with one of three outcomes:
  - **continue** to phase 6, or to swc or oxc in the transformer;
  - **keep the hybrid**: release it behind the switch, default off or on;
  - **stop**: keep JS and record what was learnt.

Szymon decides.

## What a "paid off" result looks like (proposed, for Szymon to set)

These thresholds are a proposal. The decision is Szymon's:

- **Warm-build and branch-switch:** ≥ 2× on the real app, on both machines.
- **Edit:** no slower on any app, and faster where the edit isn't dominated by the watcher.
- **Memory:** no worse than +10% after GC.
- **Correctness:** zero differential failures on every app.
