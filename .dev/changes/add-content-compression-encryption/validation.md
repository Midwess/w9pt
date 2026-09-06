# Validation: add-content-compression-encryption

Validated on 2026-09-06 with Rust 1.94.1.

## Executed checks

- `cargo fmt --all -- --check`
- `cargo test -p w9pt-fs-storage` with no optional representation features
- `cargo test -p w9pt-fs-storage --features compression-lz4 --locked`
- `cargo test -p w9pt-fs-storage --features encryption-aes-siv --locked`
- `cargo test -p w9pt-fs-storage --all-features`
- `cargo test --workspace --all-targets --all-features --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo clippy --manifest-path test/Cargo.toml --all-targets --locked -- -D warnings`
- `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked`
- root and standalone license-metadata checks
- root and standalone RustSec audits; the unfixed `rsa 0.9.10` advisory is ignored only after `cargo tree` proves that lock-only package is unreachable
- `./test/run-integration.sh`

The required integration run used the pinned SeaweedFS 4.42 image
`chrislusf/seaweedfs@sha256:f7cbc8bdbbf60a1aaba7d61784a3bdff3ec1e0657f6ad0b26d5b6ab2cd9d0dc6`
and PostgreSQL 18.6. It passed target probes, the plain and eight-mode
Raw/BlockSplit repository matrices, the complete PostgreSQL adapter suite,
generated-key PostgreSQL plus encrypted SeaweedFS reopen/rewrap composition,
and TCP/WebSocket transport fixtures. Teardown removed the scoped containers,
network, and volumes.

## Resource and security evidence

- The representation matrix covers Identity/None, LZ4/None, Identity/SIV and
  LZ4/SIV for both layouts, including sparse branch crossings, retry equality,
  no-op reuse, EOF, partial writes, truncate and re-extension.
- The large synthetic paged test reads one block through a map beyond the old
  flat-manifest limit while retaining bounded depth. Separate tests reject map
  and representation working limits before target mutation.
- Context records are limited to 512 policy bytes, 512 wrapped-key bytes and
  2 KiB total retained bytes. Scans enforce item and retained-byte bounds. Keys
  are unwrapped only into an operation-scoped context; there is no global file-key cache.
- Literal LZ4, KDF, AES-SIV, wrapper and v3 object vectors are checked. Tests
  reject wrong masters, swapped contexts/policies/object keys, ciphertext and
  root tamper, malformed compressed input, earlier format versions and
  immutable byte mismatches.
- Injected failures cover entropy/wrapping, context commit before/after
  publication, transformed payload/page/root puts, and authoritative content
  publication. Reopened readers observe the selected complete base or resolved
  complete replacement.
- Rewrap replay and stale revision/fence behavior are exercised through generic
  state conformance. Memory and live SeaweedFS tests prove that rewrap preserves
  the DEK commitment, `ContentRef`, object names, and every stored object byte.

## External checks not executed

- Live Amazon S3 qualification was not executed because no opt-in AWS bucket,
  owner, region or credentials were supplied. The workflow did not provision
  AWS resources and retains the existing explicit opt-in path.
- PostgreSQL 15, 16 and 17 live targets were not available locally. Their
  required environment-driven matrix remains wired; the executed live target
  was PostgreSQL 18.6.
- This validation is not an independent cryptographic audit, production
  SeaweedFS qualification, restart/durability proof, multi-node fault test or
  capacity benchmark.
