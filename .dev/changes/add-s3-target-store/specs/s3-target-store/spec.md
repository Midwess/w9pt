# Delta for S3 Target Store

## ADDED Requirements

### Requirement: Separate S3 Adapter Boundary
The system SHALL provide `w9pt-fs-storage-s3` as a separate adapter crate implementing `w9pt_fs_storage::TargetStore` without introducing S3 SDK or runtime dependencies into protocol, semantic, metadata, or backend-neutral content crates.

#### Scenario: Adapter is embedded
- WHEN an application constructs an S3 target and passes it to `ContentRepository`
- THEN file content operations execute through the existing backend-neutral target contract
- AND `w9pt`, `w9pt-fs`, `w9pt-fs-state`, and `w9pt-fs-storage` expose no AWS SDK, Tokio, HTTP, TLS, or credential types

#### Scenario: Non-S3 target is selected
- WHEN an application chooses a memory, local, or future target implementation
- THEN it does not link or configure `w9pt-fs-storage-s3`
- AND persisted content semantics remain defined by `w9pt-fs-storage`

### Requirement: Caller-Owned S3 Client and Runtime
The system SHALL accept a caller-created AWS S3 client and SHALL NOT discover credentials, region, endpoint, TLS, retry, timeout, addressing, or runtime configuration from hidden process state.

#### Scenario: Target is constructed
- WHEN the caller supplies a configured client and checked `S3TargetConfig`
- THEN the adapter validates bucket/provider/body/version bounds and derives safe read/write operation configuration
- AND it does not load environment credentials, shared AWS files, a default region, or a global runtime

#### Scenario: Client configuration is invalid for the deployment
- WHEN TLS, signing, endpoint, credentials, or addressing cannot access the configured bucket safely
- THEN construction or qualification fails explicitly
- AND the adapter does not downgrade to anonymous, insecure, or weaker behavior

### Requirement: Rust 1.94.1 SDK Compatibility Gate
The system SHALL pin and audit an AWS SDK dependency graph that builds and tests on the workspace Rust 1.94.1 baseline before the adapter is accepted.

#### Scenario: Adapter dependency graph is resolved
- WHEN Cargo resolves `aws-sdk-s3` and all Smithy/HTTP/TLS dependencies
- THEN the exact graph is committed and passes `cargo +1.94.1 check/test`
- AND normal workspace crates retain their declared Rust 1.94.1 support

#### Scenario: Safe compatible SDK cannot be maintained
- WHEN the pinned graph has an unresolvable advisory, provider incompatibility, or higher compiler requirement
- THEN implementation stops for an explicit SDK or MSRV decision
- AND the project does not silently weaken conditional-write semantics or raise the baseline

### Requirement: Qualified S3 Provider Profile
The system SHALL advertise writable target guarantees only for Amazon S3 general-purpose buckets or an exact S3-compatible provider profile that has passed concurrency, failure, durability, and range conformance.

#### Scenario: AWS general-purpose bucket is used
- WHEN the configured AWS profile passes live qualification with required permissions and private-prefix policy
- THEN the adapter may advertise the required durable-write, atomic conditional-write, and read-after-write guarantees
- AND qualification records the tested bucket class and client behavior

#### Scenario: Compatible endpoint is used
- WHEN a named provider/version/configuration passes the complete qualification suite
- THEN that exact profile may be enabled
- AND support does not extend automatically to another version, endpoint mode, or provider

#### Scenario: Conditional headers are ignored or weakened
- WHEN an endpoint accepts a request but does not atomically enforce `If-Match` or `If-None-Match`
- THEN writable construction/qualification fails
- AND the adapter advertises no false guarantee

### Requirement: Exact Private Key Mapping
The system SHALL use each repository-generated `ObjectKey` verbatim as the S3 object key without adding another prefix owner, visible filesystem path mapping, listing dependency, or normalization.

#### Scenario: Repository object is requested
- WHEN PUT, HEAD, GET, range GET, or CAS addresses an `ObjectKey`
- THEN every operation sends the exact same key text under the configured bucket
- AND the adapter does not expose or infer a user-visible pathname

#### Scenario: Key cannot be represented safely
- WHEN a key or SDK length conversion exceeds configured bounds or changes identity during request encoding
- THEN the operation fails before dispatch
- AND no alternate normalized key is used

