# Dependency Audit: S3 Target Store

Date: 2026-09-05

## Approved Baseline Revision

The initial Rust 1.85 / `aws-sdk-s3 1.96.0` graph was rejected after
`cargo-audit 0.22.2` reported RUSTSEC-2026-0009 in `time 0.3.45` and
unsoundness advisories RUSTSEC-2026-0002 and RUSTSEC-2026-0253 in
`lru 0.12.5`. The user explicitly selected an MSRV upgrade.

The revised baseline is:

- Rust 1.94.1 minimum;
- exact `aws-sdk-s3 1.145.0`;
- default SDK features disabled;
- `rt-tokio` enabled;
- the adapter's `default-https-client` feature selecting the current AWS-LC /
  Rustls HTTPS client;
- no normal `aws-config` dependency.

The committed lockfile resolves `time 0.3.47`, `lru 0.18.4`,
`aws-smithy-runtime 1.14.0`, `aws-smithy-runtime-api 1.16.0`,
`aws-smithy-http-client 1.4.0`, `aws-runtime 1.9.2`, and `aws-types 1.6.0`.

## Verification

The following checks passed:

```text
cargo +1.94.1 check -p w9pt-fs-storage-s3 --all-features
cargo +1.94.1 test -p w9pt-fs-storage-s3 --all-features --locked
cargo +1.94.1 tree -p w9pt-fs-storage-s3 --all-features -e normal -i ring --locked
cargo audit --file Cargo.lock
```

The inverse `ring` query returned no packages. After test-harness dependencies
were finalized without legacy SDK test utilities, `cargo audit` scanned 340 locked
packages and returned success with no vulnerability or unsoundness findings.
License and whole-workspace validation remain part of task 8.6.
