//! Column policy is independent of SQL and query libraries. Adapters supply
//! observed source columns and apply the resulting projection to their query.

use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Explore,
    Audit,
    Publish,
}

impl FromStr for Mode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "explore" => Ok(Self::Explore),
            "audit" => Ok(Self::Audit),
            "publish" => Ok(Self::Publish),
            _ => Err("mode must be 'explore', 'audit', or 'publish'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Column {
    pub table: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct ColumnUse {
    pub column: Column,
    pub used: bool,
    pub returned: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ColumnChange {
    pub table: String,
    pub column: String,
    pub new_table: Option<String>,
    pub new_column: Option<String>,
    pub removed_on: String,
}

impl ColumnChange {
    fn note(&self) -> String {
        let replacement = match (&self.new_table, &self.new_column) {
            (table, Some(column)) => format!(
                "; use `{column}`{}",
                table
                    .as_ref()
                    .filter(|table| **table != self.table)
                    .map(|table| format!(" on {table}"))
                    .unwrap_or_default()
            ),
            (Some(table), None) => format!("; see {table}"),
            (None, None) => ", with no replacement".into(),
        };
        format!("deprecated, removed on {}{replacement}", self.removed_on)
    }
}

/// The same bundled change catalog is used by every adapter.
pub fn column_changes() -> &'static [ColumnChange] {
    static CHANGES: OnceLock<Vec<ColumnChange>> = OnceLock::new();
    CHANGES.get_or_init(|| {
        serde_json::from_str(include_str!("column_policy/changes.json"))
            .expect("the bundled column-change catalog must be valid")
    })
}

#[derive(Debug)]
pub struct Finding {
    pub column: Column,
    pub note: String,
    pub used: bool,
    pub returned: bool,
}

#[derive(Debug, Default)]
pub struct Decision {
    pub findings: Vec<Finding>,
    pub removed: Vec<String>,
    pub nothing_left: bool,
}

fn internal_table(table: &str) -> Option<&'static str> {
    if table.starts_with("stg_") {
        Some("staging")
    } else if table.starts_with("int_") {
        Some("intermediate")
    } else {
        None
    }
}

/// Decide warnings, removals and the all-output-columns error from query facts.
/// Columns used only by a filter remain in the query, including publish mode.
pub fn evaluate(
    mode: Mode,
    read: &[ColumnUse],
    outputs: &[String],
    allow: &[Column],
    changes: &[ColumnChange],
) -> Decision {
    if mode == Mode::Explore {
        return Decision::default();
    }
    let published: HashSet<_> = read
        .iter()
        .filter(|read| internal_table(&read.column.table).is_none())
        .map(|read| &read.column.name)
        .collect();
    let mut findings: Vec<Finding> = Vec::new();
    for read in read.iter().filter(|read| read.used || read.returned) {
        if allow.contains(&read.column) {
            continue;
        }
        let notes = changes
            .iter()
            .filter(|change| change.table == read.column.table && change.column == read.column.name)
            .map(ColumnChange::note)
            .chain(
                internal_table(&read.column.table)
                    .filter(|_| !published.contains(&read.column.name))
                    .map(|kind| format!("{kind} table, may change without notice")),
            );
        for note in notes {
            if let Some(hit) = findings
                .iter_mut()
                .find(|hit| hit.column == read.column && hit.note == note)
            {
                hit.used |= read.used;
                hit.returned |= read.returned;
            } else {
                findings.push(Finding {
                    column: read.column.clone(),
                    note,
                    used: read.used,
                    returned: read.returned,
                });
            }
        }
    }
    let removed = if mode == Mode::Publish {
        findings
            .iter()
            .filter(|hit| hit.returned)
            .map(|hit| hit.column.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let nothing_left = !outputs.is_empty() && outputs.iter().all(|name| removed.contains(name));
    Decision {
        findings,
        removed,
        nothing_left,
    }
}

#[cfg(test)]
mod tests;