### Requirement: Canonical Opaque ETag Versions
The system SHALL encode the exact S3 ETag used by conditional requests in a bounded, canonical, adapter-tagged `ObjectVersion` and SHALL NOT treat it as a content digest.

#### Scenario: Successful object response supplies an ETag
- WHEN PUT, HEAD, or GET succeeds with a valid bounded ETag
- THEN the adapter returns or compares one canonical token preserving its exact quoting and case
- AND subsequent `If-Match` uses the same ETag value

#### Scenario: Version token is malformed or foreign
- WHEN a caller supplies an empty, oversized, control-containing, noncanonical, trailing, or other-adapter token
- THEN the adapter rejects it before sending a request
- AND it does not fabricate an ETag from timestamps, content, or SDK-local state

#### Scenario: Integrity is verified
- WHEN repository content is loaded
- THEN ETag is used only for concurrency identity
- AND envelope checksums and BLAKE3 digests remain the persistent content-integrity authority

### Requirement: Pre-Body Bounded Complete Reads
The system SHALL reject an oversized complete object using HEAD metadata before requesting or collecting its response body and SHALL return bytes and version from one exact object version.

#### Scenario: Object fits the bound
- WHEN HEAD returns a nonnegative length within `max_bytes` and a valid ETag
- THEN the adapter issues `GetObject` conditioned on that ETag and collects at most the declared length
- AND the returned length and ETag must match the HEAD observation exactly

#### Scenario: Object exceeds the bound
- WHEN HEAD reports a content length above the caller or adapter maximum
- THEN the adapter returns a typed limit error before issuing or polling the body GET
- AND no oversized buffer is allocated

#### Scenario: Object changes between HEAD and GET
- WHEN conditional GET fails because the ETag no longer matches
- THEN the adapter restarts the complete read within a bounded read-only retry limit or returns a typed race error
- AND never combines metadata and bytes from different versions

#### Scenario: Body stream is invalid
- WHEN the body stalls, times out, fails, overruns, or ends before the declared length
- THEN the adapter returns a typed read error
- AND never returns partial bytes as success

### Requirement: Exact Half-Open Range Reads
The system SHALL map a nonempty logical `[start,end)` range to one checked inclusive S3 range and accept only a response proving the exact requested bytes.

#### Scenario: Exact range is returned
- WHEN S3 returns the requested inclusive endpoints, valid total length, matching content length, valid ETag, and exact body bytes
- THEN the adapter returns exactly `end - start` bytes
- AND no bytes outside the logical range are included

#### Scenario: S3 clamps or ignores the range
- WHEN the response contains a shorter EOF-clamped range, a full-object response, inconsistent content range, short body, or long body
- THEN the adapter returns a typed exact-range failure
- AND does not silently clamp or slice the unexpected response into success

#### Scenario: Empty range is requested
- WHEN `start == end`
- THEN the adapter uses metadata lookup to distinguish presence from absence and returns an empty vector only for a proven present object
- AND no invalid HTTP byte-range header is emitted

#### Scenario: Range is unsatisfiable
- WHEN S3 returns `416` for an existing object
- THEN the adapter returns a range error rather than `None`
- AND absence remains a separate proven outcome

### Requirement: Explicit Immutable-Write Ambiguity
The system SHALL represent a create-if-absent request whose commit status is unknown as `PutIfAbsent::Ambiguous` and SHALL resolve it before publishing a dependent manifest.

#### Scenario: Immutable PUT response is lost
- WHEN the request may have committed but no definitive successful or failed response is available
- THEN the S3 adapter returns `PutIfAbsent::Ambiguous`
- AND does not report a definitive target error or ordinary existing-object result

#### Scenario: Ambiguous object matches on readback
- WHEN bounded exact readback finds the desired encoded bytes
- THEN the repository treats the immutable dependency as present and durable
- AND may continue to prepare its dependent manifest

#### Scenario: Ambiguous object differs or cannot be proven
- WHEN readback finds different bytes, proves no object, or cannot complete reliably
- THEN the repository reports immutable collision or typed unresolved ambiguity as appropriate
- AND no dependent manifest is published

