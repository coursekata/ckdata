# ckdata 0.0.1

* Adds an Athena DBI backend and dbplyr workflows, including positional query
  parameters and Arrow retrieval.
* Preserves schemas for empty results through the shared Rust and Parquet reader.
* Opens browser SSO sign-in when needed and renews credentials through AWS providers.
* Adds Parquet saved results with freshness policies and named snapshots.
* Remote `median()` and `quantile()` now direct researchers to the explicit
  `approx_median()` and `approx_quantile()` helpers.
