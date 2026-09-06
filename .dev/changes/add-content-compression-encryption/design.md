# Design: Per-File Envelope Encryption and Bounded Compression

## 1. Ownership

```text
Caller supplies one master wrapping key (KEK) and secure entropy
  -> storage helper generates a candidate per-file data key (DEK)
  -> state atomically persists the wrapped DEK and pinned file context
  -> PostgreSQL durably selects the winning metadata
  -> orchestration reloads and unwraps that committed context
  -> file DEK derives naming and per-object encryption keys
  -> storage reads/writes immutable S3 payloads, pages and roots
  -> state transaction publishes ContentRef + inode changes + result
```

`w9pt-fs-state` owns generic file metadata, bounds, identity, transactions,
leases/fences and lifetime rules. The PostgreSQL adapter persists those records.
Neither interprets a compression algorithm, calls a cipher, or sees a plaintext
KEK/DEK. `w9pt-fs-storage` owns policy encoding, key helpers and content transforms;
the filesystem/host orchestration passes values between the layers.

The new direction permits state and SQL schema changes for bounded file-context
metadata. It supersedes the initial proposal's no-state-edit requirement.
Block maps, manifests and physical content representation stay in S3/storage.

## 2. Key Hierarchy and Trust Boundary

A normal deployment provides one identified 32-byte high-entropy master KEK.
The library obtains 32 random bytes for each encrypted file from an explicit
caller-supplied CSPRNG interface. A file DEK is selected once at creation and
retained for all versions of that file in this first implementation.

The KEK derives only a purpose-separated wrapping key. The committed file DEK
independently derives a naming key and separate SIV working keys for each exact
immutable object key. The master ID/material, wrapped bytes and state record
revision MUST NOT enter S3 object names, content KDFs or content AAD. Rewrapping
the same DEK must leave all existing ciphertext and ContentRefs usable.

Master material remains outside state, PostgreSQL and S3. Plaintext DEKs exist
only in bounded storage-owned secret buffers while generation/wrapping or
content access is running. Database records contain wrapped DEKs and public
identity/policy metadata, not raw keys. A database backup alone must not expose
plaintext DEKs without the KEK.

Encryption protects immutable object bodies and key envelopes, subject to the
trusted metadata root and caller. It does not hide PostgreSQL file metadata,
object names/counts/lengths, sparse allocation, access patterns, or compression
length leakage. Standalone clear heads remain trusted publication inputs;
their checksum is not a hostile-store rollback defense. No secure-erasure-on-
unlink or complete process-memory wiping claim is made.

## 3. Generic File-Context State Record

Add a record keyed by `(FilesystemId, content FileId)`:

```text
ContentMetadataRecord:
  owner_inode_id
  content_file_id
  context_id                   # stable opaque ID; also identifies this file's DEK
  policy_format                # generic nonzero format identifier
  policy_bytes                 # bounded opaque storage policy, immutable
  key_commitment?              # opaque fixed-size DEK identity check, immutable
  wrapped_key_bytes?            # bounded opaque envelope; None for plain policy
  record_revision
```

The wrapping envelope is storage-owned and includes its format, master-key ID,
binding information, and authenticated ciphertext. State stores it as opaque
bytes. An encrypted envelope wraps exactly one random file DEK. Its immutable
key commitment is computed by a purpose-separated keyed hash of the DEK and
file/context binding; it is not a password verifier. Storage verifies that
commitment after unwrap and before producing a usable secret context.

Every managed regular inode carries `content_context_id` alongside its existing
content FileId and optional ContentRef. The metadata record binds that same
file/context to its original inode. State compares IDs, opaque policy bindings,
optional commitment shape, bounded bytes and revisions. It never decodes the
policy or wrapping algorithm. Plain files also have a record with pinned policy
and no key envelope/commitment, avoiding first-write default ambiguity.

Proposed bounds: 512 bytes for opaque policy, 512 bytes for the wrapped-key
record, and 2 KiB total retained content metadata. Fixed-size identifiers and
accounting overhead are included. Receiver limits are revalidated and scans
remain bounded by both item count and retained bytes.

Insert a context only together with its matching newly created regular inode.
No separate pending-key reservation or unattached pre-create row is introduced.
An existing context cannot be attached to a different inode or have its identity,
policy or key commitment replaced. Hard links share the same inode/context.

Within a filesystem, context ID and original owner inode are unique as well as
content FileId. Retained records therefore also prevent reuse of old bindings
after inode retirement. Creation validates the owner in the same transition;
a retained record is allowed to outlive that owner's row.

