# Using ckdata

Complete [installation and configuration](installation.md) first. That guide
also records which parts of the target R and Python interface are available
in the current build.

## Queries in R

`lake_tbl()` creates a lazy dbplyr table. Compose dplyr operations on it, then
call `collect()` to retrieve a tibble. `show_query()` displays the SQL without
executing it. Printing a lazy table previews rows and therefore runs a query.

```r
library(ckdata)
library(dplyr)

query <- lake_tbl("mart_classes") |> count()

show_query(query)
collect(query)
```

`lake_query()` runs SQL directly. Choose Arrow output when you need the original
types and precision:

```r
lake_query("SELECT 1 AS example")
lake_query("SELECT 1 AS example", as = "arrow")
```

The managed helpers apply the existing column-policy heuristics when configured.
They do not establish arbitrary SQL provenance or replace AWS access controls.
See `?ckdata::ckdata_options` for explore, audit and publish modes.

### DBI and dbplyr

Use `DBI::dbConnect(ckdata::ckdata(), ...)` for an explicit connection, then
ordinary read-side DBI methods or `dplyr::tbl(con, "my_table")`. Close it with
`DBI::dbDisconnect(con)`.

The backend supports reading, metadata, quoting, cursor fetching and parameter
binding. Writes, transactions and remote materialization are outside its scope.
For small local lookup frames, use `dbplyr::copy_inline(con, lookup)` or
`left_join(..., copy = "inline")`. These embed values into the SELECT query
without creating a remote table. Keep inline inputs small. Inline double columns
preserve signed infinity; NaN becomes SQL NULL, as in dbplyr projections.

Remote `median()` and `quantile()` require an explicit choice of
`approx_median()` or `approx_quantile()`.

### Query parameters

Use positional `?` placeholders with Athena's native parameter API:

```r
DBI::dbGetQuery(con, "SELECT ? AS value", params = list("O'Brien"))
```

Alternatively, prepare with `dbSendQuery()`, bind with `dbBind()` and fetch with
`dbFetch()`. Clear the result with `dbClearResult()` when finished. Unnamed lists
and data frame columns bind by position. Equal-length vectors execute one query
per row and concatenate the results; rebinding replaces the previous result.
Zero-length vectors return a typed empty result. Timeouts apply per execution.

Bindings support logical, integer, double, character, factor, exact `integer64`,
Date, POSIXt, difftime, and lists of raw vectors/NULL for binary values. Factors
convert to character with a warning. Temporal predicates can bind difftime as
TIME, though Athena UNLOAD cannot return TIME columns.

## Saved results

`collect()` always retrieves a result. `collect_saved()` also saves the original
Arrow result as Parquet and can reuse it according to a freshness policy:

```r
collect_saved(
  query,
  freshness = daily_at("07:00", tz = "America/Los_Angeles")
)
```

A daily schedule assumes the source has refreshed by that local time. It follows
daylight saving time; it does not verify that a build completed. Use
`freshness_check(function(con) ..., every = 300)` when your source provides a
completed-build revision. The callback returns one revision string; `every`
controls how long that check is reused. Neither policy runs in the background.

Freshness takes precedence in this order:

1. The `collect_saved()` argument.
2. The source table's `lake_tbl(..., freshness = ...)` setting.
3. The connection's `DBI::dbConnect(..., freshness = ...)` setting.
4. The R option `ckdata.freshness`.
5. `CKDATA_FRESHNESS_AT` together with `CKDATA_FRESHNESS_TZ`.
6. Fetch anew when no policy is configured.

For an application-wide daily schedule, set the option or both environment
variables:

```r
options(ckdata.freshness = daily_at("07:00", tz = "America/Los_Angeles"))
```

```sh
export CKDATA_FRESHNESS_AT=07:00
export CKDATA_FRESHNESS_TZ=America/Los_Angeles
```

`freshness = "always"` overrides a configured policy. `refresh = TRUE` forces a
normal saved result to refresh. When joined tables have conflicting policies,
choose one explicitly in `collect_saved()`.

Normal saved results default to a ckdata directory within R's session temporary
directory. Set `options(ckdata.saved_dir = "path")` or pass `dir` to use another
directory. Persistent directories are not automatically deleted.

### Reproducible snapshots

```r
collect_saved(query, snapshot = "baseline", dir = "snapshots")
```

A snapshot is frozen indefinitely. Use `snapshot = TRUE` for an automatically
named snapshot or a string for a label. The default directory is the current
directory, configurable through `ckdata.snapshot_dir`. Choose a new label or
directory to create another snapshot; `refresh = TRUE` is invalid for snapshots.
An unreadable existing snapshot raises an error and is preserved.

### Files and failures

Saved results are ordinary Parquet files, readable offline without a connection
authorization check or package-managed encryption. Save failures warn and return
the fresh result. Refresh failures warn and return an available stale result;
without one, the query error propagates.

Keys include the exact SQL and source/access context, plus applicable managed
column policy for ckdata connections. Other DBI backends require an explicit
named `source` context; see `?collect_saved`. Concurrent callers can both fetch;
each writes a complete temporary sibling file before publishing. Normal results
replace the previous file, so the last publication wins. Snapshots publish
without replacement and return the first published result. No lock service is used.

## Queries in Python

`ckdata.query(sql)` exposes an Arrow result that PyArrow can consume:

```python
import ckdata
import pyarrow as pa

table = pa.table(ckdata.query("SELECT 1 AS example"))
frame = table.to_pandas()
```

Keep the Arrow table when you need its original types, or pass it to your
preferred dataframe library. Library-specific conversions can change precision
or representations.

## Types, empty results and ordering

Athena writes results as Parquet through UNLOAD. Empty results retain their
schema. When Athena produces no files, ckdata runs an additional schema-only
query to recover the Parquet schema. It does this only for fileless results.

Arrow output retains full precision and nested types. R tibble output uses
Arrow's conversions, with BIGINT consistently represented as
`bit64::integer64`. R and bit64 reserve their minimum integer values for NA;
R numeric conversions can also lose decimal or timestamp precision. See
`?ckdata::ckdata_types` for the type reference.

Typed missing R literals in dbplyr projections retain their column types,
including for empty results. SQL determines types inside expressions such as
`id + NA`. Raw SQL must follow Athena's UNLOAD output rules: alias expressions
(`SELECT 1 AS example`) and cast standalone NULL outputs
(`SELECT CAST(NULL AS VARCHAR) AS example`). Athena rejects duplicate output
names and unsupported UNLOAD types. ckdata does not rewrite these outputs.

UNLOAD writes files in parallel and does not guarantee combined row order, even
with SQL `ORDER BY`. Sort after retrieval when order matters, for example
`collect(query) |> arrange(column)` in R.

## Sessions and parallel work

AWS providers renew temporary credentials, including additional assumed roles,
while their source permits. Interactive SSO reauthentication opens AWS CLI's
browser flow when necessary; `login = FALSE` disables that interaction. A
running request resumes in the same native job after sign-in, without creating
another query. Permission denials stop immediately and never select another
identity. Call `lake_disconnect()` before changing connection settings; existing
lazy queries retain their original connection.

For parallel R work, use fresh sessions or PSOCK workers and connect inside each
worker. Forking after ckdata's native runtime starts cannot safely inherit the
worker and is rejected.