### Requirement: Atomic S3 Create-If-Absent
The system SHALL implement immutable creation with exactly one retry-disabled single-part `PutObject` carrying `If-None-Match: *` and qualified transfer-integrity behavior.

#### Scenario: Key is absent
- WHEN S3 durably accepts the complete conditional PUT and returns a valid ETag
- THEN the adapter returns `Created` with the canonical version
- AND an independent client can immediately read the exact bytes

#### Scenario: Key already exists
- WHEN the single conditional request returns `412` and current ETag lookup succeeds
- THEN the adapter returns `AlreadyExists` without replacing the object
- AND the repository verifies the current bytes exactly before reuse

#### Scenario: Concurrent conditional request returns 409
- WHEN S3 reports a conditional request conflict
- THEN the adapter observes current state within a bounded resolution limit
- AND never blindly repeats the mutation through SDK retries

### Requirement: Atomic ETag Compare-and-Swap
The system SHALL implement expected-absence CAS with `If-None-Match: *` and expected-version CAS with `If-Match: <etag>`, dispatching exactly one conditional mutation attempt.

#### Scenario: Expected version is current
- WHEN S3 accepts the conditional replacement and returns a valid ETag
- THEN the adapter returns `Replaced` with the new canonical version
- AND subsequent reads observe the complete replacement bytes

#### Scenario: Expected version is stale
- WHEN one retry-disabled conditional request returns `412`
- THEN the adapter returns `Conflict` with the latest observable optional ETag
- AND it does not overwrite or rebase the newer object

#### Scenario: Expected object is absent
- WHEN present-object CAS receives a proven missing-current-object result
- THEN the adapter returns `Conflict { current: None }`
- AND does not reinterpret absence as authorization failure

### Requirement: No Hidden Mutation Retries
The system SHALL disable AWS SDK automatic retries for conditional write requests and leave immutable readback, head publication readback, and filesystem rebase to their owning layers.

#### Scenario: First mutation commits but response is lost
- WHEN a conditional PUT is dispatched and its response becomes unavailable
- THEN the adapter returns the corresponding ambiguous outcome after one mutation attempt
- AND no hidden second attempt can turn the committed operation into a false `412` conflict

#### Scenario: Read request encounters a transient error
- WHEN an idempotent HEAD or GET is eligible for retry
- THEN only the caller/adapter configured bounded read policy applies
- AND mutation retry settings remain disabled and independent

### Requirement: Conservative S3 Failure Classification
The system SHALL classify S3/SDK observations by operation and commit certainty, preserving ambiguity whenever a write might have committed.

#### Scenario: Definite pre-dispatch failure occurs
- WHEN request construction, invalid configuration, credentials, permission, expected owner, or unsupported behavior fails definitively
- THEN the adapter returns a typed redacted error
- AND does not label the operation committed or missing

#### Scenario: Potentially committed write fails
- WHEN timeout, dispatch uncertainty, connection loss, `408`, `429`, `5xx`, or malformed success metadata occurs after possible mutation dispatch
- THEN immutable creation or CAS returns its ambiguous outcome
- AND upper layers resolve exact state before retry/rebase

#### Scenario: Conditional non-commit is reported
- WHEN a single write attempt receives a documented `412`, `409`, or applicable `404`
- THEN the adapter classifies current state without another mutation attempt
- AND returns conflict only when non-commit is established

### Requirement: Proven Absence Versus Access Denial
The system SHALL return `None` only for a proven missing S3 object and SHALL preserve authorization, expected-owner, and indistinguishable `403` responses as errors.

#### Scenario: Missing key is observable
- WHEN S3 returns a not-found response under permissions that distinguish absence
- THEN complete or range lookup returns `None`
- AND no body or version is fabricated

#### Scenario: Caller lacks absence visibility
- WHEN a missing object is represented as `403` because prefix-scoped list permission is absent
- THEN the adapter returns an authorization/absence-visibility error
- AND does not tell the repository that the object is absent

### Requirement: Write-Through Durability and Visibility
The system SHALL report successful target mutation only after the qualified S3 provider acknowledges the complete object and subsequent independent reads are strongly consistent.

