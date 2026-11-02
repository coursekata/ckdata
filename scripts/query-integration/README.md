# Synthetic query integration checks

This developer harness uses the reusable fixture environment owned by `coursekata/ck-datalake`, under `environments/ckdata-test`. The environment contains only invented rows and test-scoped identities. The Python probe establishes the real Athena/manifest/Parquet/Arrow path while the Rust reader is developed; it is not the Rust acceptance test.

Obtain the environment's nonsecret deployment contract with `tofu output -json ckdata_test` from its own infrastructure state. Keep the resulting file outside Git. Using a standard AWS session for an operator admitted by that environment:

```sh
uv run --locked scripts/query-integration/check.py --run-live \
  --deployment /absolute/path/to/ckdata-test-deployment.json
```

The contract supplies the region, account, exact role chain, workgroups, database and fixed table. There are no arbitrary SQL or production-role options. The harness rejects non-test namespaces before creating data clients, verifies the source and every assumed identity, then queries as each restricted test reader. Credentials remain in memory.

For each seat it checks literal types, an empty query and the named invented Parquet fixture, including NULL, empty string, exact int64, decimal, timestamp and array values. Table/column discovery uses metadata APIs without submitting a query. It also requires denied access to the other test workgroup and to a known fixture object's source bytes. Manifest entries and downloads are confined to the generated output prefixes. Results must match the workgroup's encryption; only assertion outcomes, zero-row representation and safe failure identifiers are printed.

Three bounded UNLOAD queries run per seat. Athena writes fresh outputs under that seat's test prefix; the environment expires them after three days. The harness has no deletion or infrastructure permissions. A zero-row manifest with no files is recorded as an observation; it supplies no Parquet schema. The Rust reader still needs an explicit schema-recovery design.

Run the same transport and safety checks with fake services, without constructing AWS clients:

```sh
uv run --locked scripts/query-integration/check.py --self-test
```

CI runs only this offline mode. Real internal/enclave-role checks and actual one-hour expiry remain final BB-6315 acceptance after the Rust query path works. This harness deliberately cannot run against those production roles.
