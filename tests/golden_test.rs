//! Golden integration tests for the config evaluator.
//!
//! Each case is a directory `tests/golden/<case>/` containing:
//!
//! - `in/` — a monorepo tree (SRC files and `.star` libraries) to evaluate.
//! - **eval-stage** golden (always one):
//!   - `expected.json` — captured config as JSON (evaluation succeeded), or
//!   - `expected.err`  — the error chain text (evaluation failed).
//! - **compile-stage** golden (only when evaluation succeeded — lowering to IR
//!   plus static validation):
//!   - `expected.ir.json`     — the validated IR (compilation succeeded), or
//!   - `expected.compile.err` — the sorted diagnostics (validation failed).
//!
//! The harness runs [`capyfun::config::evaluate`] then [`capyfun::ir::compile`]
//! on each `in/` tree and diffs against the goldens. To (re)generate goldens
//! after an intentional change:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test golden_test
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use capyfun::{config, ir};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// All case directories under `tests/golden`, sorted for determinism.
fn discover_cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = fs::read_dir(golden_dir())
        .expect("tests/golden exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    cases
}

/// Diff one stage's outcome (Ok text vs Err text) against its pair of goldens,
/// enforcing that the right golden is present.
fn stage(
    name: &str,
    stage: &str,
    ok_golden: &Path,
    err_golden: &Path,
    outcome: Result<String, String>,
    update: bool,
    failures: &mut Vec<String>,
) {
    match outcome {
        Ok(actual) => {
            if update && err_golden.exists() {
                fs::remove_file(err_golden).unwrap();
            }
            if !update && err_golden.exists() {
                failures.push(format!(
                    "case `{name}` [{stage}]: expected failure but it succeeded with:\n{actual}"
                ));
                return;
            }
            check(ok_golden, &actual, update, failures);
        }
        Err(actual) => {
            if update && ok_golden.exists() {
                fs::remove_file(ok_golden).unwrap();
            }
            if !update && ok_golden.exists() {
                failures.push(format!(
                    "case `{name}` [{stage}]: expected success but it failed with:\n{actual}"
                ));
                return;
            }
            check(err_golden, &actual, update, failures);
        }
    }
}

/// Compare `actual` against the golden at `path`, or rewrite it under update
/// mode. On mismatch/missing, push a human-readable failure.
fn check(path: &Path, actual: &str, update: bool, failures: &mut Vec<String>) {
    if update {
        fs::write(path, actual).unwrap();
        return;
    }
    match fs::read_to_string(path) {
        Ok(expected) if expected == actual => {}
        Ok(expected) => failures.push(format!(
            "MISMATCH {}\n--- expected ---\n{expected}--- actual ---\n{actual}",
            path.display()
        )),
        Err(_) => failures.push(format!(
            "MISSING GOLDEN {} (run with UPDATE_GOLDEN=1)\n--- actual ---\n{actual}",
            path.display()
        )),
    }
}

#[test]
fn golden_config() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut failures = Vec::new();

    for case in discover_cases() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let in_dir = case.join("in");
        assert!(in_dir.is_dir(), "case `{name}` has no in/ directory");

        // Stage 1: evaluate SRC files into captured config.
        let eval = config::evaluate(&in_dir);
        let eval_outcome = match &eval {
            Ok(cfg) => Ok(serde_json::to_string_pretty(cfg).unwrap() + "\n"),
            Err(e) => Err(format!("{e:#}\n")),
        };
        stage(
            &name,
            "eval",
            &case.join("expected.json"),
            &case.join("expected.err"),
            eval_outcome,
            update,
            &mut failures,
        );

        // Stage 2: lower to IR + validate (only when evaluation succeeded).
        if let Ok(cfg) = &eval {
            let compile_outcome = match ir::compile(cfg) {
                Ok(ir) => Ok(serde_json::to_string_pretty(&ir).unwrap() + "\n"),
                Err(diags) => Err(diags.join("\n") + "\n"),
            };
            stage(
                &name,
                "compile",
                &case.join("expected.ir.json"),
                &case.join("expected.compile.err"),
                compile_outcome,
                update,
                &mut failures,
            );
        }
    }

    assert!(
        failures.is_empty(),
        "{} golden case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
