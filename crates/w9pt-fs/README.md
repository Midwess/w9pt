# w9pt-fs

`w9pt-fs` is the runtime-neutral filesystem semantic engine for `w9pt`. It
coordinates authoritative metadata through `w9pt-fs-state` and immutable file
content through `w9pt-fs-storage`.

> **Development status:** This crate is unreleased. Its API and semantic
> contracts may change directly, with no deprecated aliases or compatibility
> shims for earlier builds.

The crate does not own transport connections, parse or encode 9P frames, open a
database, issue target-provider SDK calls, read clocks, generate random values,
or start an executor. Hosts provide those dependencies and drive returned
futures.
