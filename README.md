# ckdata

[![R-universe version](https://coursekata.r-universe.dev/badges/ckdata)](https://coursekata.r-universe.dev/ckdata)

Query Amazon Athena from R or Python. ckdata runs read-only SQL
queries and brings Parquet results into your analysis through Apache Arrow.

In R, use dplyr and dbplyr to build lazy queries, or use SQL through DBI. In
Python, use SQL and consume results with PyArrow, pandas, Polars or DuckDB.
Both packages share the same Rust query engine.

## Installation and configuration

Follow the [installation and setup guide](docs/installation.md) for package
availability, installation and AWS configuration. R can open your configured
AWS SSO sign-in in the browser when the first connection needs it.

### R

With your CourseKata source configured, a summary query looks like:

```r
library(ckdata)
library(dplyr)

query <- lake_tbl("mart_classes") |> count()

show_query(query)  # Inspect the SQL
collect(query)     # Run it and retrieve a tibble

# Or write SQL directly
lake_query("SELECT 1 AS example")
```

Use `collect_saved()` to reuse a local Parquet result until the source refreshes,
or keep an indefinite snapshot for reproducibility. See the
[usage guide](docs/usage.md#saved-results).

### Python

```python
import ckdata
import pandas as pd
import pyarrow as pa

result = pa.table(ckdata.query("SELECT 1 AS example"))
frame = result.to_pandas(types_mapper=pd.ArrowDtype)
```

Empty results retain their column types. Results do not guarantee row order
across Athena's output files; sort after retrieval when local order matters.

## Documentation

- [Installation and configuration](docs/installation.md)
- [Usage, saved results and query behavior](docs/usage.md)
- [Development, testing and releases](docs/development.md)

R function reference is available through `help(package = "ckdata")`.
ckdata is licensed under the [MIT license](LICENSE).