Metadata can remain after the owner inode is retired. There is no cascade from
inode deletion and no public generic Delete/Replace bypass for the protected
context. Only explicit rewrap may change its envelope/revision. A future safe
reachability GC will define removal of detached contexts; this change retains
them conservatively, including keys for old readers, roots and retained results.

## 4. Atomic Creation and Durable Winner

Use the existing unpublished empty-file lifecycle (`content: None`, logical
size/data generation zero). This is a state-contract capability, not a claim
that the unfinished semantic engine already implements full create. Focused
helpers augment a caller-assembled declarative transaction and are tested by
direct state/storage composition; no complete path-based filesystem API is
implied. The protocol is:

1. Capture the semantic create operands, pinned storage policy and stable
   filesystem/inode/content/context identities supplied by the caller.
2. Check the mutation ledger first. An exact committed retry resolves its
   original result instead of allocating another usable key context.
3. Generate a candidate file DEK through the injected entropy interface, wrap
   it under the supplied KEK, compute the key commitment, and zeroize the raw
   candidate. For plain policy, construct only opaque policy metadata.
4. Build one normal declarative metadata transaction inserting the inode,
   ContentMetadata record, directory entry and any open/pin records together,
   with parent/allocation updates, authorization/fence preconditions and result.
5. Resolve commit or ambiguity through the existing ledger/state contract.
   Before success, no encrypted S3 object may be prepared from the candidate.
6. Read the committed inode/context winner from a consistent authoritative
   snapshot, then ask storage to unwrap that exact metadata into an
   operation-scoped FileCryptoContext.
7. Only this context is used for first content preparation and subsequent
   ContentRef publication.

The generation helper returns bounded opaque candidate metadata, not a
storage-ready plaintext-key handle. Storage primitives cannot independently
prove SQL durability: orchestration MUST perform the authoritative selection
before unwrapping a context for file operations.

Random candidate/key-envelope bytes are generated allocation results, not
semantic retry operands. A retry may generate different candidates but must
use the committed winner. Actual requested filenames/flags/attributes and any
explicitly requested policy remain fingerprint-protected operands. Replay must
not conceal a genuinely different operation under the same mutation ID.

A failed create transaction leaves no committed inode or key record. A crash
after durable context creation but before S3 upload leaves a valid empty inode
whose key is recoverable. Competing distinct creates obey existing namespace
conflict semantics; a loser cannot adopt another create's identity unless it
is the exact replay of that same logical operation.

An exact retained create result can also replay after the inode was retired.
Return that original terminal result without requiring a live inode/context
lookup, recreating the inode, or initiating content preparation. Composite
live-inode/context reads are for subsequent live operations; authorized old
roots use the separately retained context point lookup.

Creation with initial payload bytes in one atomic namespace operation is outside
scope. Empty-file create followed by write follows the existing interface and
avoids a separate encrypted-key reservation/recovery protocol.

## 5. First Write, Read and Publication

First write, append-position preparation and first truncate against an inode
with no ContentRef use its already committed context. They must not generate a
key or consult changed creation defaults. Append serialization remains owned
by the filesystem metadata operation; this proposal does not finish unrelated
semantic-engine operations.

Orchestration reads inode plus context under one state snapshot, unwraps the
DEK with the matching supplied KEK, verifies owner/context/policy/commitment,
and supplies a FileCryptoContext to storage. Metadata-only operations need not
unwrap keys. Content access to encrypted files requires a usable context even
when no payload has yet been materialized.

FileCryptoContext retains one file's plaintext DEK/derived state and nonsecret
binding. It is redacted, zeroizing, operation-scoped and not a registry of every
file key. Correctness must survive discarding it between operations. No cache
or external resolver is required; any future cache needs a separate bound and
revision/lifetime contract.

Extend PreparedContent with a nonsecret context binding: file/context identity,
policy binding and optional key commitment. Policy binding means the exact
bounded `policy_format + policy_bytes`, compared byte-for-byte with state;
state need not decode or hash the policy. Crypto helpers may independently
compute a commitment over those canonical bytes for wrapping/KDF purposes.
The metadata publication transaction
compares that binding with the inode and selected ContentMetadata revision,
alongside existing mutation/base/size/generation/fence/authorization checks.
A missing, foreign or stale context prevents publication. This is opaque equality
validation, not decryption in state. ContentRef can retain its existing bounded
root fields; the inode and retained metadata supply its file context separately.

Storage also compares the root/page/payload binding with the supplied committed
context. A successful self-authentication under an unrelated key or policy is
not enough. State never has to inspect S3 ciphertext to enforce publication.

