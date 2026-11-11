use super::*;

fn column(table: &str, name: &str) -> Column {
    Column {
        table: table.into(),
        name: name.into(),
    }
}

fn observed(table: &str, name: &str, used: bool, returned: bool) -> ColumnUse {
    ColumnUse {
        column: column(table, name),
        used,
        returned,
    }
}

fn change() -> ColumnChange {
    ColumnChange {
        table: "mart_events".into(),
        column: "old".into(),
        new_table: Some("mart_events".into()),
        new_column: Some("current".into()),
        removed_on: "2026-12-01".into(),
    }
}

#[test]
fn modes_bypass_report_or_remove_the_same_observed_column() {
    let read = [observed("mart_events", "old", true, true)];
    let outputs = vec!["id".into(), "old".into()];
    let changes = [change()];
    let explore = evaluate(Mode::Explore, &read, &outputs, &[], &changes);
    assert!(explore.findings.is_empty());
    assert!(explore.removed.is_empty());
    let audit = evaluate(Mode::Audit, &read, &outputs, &[], &changes);
    assert_eq!(audit.findings.len(), 1);
    assert!(audit.removed.is_empty());
    let publish = evaluate(Mode::Publish, &read, &outputs, &[], &changes);
    assert_eq!(publish.removed, ["old"]);
    assert!(!publish.nothing_left);
}

#[test]
fn only_returned_outputs_are_removed_and_allow_is_table_specific() {
    let read = [
        observed("mart_events", "old", true, false),
        observed("int_events", "raw", true, true),
    ];
    let outputs = vec!["raw".into()];
    let decision = evaluate(
        Mode::Publish,
        &read,
        &outputs,
        &[column("other", "raw")],
        &[change()],
    );
    assert_eq!(decision.findings.len(), 2);
    assert_eq!(decision.removed, ["raw"]);
    assert!(decision.nothing_left);
    let allowed = evaluate(
        Mode::Publish,
        &read,
        &outputs,
        &[column("int_events", "raw")],
        &[change()],
    );
    assert_eq!(allowed.findings.len(), 1);
    assert!(!allowed.findings[0].returned);
    assert!(allowed.removed.is_empty());
    assert!(!allowed.nothing_left);
}

#[test]
fn unused_or_redefined_columns_do_not_trigger_findings() {
    let read = [
        observed("mart_events", "old", false, false),
        observed("other", "old", true, true),
    ];
    let decision = evaluate(Mode::Publish, &read, &["old".into()], &[], &[change()]);
    assert!(decision.findings.is_empty());
    assert!(!decision.nothing_left);
}

#[test]
fn published_keys_suppress_internal_warnings_but_private_columns_remain_flagged() {
    let read = [
        observed("stg_events", "id", true, true),
        observed("stg_events", "raw", true, true),
        observed("mart_events", "id", false, false),
        observed("int_events", "temporary", true, false),
    ];
    let decision = evaluate(Mode::Publish, &read, &["id".into(), "raw".into()], &[], &[]);
    assert_eq!(decision.removed, ["raw"]);
    assert_eq!(decision.findings.len(), 2);
    assert!(decision.findings[0].note.starts_with("staging"));
    assert!(decision.findings[1].note.starts_with("intermediate"));
}

#[test]
fn deprecation_notes_cover_every_replacement_kind() {
    let mut change = change();
    assert_eq!(
        change.note(),
        "deprecated, removed on 2026-12-01; use `current`"
    );
    change.new_table = Some("mart_replacements".into());
    assert!(
        change
            .note()
            .ends_with("; use `current` on mart_replacements")
    );
    change.new_column = None;
    assert!(change.note().ends_with("; see mart_replacements"));
    change.new_table = None;
    assert!(change.note().ends_with(", with no replacement"));
}

#[test]
fn bundled_catalog_has_unique_complete_entries() {
    let changes = column_changes();
    assert!(!changes.is_empty());
    let mut seen = HashSet::new();
    for change in changes {
        assert!(
            !change.table.is_empty() && !change.column.is_empty() && !change.removed_on.is_empty()
        );
        assert!(seen.insert((&change.table, &change.column)));
    }
}
