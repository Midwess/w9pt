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

`FilesystemEngine` accepts each owned `Effect::Filesystem` payload together
with its original `OperationId` and a caller-created `ExecutionContext`.
Read-only work needs the stable client incarnation. Mutations additionally need
a globally stable mutation ID, frozen timestamp, retention horizon, and the
exact current writer fence. The host acquires and renews writer leases through
the state adapter; the engine runs no lease task or clock.

The engine returns an `EngineOutcome`. A terminal outcome contains the exact
`FilesystemResult` or client-safe `FilesystemError` that may be supplied once to
`Session::complete`. An unresolved outcome retains typed state, target, policy,
identity, or fencing authority failure. The host must retry or resolve that
failure before completing the Session operation; an ambiguous commit is never
guessed into an `EIO` response.

Attach policy can call `resolve_attach` to validate the configured filesystem
and self-parented export root at one authoritative revision. The returned
`AttachResult` contains the stable root handle/QID and a capability set derived
from the implemented operation slice, the export ceiling/read-only policy, the
state contract, and target guarantees. The implemented slice covers walk,
portable open/create/release, regular-file read/write/append, directory reads,
getattr/setattr, readlink, fsync, mkdir, symlink, hard link, unlinkat, and
renameat. Fid-based rename/remove, mknod, statfs, xattrs, byte-range locks,
cancellation, and session migration return `EOPNOTSUPP` and are not advertised.

Regular content publication always follows immutable target payloads and
manifest, then one authoritative metadata/result commit. The engine never calls
the standalone object-head publisher. New empty files remain explicitly
unpublished until their first nonempty write or extension. Reads use one
immutable `ContentRef` and the first slice deliberately leaves atime unchanged.

`w9pt::Session` remains connection-affine process memory. Losing the node that
owns it ends the stock 9P connection even though committed filesystem state and
mutation results remain recoverable through another engine instance. Durable
session inbox/outbox state, reconnect, transport gateways, production host
wiring, and additional target/state adapters remain separate work.
