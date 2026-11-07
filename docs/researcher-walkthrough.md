# Researcher walkthrough

Use this walkthrough to try the R package against the retained synthetic Athena
environment. All rows are invented. It exercises the installed package and the
real Athena/Parquet path, including joins before download and local saved results.

The fixture has one physical table, `types`, with two rows. A remote join below
uses two projections of that table; a second example joins a local lookup inline.
The fixture does not reproduce real research datasets, consent filtering or the
SafeInsights enclave. Those remain separate final acceptance checks.

## Start the prepared project

For this local checkout, the isolated project is `.local/researcher-uat/`. It
contains its own R library and AWS configuration, plus a `snapshots/` directory.
It is ignored by Git. Preparing it does not sign in, assume roles or query AWS.
Another tester needs an operator to provide equivalent fixture-only configuration.

Open `researcher-uat.Rproj` in RStudio and start a new R session in the project
so its `.Renviron` and `.Rprofile` are read. No terminal login command is needed.
Do not use `R --vanilla` here: it skips the prepared startup files.

The prepared project has a distinct SSO session name with no cached sign-in.
It does not log you out of other AWS sessions. Starting R and loading ckdata
do not authenticate. Your first connection should open AWS's browser login;
complete it there while the R call waits. Later connections can reuse that
sign-in. AWS CLI v2 must be installed and available on R's PATH.

The local configuration uses a synthetic source role and asks ckdata to assume
the restricted synthetic research reader once. It provides no embedded access
keys. Browser sign-in is still required for live queries. No sign-in or live-resource
verification is performed merely by preparing these files.

## 1. Check the installation and connect

In R:

```r
library(ckdata)
library(DBI)
library(dplyr)

packageVersion("ckdata")
find.package("ckdata")
```

Optionally, before the first sign-in, check fail-fast mode:

```r
dbConnect(ckdata::ckdata(), login = FALSE)
```

Expect an SSO authentication error immediately, without a browser. This mode is
also the default in scripts and jobs. `options(ckdata.login = FALSE)` applies it
to managed helpers. Set that option to NULL to restore automatic interactive
sign-in, then connect normally:

```r
con <- dbConnect(ckdata::ckdata())
info <- dbGetInfo(con)
info[c("account", "arn", "region", "schema", "workgroup")]
```

Expect the browser to open for sign-in, then the connection to complete once
you finish. You can interrupt R to cancel; failed sign-in is not retried.
With an already-valid sign-in, the connection completes without a new browser.
Expect ckdata version `0.0.1` from the project's `library/`, not the older package
in your regular R library. Connection information should identify the synthetic
research reader, database `ck_datalake_tst_ckdata_fixtures`, region `us-west-2`,
and workgroup `ck-datalake-tst-ckdata-research`. Check before proceeding:

```r
stopifnot(
  info$schema == "ck_datalake_tst_ckdata_fixtures",
  info$workgroup == "ck-datalake-tst-ckdata-research",
  grepl(":assumed-role/ck-datalake-tst-ckdata-research-reader/", info$arn, fixed = TRUE)
)
```

Connecting authenticates and checks identity; it does not select table rows.
If sign-in fails, read the error and retry the connection when ready. Check that
AWS CLI v2 is available if the login tool cannot be started. Do not change the
workgroup, role or database to a real research environment.

## 2. Discover tables and columns

```r
dbListTables(con)
dbListObjects(con)
dbListFields(con, "types")
dbExistsTable(con, "types")
dbExistsTable(con, "missing_table")
```

Expect the `types` table and columns `id`, `s_null`, `s_empty`, `big`, `dec`,
`ts` and `arr`. The existence checks return TRUE and FALSE. These operations
use catalog metadata without executing a data query.

## 3. Build and inspect a query before running it

```r
types <- tbl(con, "types")
selected <- types |>
  filter(id == 1L) |>
  select(id, s_empty, big) |>
  mutate(double_id = id * 2L)

colnames(selected)
show_query(selected)
```

Expect SQL with the filter, projection and calculated column. Creating the
pipeline and showing its SQL do not fetch rows. Now execute it:

```r
collect(selected)
```

Expect one row, with `id = 1`, an empty string and `double_id = 2`.
Printing `types` or `selected` also executes a preview query. Try this deliberately:

```r
print(types, n = 2)
```

## 4. Inspect values and empty results

```r
rows <- collect(types) |> arrange(id)
rows
as.character(rows$big)
is.na(rows$s_null)
rows$s_empty
rows$arr[[1]]

empty <- types |> filter(id < 0L) |> collect()
empty
vapply(rows, function(x) paste(class(x), collapse = "/"), character(1))
vapply(empty, function(x) paste(class(x), collapse = "/"), character(1))
```

