# Delta for SeaweedFS Storage Integration

## ADDED Requirements

### Requirement: SeaweedFS Behavioral Evidence Boundary
The system SHALL treat digest-pinned SeaweedFS integration results as behavioral compatibility evidence only and SHALL preserve every production qualification guard.

#### Scenario: Exact provider artifact starts
- WHEN the integration runner starts the configured SeaweedFS artifact
- THEN it verifies the exact image digest selection and reported `SeaweedFS 30GB 4.42` identity before storage tests run
- AND a provider identity mismatch fails the required integration run

#### Scenario: Behavior probes pass
- WHEN two independently constructed S3 clients pass the target and pair behavior probes against SeaweedFS
- THEN repository compatibility tests may use a private test-only wrapper around those candidates
- AND the ordinary `S3Target` candidates remain unqualified and advertise no writable guarantees

#### Scenario: Repository compatibility tests pass
- WHEN every Raw and BlockSplit compatibility scenario succeeds
- THEN `S3ProviderProfile::qualified_compatible("SeaweedFS", "4.42")` still returns `UnsupportedProviderProfile`
- AND the result is not described as production durability, restart, multi-node, TLS, lifecycle, or broad S3-compatible-provider qualification

### Requirement: SeaweedFS Repository Compatibility Matrix
The system SHALL exercise both persisted storage methods through the real S3 target implementation and two independently constructed clients against the running SeaweedFS artifact.

#### Scenario: Raw lifecycle executes
- WHEN the Raw repository creates, publishes, reopens, reads, overwrites, extends beyond EOF, shrinks, and re-extends content
- THEN every independently reopened version matches the byte-vector model and exact logical EOF
- AND discarded bytes never reappear after re-extension

#### Scenario: BlockSplit lifecycle executes
- WHEN the BlockSplit repository operates on content spanning multiple 32 KiB blocks
- THEN within-block and cross-block reads and writes match the byte-vector model
- AND its persisted method remains BlockSplit when reopened by a repository configured to create new files as Raw

#### Scenario: Independent client observes publication
- WHEN one client publishes a complete repository version
- THEN the other independently constructed client immediately reopens the same standalone head and reads exactly that complete version
- AND it never observes a mixture of manifests or blocks from different generations

#### Scenario: Existing immutable preparation is reused
- WHEN an independent client repeats an identical deterministic preparation whose immutable objects already exist
- THEN the repository verifies exact bytes before reuse
- AND any different bytes at the same key are reported as collision or corruption rather than accepted

### Requirement: Live BlockSplit Boundary Coverage
The system SHALL cover representative multi-object BlockSplit boundaries against SeaweedFS with bounded deterministic payloads.

#### Scenario: Partial write crosses a block boundary
- WHEN an unaligned positioned write spans two adjacent logical blocks
- THEN both affected blocks are reconstructed and verified correctly
- AND unaffected blocks retain their exact prior bytes

#### Scenario: Full block is overwritten
- WHEN one aligned 32 KiB logical block is completely replaced
- THEN the resulting content contains the new block exactly
- AND the operation does not require old block bytes for the overwritten range

#### Scenario: Sparse gap and zero block are written
- WHEN a write occurs beyond one or more absent blocks or a materialized block becomes all zero
- THEN reads return zeros for the logical holes
- AND the manifest does not require payload objects for missing or all-zero blocks

#### Scenario: Final retained block is truncated and re-extended
- WHEN content shrinks inside a materialized block and later extends beyond that point
- THEN the discarded tail remains zero
- AND no physical padding is exposed beyond logical EOF

#### Scenario: Read reaches EOF
- WHEN a range begins before logical EOF and requests bytes beyond it
- THEN the repository returns only the logical bytes through EOF
- AND physical 32 KiB padding is never returned

### Requirement: Compose-Safe Publication Boundary Evidence
The system SHALL test deterministic repository publication boundaries without claiming timing-dependent transport or durable-restart behavior from the single-node tmpfs service.

#### Scenario: Preparation is abandoned before publication
- WHEN immutable payloads and a manifest are prepared and local preparation state is discarded before the standalone head changes
- THEN an independent client continues to observe the complete old published version
- AND unreachable immutable preparation objects do not become visible content

#### Scenario: Publication return value is discarded
- WHEN a head publication succeeds and the publishing process discards the returned publication value
- THEN an independent client reopens the complete new version from target state
- AND it observes no incomplete content mixture

#### Scenario: Two stale preparations compete
- WHEN two clients prepare distinct updates from the same published base and one publishes first
- THEN the second stale publication conflicts without overwriting the winner
- AND rereading, repreparing against the winner, and publishing produces one valid serial result

#### Scenario: Transport ambiguity needs fault injection
- WHEN response loss, timeout, malformed success, or after-dispatch connection failure must be tested
- THEN deterministic captured-SDK tests remain the authoritative evidence
- AND the Compose suite does not use container-kill timing to claim an exact ambiguous outcome

### Requirement: Required and Isolated SeaweedFS Automation
The system SHALL run SeaweedFS target and repository integration in explicit required mode with bounded setup, isolated state, useful failure evidence, and scoped teardown.

#### Scenario: Required environment is incomplete
- WHEN the required integration mode lacks its endpoint, bucket, provider, version, or test namespace
- THEN the test fails rather than returning early as a skip

#### Scenario: Provider readiness fails
- WHEN SeaweedFS does not become ready within the configured attempt and time bounds
- THEN the runner fails and prints bounded provider diagnostics
- AND it does not wait indefinitely

#### Scenario: Scenarios share one ephemeral bucket
- WHEN Raw, BlockSplit, publication-boundary, and concurrency tests execute
- THEN each uses a distinct validated child prefix and non-overlapping stable identities
- AND parallel execution cannot reuse another scenario's mutable head

#### Scenario: Integration run terminates
- WHEN the suite succeeds, fails, or is interrupted through the runner's handled exit path
- THEN Docker Compose removes only the unique integration project's containers, network, ephemeral volumes, and orphans
- AND no broad bucket cleanup operation targets external data
