//! Tests for the hermetic [`FixtureRunner`] (a deterministic mock agent driven
//! by recorded shell scripts) and the [`VerifyingRunner`] verify → retry loop.
//! Both run real subprocesses (`sh`), but stay hermetic: no network, no model.

use std::cell::Cell;
use std::fs;

use super::*;

/// An [`AgentInvocation`] is only used here for its `agent_id`; the other fields
/// are irrelevant to the fixture/verify runners, so give them placeholders.
fn inv(agent_id: &str) -> AgentInvocation {
    AgentInvocation {
        harness: HarnessKind::ClaudeCode,
        provider: "anthropic".into(),
        model_id: "claude-opus-4-8".into(),
        credential: None,
        base_url: None,
        prompt: String::new(),
        agent_id: agent_id.into(),
        paths: Vec::new(),
    }
}

#[test]
fn fixture_script_name_strips_label_to_target() {
    assert_eq!(FixtureRunner::script_name("//tools/agent:porter"), "porter");
    assert_eq!(FixtureRunner::script_name("porter"), "porter");
    assert_eq!(FixtureRunner::script_name("a/b/c"), "c");
}

#[test]
fn fixture_runner_runs_the_recorded_script_in_the_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    // A recorded "agent": appends a line to a file and echoes the prompt back.
    fs::write(
        dir.path().join("porter.sh"),
        "#!/bin/sh\necho \"$CAPYFUN_AGENT_PROMPT\" > prompt.txt\nprintf 'edited\\n' >> out.txt\n",
    )
    .unwrap();

    let runner = FixtureRunner::new(dir.path());
    runner
        .run(&inv("//tools/agent:porter"), "do the thing", work.path())
        .unwrap();

    assert_eq!(
        fs::read_to_string(work.path().join("out.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        fs::read_to_string(work.path().join("prompt.txt")).unwrap(),
        "do the thing\n"
    );
}

#[test]
fn fixture_runner_errors_when_script_missing() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let runner = FixtureRunner::new(dir.path());
    let err = runner
        .run(&inv("//tools/agent:ghost"), "p", work.path())
        .unwrap_err();
    assert!(err.to_string().contains("ghost.sh"), "{err}");
}

/// A fake inner runner that records how many times it ran and can "fix" the
/// workdir on a later attempt (so the verifier passes only after a retry).
struct CountingRunner {
    runs: Cell<usize>,
    /// Attempt index (0-based) at which it writes the `pass` marker the verifier
    /// checks; `usize::MAX` means never.
    fix_on: usize,
}

impl AgentRunner for CountingRunner {
    fn run(&self, _inv: &AgentInvocation, _prompt: &str, workdir: &Path) -> Result<()> {
        let n = self.runs.get();
        self.runs.set(n + 1);
        if n >= self.fix_on {
            fs::write(workdir.join("pass"), "ok").unwrap();
        }
        Ok(())
    }
}

#[test]
fn verifying_runner_passes_first_try_runs_inner_once() {
    let work = tempfile::tempdir().unwrap();
    let inner = CountingRunner {
        runs: Cell::new(0),
        fix_on: 0,
    };
    // Verifier passes immediately (inner writes `pass` on attempt 0).
    let runner = VerifyingRunner::new(&inner, "test -f pass", 1);
    runner.run(&inv("a:b"), "p", work.path()).unwrap();
    assert_eq!(inner.runs.get(), 1, "no retry needed");
}

#[test]
fn verifying_runner_retries_then_succeeds() {
    let work = tempfile::tempdir().unwrap();
    // Inner only writes the `pass` marker on its second run (attempt index 1).
    let inner = CountingRunner {
        runs: Cell::new(0),
        fix_on: 1,
    };
    let runner = VerifyingRunner::new(&inner, "test -f pass", 1);
    runner.run(&inv("a:b"), "p", work.path()).unwrap();
    assert_eq!(inner.runs.get(), 2, "first attempt + one retry");
}

#[test]
fn verifying_runner_bails_after_exhausting_retries() {
    let work = tempfile::tempdir().unwrap();
    let inner = CountingRunner {
        runs: Cell::new(0),
        fix_on: usize::MAX, // never fixes
    };
    let runner = VerifyingRunner::new(&inner, "test -f pass", 1);
    let err = runner.run(&inv("a:b"), "p", work.path()).unwrap_err();
    assert!(err.to_string().contains("still failing"), "{err}");
    assert_eq!(inner.runs.get(), 2, "first attempt + one retry, then give up");
}

#[test]
fn verifying_runner_feeds_failure_back_into_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    // A fixture agent that only "fixes" the build once it sees verifier feedback
    // in the prompt — proving the loop closes feedback back to the agent.
    fs::write(
        dir.path().join("fixer.sh"),
        "#!/bin/sh\nif echo \"$CAPYFUN_AGENT_PROMPT\" | grep -q 'VERIFIER FAILED'; then\n  : > pass\nfi\n",
    )
    .unwrap();
    let inner = FixtureRunner::new(dir.path());
    let runner = VerifyingRunner::new(&inner, "test -f pass", 1);
    runner.run(&inv("//x:fixer"), "port it", work.path()).unwrap();
    assert!(work.path().join("pass").exists());
}