## 6. Master-Key Rewrap

Include an explicit per-record administrative rewrap operation:

1. Read the context record and revision, and look up the administrative mutation
   result before performing new crypto work.
2. Unwrap with the old identified master and verify the immutable DEK commitment.
3. Wrap that same DEK using the new identified master, with the same file/context
   and policy binding. Verify the resulting envelope/commitment locally.
4. Submit a dedicated `RewrapContentMetadata` change with exact expected context
   ID and record revision, current administrative authorization/fence, unchanged
   policy/commitment, and new opaque wrapped bytes.
5. Commit the replacement, revision, change event and terminal result atomically.
   Resolve ambiguity/replay using the existing ledger protocol.

State rejects rewrap on a plain context and rejects changes to immutable
identity/policy/commitment. Generic replacement must not bypass these rules.
The crypto helper establishes that the new envelope contains the same DEK;
state only checks its opaque declared bindings and allowed transition.

Rewrap does not change file bytes, data/inode content generations or ContentRef,
and performs no S3 rewrite. Existing S3 names, ciphertext and AAD depend on the
file DEK/context, never on wrapping metadata. A concurrent content publication
using an old context revision may conservatively conflict and must reload the
new record; it must not generate a replacement DEK.

One active KEK is the ordinary case. Rewrap necessarily needs old and new
master material during transition. A caller may drive bounded record pages
manually; no rotation daemon or instantaneous retirement is promised. Old
masters may still be needed for in-flight snapshots and database backups/WAL.
Actual per-file DEK rotation is different and remains out of scope.

## 7. Cryptographic Profiles and Framing

