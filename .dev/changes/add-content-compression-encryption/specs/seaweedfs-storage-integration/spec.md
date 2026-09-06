# Delta for SeaweedFS Storage Integration

## ADDED Requirements

### Requirement: Protected Representation Compatibility Evidence

The required SeaweedFS repository suite SHALL exercise plain, LZ4-only,
SIV-only and combined representations through the real S3 target implementation
for Raw and paged BlockSplit. It SHALL use bounded public test masters/entropy/data and
retain the existing qualification, isolation and teardown rules.

#### Scenario: Required matrix is selected

- WHEN the required integration job runs representation conformance
- THEN the compression/encryption implementations are explicitly enabled
- AND missing required feature coverage is a job failure rather than a silent skipped matrix

#### Scenario: Independent clients reopen transformed content

- WHEN one client publishes content and another reloads committed file metadata and unwraps it with the supplied test master despite different creation defaults
- THEN selected data is read with its recorded policy and exact logical bytes
- AND current defaults do not reinterpret or rekey the file

#### Scenario: PostgreSQL selects the file key

- WHEN the composed test creates an encrypted file with a generated wrapped DEK
- THEN inode/context/namespace metadata commit in PostgreSQL before encrypted S3 preparation
- AND a new client with no cached DEK recovers the selected key from committed metadata and the external test master

#### Scenario: Losing creation candidate is discarded

- WHEN duplicate or competing create attempts generate different candidates
- THEN only the committed winning context is used for selected S3 content
- AND failed or unresolved candidates do not authorize content publication

#### Scenario: Master is rewrapped

- WHEN the composed test rewraps one file context under a new test master
- THEN the file remains readable from the same ContentRef and S3 objects
- AND the rewrap performs no content-object rewrite

#### Scenario: Paged encrypted content crosses boundaries

- WHEN sparse nonzero content crosses real leaf and branch boundaries
- THEN the suite stores and fetches encrypted mapping pages and transformed payloads through exact references
- AND the scenario remains bounded without dense large-file or tmpfs capacity testing

#### Scenario: Preparation is repeated or abandoned

- WHEN an independent client repeats the same encoding context or a preparation is abandoned before publication
- THEN exact immutable readback and old/new root behavior match deterministic repository conformance
- AND no plaintext fallback or mutable per-block replacement is introduced

#### Scenario: Compatibility evidence completes

- WHEN the representation matrix passes
- THEN ordinary SeaweedFS targets remain unqualified and the compatible profile remains unsupported
- AND this result is not described as a cryptographic audit, production qualification or capacity guarantee

#### Scenario: External AWS configuration is absent

- WHEN opt-in live AWS credentials/bucket configuration are unavailable
- THEN the external check is recorded as not executed
- AND the workflow does not provision resources or present that skip as a successful live AWS result
