# Fuzz targets

From this directory, run:

```text
cargo fuzz run decode
cargo fuzz run session_actions
```

`decode` sends arbitrary complete-frame bytes through the bounded checked decoder.
`session_actions` feeds arbitrary chunks, drains effects, and supplies deterministic semantic
errors for external work. Neither target opens a transport or filesystem through the core.