Keep standard RFC 5297 deterministic AES-256-SIV for file-key wrapping and
immutable content protection, with distinct KDF/AAD domains for those purposes.
Use the reviewed RustCrypto implementation, not a custom cipher. The SIV working
key is 64 bytes; the master KEK and random file DEK are independent 32-byte
high-entropy inputs from which purpose-specific working keys are derived.
[RFC 5297](https://www.rfc-editor.org/rfc/rfc5297.html)

Wrapping KDF/AAD binds filesystem, owner inode, content FileId, stable context
ID, canonical policy binding, key commitment, wrapping format and master-key ID.
Only the master KEK participates in wrapping. No fresh nonce is required for
this deterministic wrapper; fresh randomness is used for candidate DEK generation.

For content, derive a 32-byte naming key from the file DEK with its own domain.
Derive a separate 64-byte SIV working key for each complete immutable object key.
Use BLAKE3's dedicated derivation/XOF mode with independent fixed labels and
unambiguous canonical inputs. Never use a raw password as key material.
[BLAKE3 derivation](https://docs.rs/blake3/latest/blake3/fn.derive_key.html)

The protected preparation token binds file/context identity, complete semantic
PreparationIdentity, attempt and immutable storage policy. It excludes the
master ID, wrapper bytes and state revision. Full preparation provenance is
inside the encrypted body; public keys expose no unkeyed plaintext fingerprint.
Derive the naming token first, then the exact key, then its content working key.
There is no current-ciphertext digest in these KDF inputs, avoiding circularity.

S3 headers identify the stable file/context domain and protection profile,
not the KEK used in PostgreSQL. Authenticate the exact object key and canonical
bounded header. Reused objects bind their own creation context rather than a
new parent generation. Enforce primitive/library size limits and constant-time
cryptographic comparisons. Per-object subkeys are not an unlimited-security
claim for any master or DEK.

The bounded v3 outer header describes kind/version/cipher/context and body
length. Encrypted bodies contain provenance plus metadata or encoded payload
bytes. Plain bodies have a checked unkeyed checksum. No public checksum covers
unencrypted content when encryption is selected, and no AAD field depends on
the ciphertext it helps produce.

## 8. Policy, Compression and Stored Lengths

State pins bounded opaque storage policy at file creation. Storage interprets
it and checks matching policy in S3 root/page/payload bodies. Encryption/key
identity and codec policy do not change on ordinary writes or rewrap.

Supported combinations remain Identity/None, LZ4/None, Identity/SIV and LZ4/SIV.
BlockSplit transforms independent canonical 32 KiB blocks after zero padding
and plaintext hashing. Raw transforms its bounded complete payload; compression
does not remove Raw's full-file RAM cost. All-zero BlockSplit payloads are holes.
Root and mapping page bodies are encrypted but not compressed in this profile.

Lz4BlockV1 uses pinned safe/checked `lz4_flex` block APIs, no dictionary/history
or size-prefixed allocation, and a fixed encoder. Select LZ4 only when it saves
at least 64 bytes; otherwise select actual Identity while retaining encryption.
Execution/resource errors are not an incompressible-data fallback.

Initially support reproducible little-endian 64-bit encoding and verify exact
vectors on supported writer targets. A known policy can be parsed elsewhere;
actual Identity reads do not require the LZ4 feature, while actual compressed
access or new profile encoding requires the relevant implementation. Do not
assume a package pin guarantees identical encoding on every architecture.

Use bounded `compress_into`/`decompress_into`, exact initialized output and
whole-input validation. Reject malformed matches, invalid trailing/truncated
input and excessive output before exposure. The expected plaintext length comes
from trusted metadata, not an attacker-controlled size prefix. Preserve the
reused-buffer regression coverage motivated by the prior LZ4 advisory.

| Value | Meaning |
| --- | --- |
| Canonical plaintext length | Logical Raw bytes or exactly 32 KiB per materialized block |
| BlobRef stored length | Complete v3 object including provenance/header/protection |
| Actual codec length | Bytes of Identity/LZ4 data inside the checked body |
| BlobRef digest | Canonical plaintext BLAKE3, inside protected metadata when encrypted |
| PageRef length/digest | Complete stored mapping-page object |
| ContentRef root digest | Complete stored immutable manifest object |

Exact body fields/tags/AAD/token inputs need independent golden encodings.
Use one current v3 storage format and update the current code-first PostgreSQL
schema/checksum for metadata records. Reject older development data and require
operator recreation; do not add old readers, dual writers or migration paths.

## 9. Read and Preparation Pipeline

Reads first resolve the committed context outside storage. Then:

1. Validate the requested range and exact reference/type-specific target bounds.
2. Fetch the selected compact root/page/payload and verify stored lengths and
   root/page digests before further processing.
3. Match bounded header context to the supplied file context; authenticate and
   decrypt completely, or verify the plain-mode checksum.
4. Validate protected provenance, token, file/location/profile and local page
   structure before following references.
5. Decompress payloads only after authentication and bound validation, requiring
   exact canonical length, digest and relevant EOF padding.
6. Copy the requested bytes and release scratch as the lazy cursor advances.

Preserve page summaries against selected EOF, wrong-range/level rejection and
missing-reference errors. A missing page/key/context is never a hole. Root
validation and content sync authenticate the root without a full-tree scrub.
An old retained ContentRef can resolve its separately retained file context even
if its inode has been retired, under the caller's reader authorization contract.

One shared deterministic encoder is used by Raw/BlockSplit expected payloads,
page/root preflight, actual upload and exact reconciliation. Compression and
content encryption run under the committed DEK and stable policy in both passes.
Known structural/working-size errors still fail before PUTs. Release buffers
rather than retain every first-pass encoding.

Unchanged plaintext and EOF reuse the original reference with no PUTs; changes
to master wrapping metadata alone also never force re-encoding. Full block
writes skip superseded payload reads, while partial/tail operations verify the
needed old bytes. Rewrap revision changes can invalidate publication preconditions
without changing deterministic S3 preparation.

Confirm every new child before parent, then the compact root, then publish
ContentRef and metadata/result atomically. File-context metadata was already
durable before this content preparation began. Failures may leave unreachable
S3 objects, but must never publish content whose key record was a losing or
unconfirmed candidate. No eager S3/context deletion occurs.

## 10. State and PostgreSQL Implementation Surface

Add RecordFamily/RecordKey/StateRecord ContentMetadata variants, a typed bounded
opaque record, exact point read, keyset scan/resume, byte accounting and change
notifications. Add a stable context binding to regular inodes and the prepared
handoff. Generic insert/replace/delete rules enforce creation association,
immutable policy/identity, dedicated rewrap and conservative retention.

Provide a bounded composite inode-with-content-metadata read (for example,
`ReadQuery::InodeWithContentMetadata`) so callers starting with an inode ID
resolve its selected context in one snapshot without a racy second lookup.

The PostgreSQL table is
`public.w9pt_fs_state_content_metadata`, keyed by filesystem/content FileId.
It stores owner inode/context IDs, opaque policy, optional key commitment,
optional wrapped envelope and record revision. Add appropriate regular-inode
context columns and deferred inode-to-metadata constraints. The retained record
must not cascade from inode deletion or require its retired owner to remain.
Validate initial owner existence/atomic creation in state transitions instead.

Use per-filesystem uniqueness for content FileId, context ID and original owner
inode. Enforce the composite inode-to-context binding without a reverse
foreign key that would force retained metadata deletion at final close.

Extend codecs, SQL key tags, bounded query projections, transaction byte
estimates, canonical lock ordering, constraint classification, schema catalogs,
privileges and tests. Reads of inode plus context use one consistent snapshot;
publication/rewrap revalidate revisions within the authoritative transaction.
Do not parse cipher tags, unwrap keys or access S3 in SQL adapter code.

Current unreleased schema changes are applied to the code-first baseline and
its checksum fixtures. Update existing consumers/tests directly. Necessary
filesystem orchestration helpers drive context creation/reload/rewrap but do
not introduce a database SDK into the semantic/storage contracts or complete
unrelated runtime features.

## 11. Memory, Errors and Dependencies

Keys are loaded by exact file lookup; never load a plaintext keyring proportional
to file count. Context scans use existing item/byte limits. Keep at most the
operation's required DEK and working material; rewrap uses two external master
contexts transiently. No persistent all-files key cache is added.

Retain the proposed 64 MiB representation working budget and existing map,
request, Raw, page/root and target bounds. Account encrypted/decrypted copies,
codec candidates/workspace, policy/wrapper parsing, key derivation and exact
PUT/readback buffers. Context records add a separately bounded metadata cost.
Check true v3 overhead rather than the old fixed 53-byte assumption. Budgets
are operation-local, not process RSS/global concurrency guarantees.

Use typed entropy, missing-master, wrapping/authentication, context/commitment,
codec/profile, resource, state-conflict and ambiguity errors. No plaintext key
or unauthenticated data enters errors, state results or traces. Redact secret
Debug; zeroize owned master/DEK/working-key/scratch buffers, with documented
limits for caller memory, compiler temporaries, registers and swap.

Candidate optional dependencies remain `lz4_flex` 0.14.0 (safe/checked block
features), `aes-siv` 0.8.0 (defaults/RNG helpers off; alloc/zeroize), `zeroize`
1.9.0 and existing BLAKE3. The host implements entropy; storage calls that
explicit interface. Validate transitive features, cipher-schedule cleanup,
MSRV, licenses, advisories and supported encoder targets before implementation.
State/PostgreSQL source must not import transformation algorithms even if host
feature selection makes computation crates transitively reachable.

## 12. Required Tests

- Generic opaque state record bounds, family/key identity, exact point/scan
  behavior, atomic inode/context creation and forbidden mutation/deletion.
- Two creators with different random candidates; only the authoritative winner
  is ever unwrapped for S3. Matching replay reloads it; semantic mismatch fails.
- Entropy/wrap failure before create, transaction failure/ambiguity, crash after
  durable context before upload, and failure throughout S3 preparation/publication.
- First write/truncate with content=None and changed defaults uses pinned state
  policy and the same DEK. No new candidate is generated.
- Prepared context/policy/key commitment mismatch, stale context revision and
  writer fence reject publication without modifying current content.
- Rename/hard links/open-unlinked/final close preserve recoverability; metadata
  survives inode retirement. No file-key deletion or crypto-erasure is claimed.
- Rewrap same-DEK validation, stale revision/fence, exact replay/response loss,
  racing content write, wrong master and mixed administrative rollout; S3 keys,
  objects and ContentRefs stay unchanged.
- Both methods in all four modes, primitive/wrapper/object vectors, frozen
  compressor output, malformed compressed/reused buffers and AEAD/context swaps.
- Empty/sparse/EOF/full/partial/no-op/truncate/page-boundary behavior, deterministic
  two-pass equality, exact immutable ambiguity and resource accounting.
- Independent PostgreSQL clients lose all local DEK state and recover from the
  external master plus committed rows. Generic state tests use opaque fixtures;
  actual cryptographic composition belongs in storage/integration tests.
- A create replay after inode retirement returns its retained result without
  inode/context re-creation or new encrypted uploads. Generic state publication
  compares exact bounded policy bytes, not a storage-algorithm computation.
- The standalone composed test owns a private external-test post-probe target
  wrapper; it cannot import the private S3 crate integration-test bridge and
  must not add a production guarantee bypass.
- Explicit feature wiring in S3 Cargo tests, local runner and CI. Required
  bounded SeaweedFS plus PostgreSQL composition; AWS remains opt-in. Use public
  test keys, isolated records/prefixes and verified teardown.

Passing tests is not an independent cryptographic audit. Record actual
resource/security/dependency evidence and any unexecuted external checks.
