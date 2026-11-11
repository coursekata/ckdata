//! R conversion only; the shared core owns column classification and decisions.
use ckdata::column_policy::{self, Column, ColumnChange, ColumnUse};
use extendr_api::Result;
use extendr_api::prelude::*;

fn field(row: &List, name: &str) -> Result<Robj> {
    row.iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
        .ok_or_else(|| Error::Other(format!("missing column-policy field: {name}")))
}

fn text(row: &List, name: &str) -> Result<String> {
    String::try_from(field(row, name)?)
}

fn optional_text(row: &List, name: &str) -> Result<Option<String>> {
    let value = field(row, name)?;
    let value = value
        .as_str_iter()
        .and_then(|mut values| values.next())
        .ok_or_else(|| Error::Other(format!("{name} must be a string or NA")))?;
    Ok((!value.is_na()).then(|| value.to_owned()))
}

fn read_columns(read: List) -> Result<Vec<ColumnUse>> {
    read.values()
        .map(|row| {
            let row = List::try_from(row)?;
            Ok(ColumnUse {
                column: Column {
                    table: text(&row, "table")?,
                    name: text(&row, "column")?,
                },
                used: bool::try_from(field(&row, "used")?)?,
                returned: bool::try_from(field(&row, "returned")?)?,
            })
        })
        .collect()
}

fn read_changes(changes: List) -> Result<Vec<ColumnChange>> {
    changes
        .values()
        .map(|row| {
            let row = List::try_from(row)?;
            Ok(ColumnChange {
                table: text(&row, "table")?,
                column: text(&row, "column")?,
                new_table: optional_text(&row, "new_table")?,
                new_column: optional_text(&row, "new_column")?,
                removed_on: text(&row, "removed_on")?,
            })
        })
        .collect()
}

/// @noRd
#[extendr]
fn native_column_policy(
    mode: &str,
    read: List,
    outputs: Vec<String>,
    allow: List,
    changes: Nullable<List>,
) -> Result<List> {
    let read = read_columns(read)?;
    let allow = allow
        .iter()
        .map(|(table, columns)| {
            Ok(Vec::<String>::try_from(columns)?
                .into_iter()
                .map(|name| Column {
                    table: table.to_owned(),
                    name,
                })
                .collect::<Vec<_>>())
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let custom = match changes {
        Nullable::NotNull(changes) => Some(read_changes(changes)?),
        Nullable::Null => None,
    };
    let decision = column_policy::evaluate(
        mode.parse()
            .map_err(|error: &str| Error::Other(error.into()))?,
        &read,
        &outputs,
        &allow,
        custom
            .as_deref()
            .unwrap_or_else(|| column_policy::column_changes()),
    );
    let hits = decision.findings;
    Ok(list!(
        hits = list!(
            table = hits
                .iter()
                .map(|hit| hit.column.table.as_str())
                .collect::<Vec<_>>(),
            column = hits
                .iter()
                .map(|hit| hit.column.name.as_str())
                .collect::<Vec<_>>(),
            note = hits.iter().map(|hit| hit.note.as_str()).collect::<Vec<_>>(),
            used = hits.iter().map(|hit| hit.used).collect::<Vec<_>>(),
            returned = hits.iter().map(|hit| hit.returned).collect::<Vec<_>>()
        ),
        removed = decision.removed,
        nothing_left = decision.nothing_left
    ))
}

extendr_module! {
    mod column_policy;
    fn native_column_policy;
}
