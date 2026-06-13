//! Golden integration tests for the config evaluator.
//!
//! Each case is a directory `tests/golden/<case>/` containing:
//!
//! - `in/` — a monorepo tree (SRC files and `.star` libraries) to evaluate.
//! - exactly one golden, written by the harness:
//!   - `expected.json` — captured config as JSON (the evaluation succeeded), or
//!   - `expected.err`  — the error chain text (the evaluation failed).
//!
//! The harness runs [`capyfun::config::evaluate`] on each `in/` tree and diffs
//! the result against the golden. To (re)generate goldens after an intentional
//! change:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test golden_test
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use capyfun::config;

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

        let json_golden = case.join("expected.json");
        let err_golden = case.join("expected.err");

        match config::evaluate(&in_dir) {
            Ok(cfg) => {
                let actual = serde_json::to_string_pretty(&cfg).unwrap() + "\n";
                if update && err_golden.exists() {
                    fs::remove_file(&err_golden).unwrap();
                }
                if !update && err_golden.exists() {
                    failures.push(format!(
                        "case `{name}`: expected an error (expected.err present) but \
                         evaluation succeeded with:\n{actual}"
                    ));
                    continue;
                }
                check(&json_golden, &actual, update, &mut failures);
            }
            Err(e) => {
                let actual = format!("{e:#}\n");
                if update && json_golden.exists() {
                    fs::remove_file(&json_golden).unwrap();
                }
                if !update && json_golden.exists() {
                    failures.push(format!(
                        "case `{name}`: expected success (expected.json present) but \
                         evaluation errored with:\n{actual}"
                    ));
                    continue;
                }
                check(&err_golden, &actual, update, &mut failures);
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} golden case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