Expect two rows. The wide integer is exactly `9007199254740993`, followed by NA.
The first `s_null` is NA, while the first `s_empty` is `""`. The first array is
`c(1L, 2L)`. The empty query returns zero rows with the same columns and R classes.

Sort locally after collection when row order matters. Athena's parallel UNLOAD
files do not guarantee combined row order, even with SQL `ORDER BY`.

## 5. Join remotely before downloading

```r
identifiers <- types |> select(id, big)
labels <- types |> select(id, s_empty)
joined <- left_join(identifiers, labels, by = "id") |>
  filter(id > 0L)

show_query(joined)
joined_rows <- collect(joined) |> arrange(id)
joined_rows
```

Expect a SQL `LEFT JOIN`, then two downloaded rows with `id`, `big` and
`s_empty`. Both sides are lazy relations on the same connection. They are joined
in Athena; ckdata does not download each side and join them locally.

For a small local lookup, embed its values into the remote SELECT:

```r
lookup <- tibble(id = c(1L, 2L), label = c("first", "second"))
labelled <- left_join(types, lookup, by = "id", copy = "inline")
show_query(labelled)
collect(labelled) |> arrange(id)
```

Expect `VALUES` in the SQL and the two labels in the result. This creates no
remote table. Larger uploads and `copy_to()` are outside this read-only interface.

## 6. Summarise before downloading

```r
summary <- types |>
  mutate(has_big = !is.na(big)) |>
  group_by(has_big) |>
  summarise(n = n(), .groups = "drop")

show_query(summary)
collect(summary) |> arrange(has_big)
```

Expect two groups, FALSE and TRUE, with one row in each group. The grouping and
counting happen in Athena. Remote median and quantile calculations require an
explicit choice of `approx_median()` or `approx_quantile()`.

## 7. Use SQL, parameters and Arrow

```r
lake_query("SELECT id, s_empty FROM types WHERE id = 1", con = con)
dbGetQuery(con, "SELECT id, s_empty FROM types WHERE id = ?", params = list(1L))

arrow_result <- lake_query("SELECT * FROM types", as = "arrow", con = con)
arrow_result$schema
```

Expect the two filtered SQL queries to agree. Arrow output retains nested types
and exact integers/decimals. Converting Arrow data to an R frame can change its
representation or precision; see `?ckdata_types`.

For paged DBI retrieval, one query supplies all fetches:

```r
result <- dbSendQuery(con, "SELECT id, s_empty, big FROM types")
dbFetch(result, n = 0)  # Typed columns; cursor does not advance
dbFetch(result, n = 1)
dbFetch(result, n = Inf)
dbHasCompleted(result)
dbClearResult(result)
```

Expect one row in each nonempty fetch and TRUE at completion. Do not assume
which row arrives first. Clear results when finished.

## 8. Reuse a saved result until the daily refresh

The prepared project sets freshness to 07:00 in `America/Los_Angeles` and enables
reuse messages. The synthetic table itself is static: this demonstrates the
freshness rule, not a scheduled rebuild of that fixture.

```r
saved <- collect_saved(joined)
reused <- collect_saved(joined)
identical(saved, reused)

fresh <- collect_saved(joined, refresh = TRUE)
identical(arrange(saved, id), arrange(fresh, id))
```

Expect TRUE for both comparisons. The second call reports that it is reading a
saved result and submits no data query, provided no daily refresh boundary has
passed. `refresh = TRUE` reruns the query. Ordinary saves default to a directory
within this R session's temporary directory.

Override the schedule for one call or source when needed:

```r
collect_saved(joined, freshness = "always")
collect_saved(joined, freshness = daily_at("08:00", tz = "America/Los_Angeles"))
```

Precedence is collection, source table, connection, R option, environment,
then fetch anew. `freshness_check()` can instead use a source-specific completed
build revision; this fixture has no completed-build marker. That behavior is
covered with controlled unit tests rather than a pretend lake rebuild.

## 9. Keep an indefinite reproducible snapshot

```r
snapshot_dir <- file.path("snapshots", "researcher-walkthrough")
baseline <- collect_saved(joined, snapshot = "baseline", dir = snapshot_dir)
again <- collect_saved(joined, snapshot = "baseline", dir = snapshot_dir)
identical(baseline, again)
list.files(snapshot_dir, full.names = TRUE)
```

Expect TRUE and a `snapshot-baseline-<query-key>.parquet` file. The snapshot does
not refresh or expire. A new label creates another snapshot; `snapshot = TRUE`
chooses a name automatically. Labels supplement the SQL and source identity:
reusing a label with a different query creates a distinct file.

