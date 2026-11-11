//! Convert Python query observations into the core's language-neutral policy.
use std::collections::HashMap;

use ckdata::column_policy::{self, Column, ColumnUse};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

type Finding = (String, String, String, bool, bool);
type Decision = (Vec<Finding>, Vec<String>, bool);

#[pyfunction]
#[pyo3(name = "_column_policy")]
pub(super) fn evaluate(
    mode: &str,
    read: Vec<(String, String, bool, bool)>,
    outputs: Vec<String>,
    allow: HashMap<String, Vec<String>>,
) -> PyResult<Decision> {
    let read = read
        .into_iter()
        .map(|(table, name, used, returned)| ColumnUse {
            column: Column { table, name },
            used,
            returned,
        })
        .collect::<Vec<_>>();
    let allow = allow
        .into_iter()
        .flat_map(|(table, columns)| {
            columns.into_iter().map(move |name| Column {
                table: table.clone(),
                name,
            })
        })
        .collect::<Vec<_>>();
    let decision = column_policy::evaluate(
        mode.parse().map_err(PyValueError::new_err)?,
        &read,
        &outputs,
        &allow,
        column_policy::column_changes(),
    );
    let findings = decision
        .findings
        .into_iter()
        .map(|hit| {
            (
                hit.column.table,
                hit.column.name,
                hit.note,
                hit.used,
                hit.returned,
            )
        })
        .collect();
    Ok((findings, decision.removed, decision.nothing_left))
}