#### Scenario: PUT succeeds
- WHEN the SDK returns a complete successful conditional PutObject response with valid version metadata
- THEN the adapter may acknowledge `Created` or `Replaced`
- AND another independently configured client immediately reads the entire new object

#### Scenario: Provider cannot prove durability or visibility
- WHEN an endpoint cannot demonstrate durable successful acknowledgment or immediate read-after-write behavior
- THEN qualification fails
- AND `TargetGuarantees::REQUIRED` is not advertised

### Requirement: Transfer and Persistent Integrity
The system SHALL use a qualified transfer-integrity/signing profile for S3 requests while retaining repository envelope and BLAKE3 verification for persisted content.

#### Scenario: Object is uploaded
- WHEN the adapter sends a PutObject request
- THEN it uses the selected supported payload-signing or transfer-checksum behavior
- AND successful return does not bypass later repository digest verification

#### Scenario: Compatible provider rejects the checksum profile
- WHEN a provider cannot support the configured SDK integrity behavior
- THEN that provider profile fails or selects an explicitly tested compatible integrity mode
- AND persistent BLAKE3 verification remains enabled

### Requirement: Private Prefix Security and Reachability
The system SHALL require deployment policy that prevents unauthorized mutation of the private prefix and keeps every reachable object immediately readable.

#### Scenario: Runtime role is configured
- WHEN the adapter accesses its repository namespace
- THEN permissions are limited to prefix-scoped `GetObject`, `PutObject`, and the `ListBucket` visibility needed for absence
- AND expected-owner, verified TLS, and Signature Version 4 are used where applicable

#### Scenario: Lifecycle or outside writer targets the prefix
- WHEN expiration, archive transition, unconditional overwrite, delete marker, or external deletion could affect reachable content
- THEN the deployment is unsupported
- AND the adapter does not claim durable readable data

#### Scenario: Test cleanup runs
- WHEN live qualification deletes test objects
- THEN deletion is limited to a validated caller-supplied test bucket and exact unique namespace using separate permission
- AND runtime hot paths still require no delete or general listing operation

### Requirement: Bounded Adapter Resources
The system SHALL bound keys, ETags, complete objects, ranges, body accumulation, read retries, current-state resolution, timeouts, and diagnostic retention before corresponding amplification.

#### Scenario: Numeric conversion overflows
- WHEN a logical offset, length, body size, or SDK integer conversion cannot be represented exactly
- THEN the adapter returns a typed bound/range error before request dispatch or allocation
- AND no wrapping or saturating request is issued

#### Scenario: Retry or resolution bound is exhausted
- WHEN read-only race retry or conditional current-state resolution reaches its configured maximum
- THEN the adapter terminates with a typed error or unresolved ambiguity matching commit certainty
- AND does not loop indefinitely

#### Scenario: Diagnostic is produced
- WHEN an SDK or service operation fails
- THEN errors may retain bounded AWS request identifiers and safe classification
- AND credentials, authorization headers, signed URLs, encryption context, and object bytes are redacted

### Requirement: Layered S3 Conformance
The system SHALL test request mapping, fault classification, target semantics, repository behavior, and live provider guarantees as separate deterministic or opt-in layers.

#### Scenario: Offline tests run
- WHEN the normal workspace suite executes without network credentials
- THEN captured requests prove exact headers, one mutation attempt, response validation, body bounds, ambiguity, and redaction
- AND tests are deterministic

#### Scenario: Repository conformance runs
- WHEN raw and block-split operations use the S3 target test harness
- THEN create, reopen, read, write, truncate, publication, collision, and crash outcomes match the memory reference
- AND persisted keys and format bytes remain unchanged

#### Scenario: Live AWS qualification runs
- WHEN explicit test bucket, region/client configuration, unique namespace, and required-mode variables are supplied
- THEN two independent clients pass target concurrency, read-after-write, conditional-write, range, ambiguity, and durability checks
- AND no bucket is provisioned or non-test prefix is mutated automatically

#### Scenario: Compatible-provider job runs
- WHEN a digest-pinned emulator is started and addressed by two independent clients
- THEN the job verifies the reported provider/version and exercises the complete writable behavior probes against that running artifact
- AND the compatible-provider profile remains explicitly unsupported until its required durability and fault evidence is recorded
- AND emulator success is not treated as proof of Amazon S3 durability