Without `dir`, snapshots use the project's configured `snapshots/` directory.
In an unconfigured R session, their default is the current directory. Normal
saved results use temporary storage by default. A persistent directory for
normal saves keeps the files, but freshness rules still apply.

## 10. Reconnect, then read a snapshot without AWS

```r
dbDisconnect(con)
con <- dbConnect(ckdata::ckdata())
rebuilt <- left_join(
  tbl(con, "types") |> select(id, big),
  tbl(con, "types") |> select(id, s_empty),
  by = "id"
) |> filter(id > 0L)

collect_saved(rebuilt, snapshot = "baseline", dir = snapshot_dir)
dbDisconnect(con)
```

Expect the original snapshot for the same SQL and source/access identity. An
additional assumed-role session lasts at most one hour and is not silently
renewed by ckdata. Reconnect explicitly and rebuild lazy tables after expiry.

The Parquet file also works directly after disconnecting or restarting R:

```r
snapshot_files <- list.files(
  file.path("snapshots", "researcher-walkthrough"),
  pattern = "^snapshot-baseline-.*[.]parquet$", full.names = TRUE
)
snapshot_files
```

For a single run there should be one baseline file. If several exist, choose the
exact file for the analysis you want before reading it:

```r
offline <- arrow::read_parquet(snapshot_files[[1]])
offline
```

Direct Parquet reading needs no AWS sign-in. Local files have no package-managed
encryption or online authorization check; manage them as ordinary research files.

## 11. Try the convenience interface

The same project configuration also supports the managed helpers:

```r
managed <- lake_tbl(
  "types", freshness = daily_at("08:00", tz = "America/Los_Angeles")
) |> select(id, s_empty)
show_query(managed)
collect(managed) |> arrange(id)
collect_saved(managed)
lake_disconnect()
```

These open their own managed connection on first use. Use either that route or
an explicit DBI connection for an analysis. Existing lazy queries retain their
old connection after disconnecting; rebuild them before reuse. Here the table's
08:00 policy overrides the environment's 07:00 policy for `collect_saved()`;
ordinary `collect()` always fetches anew.

## Expected errors and acceptance notes

Try a closed connection to check that the error is understandable:

```r
try(dbGetQuery(con, "SELECT id FROM types"))
```

Expect a closed-connection error. Writes, transactions and remote
materialization are unsupported. Save failures return fresh rows with a warning;
refresh failures return available stale rows with a warning, or propagate the
query error when no saved result exists. An unreadable snapshot errors and is
preserved. These failure paths are tested offline; do not manufacture an AWS
outage or alter the fixture to exercise them.

Record any confusing output, unexpected query, type mismatch or unexplained
failure along with the step and package version. Never include access tokens.
Check whether you could discover the table, understand when queries ran, join and
summarise remotely, distinguish temporary reuse from durable snapshots, and recover
from a closed session using only public functions.

The fixture has no deprecated columns to demonstrate column-policy warnings.
Explore/audit/publish behavior has unit coverage. No elapsed-hour expiry or real
research/enclave test is required for this walkthrough.

## Automated coverage

This document is a manual acceptance guide, not another test runner. Existing
package unit tests cover the failure cases and configuration combinations. Keep
live tests focused on AWS/Parquet integration that offline tests cannot establish.

| Behavior | Offline coverage |
|---|---|
| Metadata discovery, lazy joins and one final submission | [test_dbi.R](../r/tests/testthat/test_dbi.R) |
| SQL translations, inline values, grouping and joins | [test_dbplyr.R](../r/tests/testthat/test_dbplyr.R) |
| Parameter quoting, vectors, missing values and rebinding | [test_parameters.R](../r/tests/testthat/test_parameters.R) |
| R types, Arrow ownership and empty results | [test_types.R](../r/tests/testthat/test_types.R), [test_handoff.R](../r/tests/testthat/test_handoff.R) |
| Saved-result identity, reconnects, snapshots and failures | [test_collect_saved.R](../r/tests/testthat/test_collect_saved.R) |
| Schedule precedence, DST and revision checks | [test_freshness.R](../r/tests/testthat/test_freshness.R) |
| Managed settings, expiry and explicit reconnection | [test_settings.R](../r/tests/testthat/test_settings.R) |
| Automatic SSO login, fail-fast options, retry limits and cancellation | [test_auth.R](../r/tests/testthat/test_auth.R) |
| Column-policy behavior | [test_column_policy.R](../r/tests/testthat/test_column_policy.R) |

The [testing guide](../tests/README.md) describes the existing opt-in live suite.
It does not run just because you open this project or read this walkthrough.
