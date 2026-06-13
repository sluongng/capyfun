//! Unit tests for the pure status-reporting logic. The Git/network-backed
//! computation is covered end to end against local bare repos in
//! `tests/status_test.rs`.

use super::*;

fn status(kind: TargetKind, state: State) -> TargetStatus {
    TargetStatus {
        label: "//p:t".to_owned(),
        kind,
        repo: "acme/x".to_owned(),
        desired: None,
        actual: None,
        state,
    }
}

#[test]
fn kind_strings() {
    assert_eq!(TargetKind::Import.as_str(), "import");
    assert_eq!(TargetKind::Vendor.as_str(), "vendor");
    assert_eq!(TargetKind::Export.as_str(), "export");
}

#[test]
fn up_to_date_is_not_actionable() {
    assert!(!State::UpToDate.is_actionable());
    assert!(!State::Error("boom".into()).is_actionable());
    assert!(State::Behind {
        count: Some(1),
        new: false
    }
    .is_actionable());
    assert!(State::PinChanged { new: true }.is_actionable());
}

#[test]
fn import_summary_counts_commits() {
    let s = status(
        TargetKind::Import,
        State::Behind {
            count: Some(3),
            new: false,
        },
    );
    assert_eq!(s.summary(), "3 commit(s) behind");
}

#[test]
fn export_summary_counts_changesets() {
    let s = status(
        TargetKind::Export,
        State::Behind {
            count: Some(2),
            new: false,
        },
    );
    assert_eq!(s.summary(), "2 change(s) to ship");
}

#[test]
fn new_target_summary_is_marked_uninitialized() {
    let s = status(
        TargetKind::Import,
        State::Behind {
            count: Some(5),
            new: true,
        },
    );
    assert_eq!(s.summary(), "uninitialized; 5 commit(s) behind");
}

#[test]
fn uncountable_delta_summary() {
    let s = status(
        TargetKind::Import,
        State::Behind {
            count: None,
            new: false,
        },
    );
    assert_eq!(s.summary(), "behind (commit(s) behind, count unknown)");
}

#[test]
fn vendor_pin_summaries() {
    assert_eq!(
        status(TargetKind::Vendor, State::PinChanged { new: false }).summary(),
        "pin changed; re-vendor"
    );
    assert_eq!(
        status(TargetKind::Vendor, State::PinChanged { new: true }).summary(),
        "uninitialized; pin to vendor"
    );
}
