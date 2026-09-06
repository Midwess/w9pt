# Delta for SeaweedFS Storage Integration

## ADDED Requirements

### Requirement: Paged BlockSplit Compatibility Evidence

The required digest-pinned SeaweedFS repository suite SHALL exercise the current
paged BlockSplit format across actual leaf and branch boundaries using bounded
sparse data and independently constructed clients. It SHALL retain the existing
post-probe test-only wrapper and every production qualification guard.

#### Scenario: Live operation crosses a leaf boundary

- WHEN sparse file content occupies blocks on both sides of a 128-slot leaf boundary
- THEN positioned reads and writes return the expected bytes through separately stored mapping pages
- AND the independently reopened root selects the complete published content

#### Scenario: Live operation requires a higher tree level

- WHEN content is placed across the block-index 16383/16384 boundary or a higher actual branch boundary
- THEN the suite exercises branch pages and root growth using the production radix profile
- AND it does not fill the intervening sparse range with payload objects or a dense in-memory file buffer

#### Scenario: Live shrink prunes and later extends

- WHEN a published multi-page file shrinks across a page/subtree boundary and later extends
- THEN independently reopened retained bytes are exact and discarded ranges read as zero
- AND current EOF and final-block padding remain distinct

#### Scenario: Prepared pages are abandoned or publication conflicts

- WHEN a paged preparation is abandoned, its publication return value is discarded, or a stale root loses publication
- THEN independent clients observe the old or fully committed new root according to the existing publication contract
- AND no test resolves block versions through object listing or independent mutable block replacement

#### Scenario: Compatibility suite completes

- WHEN the live paged matrix succeeds
- THEN ordinary SeaweedFS targets remain unqualified and the compatible-provider profile remains unsupported
- AND the result makes no production durability, restart, capacity, global-memory, or multi-node guarantee

#### Scenario: Required integration run terminates

- WHEN the paged suite succeeds or fails
- THEN the existing runner uses scoped prefixes, bounded service readiness, failure logs, and verified Compose teardown
- AND new page objects remain confined to the ephemeral integration target
