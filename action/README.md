# Silicon GitHub Action

Builds `silicon` from source and runs it against the given firmware paths.
Fails the job if any finding meets or exceeds `fail-on`. Optionally uploads
a SARIF report for GitHub code scanning.

```yaml
- uses: yourorg/silicon/action@main
  with:
    paths: src/
    fail-on: error        # error | warning | none
    upload-sarif: 'true'
    # svd: path/to/custom.svd   # defaults to the vendored RP2040 SVD
```
