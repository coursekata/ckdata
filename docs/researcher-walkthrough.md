# Internal researcher runbook

This runbook is for reviewing the internal researcher workflow with the local
BB-6316 build. CourseKata's internal documentation remains the authority for
installation, account access and production source configuration.

For this local rehearsal, the test operator prepares `.local/researcher-uat/`
and installs the development build in your normal R library. Open that directory
in your usual editor and start a new R session there. An RStudio project is
optional. R loads the prepared startup settings itself; the researcher code
below needs no environment-loading or library-path setup.

The prepared source points only at the retained synthetic environment:
`types` is a private developer fixture with two invented rows. You do not need
to provision it or create an AWS account. This is a rehearsal of the interface,
not a test of production permissions or real research data.

Environment preparation is separate from this walkthrough; see the
[operator setup notes](development.md#local-uat-environment). The only test-specific
difference in the R examples is the invented fixture data and its table name.

## Open the source

```r
library(ckdata)
library(dplyr)

types <- lake_tbl("types")
colnames(types)
```

Load both packages once per R session. ckdata imports dplyr but does not attach
its functions. The first `lake_tbl()` opens the managed connection. If SSO
sign-in is needed, finish the browser prompt while the R call waits. An existing
valid sign-in is reused. No terminal login command is needed for this configured
SSO profile.

The table's column names are read from catalog metadata, without selecting rows.
The fixture has `id`, `s_null`, `s_empty`, `big`, `dec`, `ts` and `arr`. Entering
`types` at the console previews rows and therefore runs a query:

```r
types
```

The table name is supplied with this rehearsal. There is currently no managed
helper for browsing all available tables; the QA runbook checks DBI discovery.

## Build a query and download its result

```r
selected <- types |>
  filter(id == 1L) |>
  select(id, s_empty, big) |>
  mutate(double_id = id * 2L)

show_query(selected)
rows <- collect(selected)
rows
```

The pipeline and `show_query()` do not fetch rows. `collect()` runs the query
and returns a local tibble with one row, including `id = 1` and `double_id = 2`.
`(rows <- collect(selected))` is equivalent when you want to assign and display
in one expression. Work with `rows` using ordinary R functions after collection.

## Join and summarise before downloading

```r
identifiers <- types |> select(id, big)
labels <- types |> select(id, s_empty)
joined <- left_join(identifiers, labels, by = "id") |>
  filter(id > 0L)

show_query(joined)
joined_rows <- collect(joined) |> arrange(id)
joined_rows
```

This fixture has only one physical table, so the join uses two projections of
it. With a configured research source, two `lake_tbl()` calls can open different
tables on the same managed connection. The join runs in Athena before download.
Arrange locally after collection when combined row order matters.

A small lookup already in R can be embedded in the query:

```r
lookup <- tibble(id = c(1L, 2L), label = c("first", "second"))
labelled <- left_join(types, lookup, by = "id", copy = "inline")
collect(labelled) |> arrange(id)
```

To download a summary rather than all underlying rows:

```r
counts <- types |>
  mutate(has_big = !is.na(big)) |>
  group_by(has_big) |>
  summarise(n = n(), .groups = "drop")

collect(counts) |> arrange(has_big)
```

The fixture returns FALSE and TRUE groups with one row in each.

## Reuse saved results

The prepared project sets a 07:00 `America/Los_Angeles` freshness schedule and
enables reuse messages. The fixture is static; this demonstrates the reuse
policy, not a daily rebuild of the fixture.

```r
saved <- collect_saved(joined)
reused <- collect_saved(joined)
identical(saved, reused)

fresh <- collect_saved(joined, refresh = TRUE)
```

The second call reads the saved Parquet result without submitting a data query,
provided no daily refresh boundary has passed. Ordinary saved results default
to temporary storage. `refresh = TRUE` fetches a new result. A per-call policy or
`lake_tbl(..., freshness = ...)` can override application freshness settings;
see `?collect_saved`.

## Keep a reproducible snapshot

```r
baseline <- collect_saved(joined, snapshot = "baseline", dir = "snapshots")
again <- collect_saved(joined, snapshot = "baseline", dir = "snapshots")
identical(baseline, again)
```

This keeps a durable Parquet file in the project's `snapshots/` directory.
Repeating the same query, label and access context reuses it indefinitely. Use
a new label to keep a new copy. Snapshots do not automatically refresh or expire.

## Use SQL or Arrow

```r
lake_query("SELECT id, s_empty FROM types WHERE id = 1")
arrow_rows <- lake_query("SELECT * FROM types", as = "arrow")
arrow_rows$schema
```

Both use the managed connection. Arrow retains original types and precision;
see `?ckdata_types` for R conversion details. An empty result keeps its columns:

```r
empty <- types |> filter(id < 0L) |> collect()
colnames(empty)
nrow(empty)
```

## Finish the rehearsal

You can keep using the managed connection throughout the session. The fixture's
additional role renews while its AWS source permits. To explicitly close the
connection or change settings, disconnect and rebuild your queries:

```r
lake_disconnect()
types <- lake_tbl("types")
selected <- types |> filter(id == 1L) |> select(id, s_empty)
collect(selected)
```

Old lazy queries retain the old connection. A new connection can reuse valid SSO
credentials. Set `options(ckdata.login = FALSE)` before opening a connection
to fail when sign-in is needed; scripts already default to that mode.

Record unclear messages, unnecessary connection knowledge or surprising query
execution with the step and package version. The separate
[QA acceptance runbook](qa-walkthrough.md) checks identities, DBI behavior and
expected failures. Neither runbook authorizes querying a real research dataset.
