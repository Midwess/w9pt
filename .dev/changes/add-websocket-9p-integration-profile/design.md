# WebSocket 9P Integration Profile Design

## Boundary

The profile is an application example in the standalone `test/` workspace:

```text
HTTP GET /9p + Sec-WebSocket-Protocol: 9p
  -> WebSocket upgrade
  -> one binary message == one complete 9P frame
  -> w9pt::Session::receive_frame
  -> application-owned policy/filesystem effect handling
  -> SeaweedFS S3 API
  -> one Effect::SendFrame == one binary WebSocket message
```

No transport or runtime type crosses into `w9pt` or the filesystem/storage
crates.

## HTTP surface

- `GET /healthz` returns `204 No Content` and performs no S3 operation.
- `/9p` is the only WebSocket route.
- The client must offer `Sec-WebSocket-Protocol: 9p`; the successful upgrade
  selects the same subprotocol.
- Other routes use the framework's normal `404 Not Found` response.
- The initial implementation serves HTTP/1.1 without TLS. TLS termination,
  trusted proxy headers, Origin checks, authentication, and public deployment
  policy remain host concerns outside this fixture.

## Message contract and limits

Every WebSocket binary data message contains exactly one complete 9P frame,
including its four-byte little-endian length. The adapter supplies the owned
message to `Session::receive_frame`; it never concatenates messages or splits a
message into stream fragments. Every core `SendFrame` effect becomes exactly
one binary response message.

WebSocket data messages and frames are capped at 1 MiB, matching the default
hard core frame limit. Axum/Tungstenite handles fragmentation beneath the
message boundary and WebSocket control frames. Text input is unsupported and
closes the connection with WebSocket code 1003. A malformed or invalid 9P frame
closes with code 1002. The adapter processes one incoming message and drains all
resulting effects before reading the next message, providing bounded sequential
backpressure for this demonstration.

## Session lifecycle

Router state owns the cloneable application filesystem and a checked session-ID
allocator. Every successful upgrade reserves one distinct session ID and moves
one new `Session` into the upgrade task. Peer closure calls
`Session::transport_closed`; transport-independent cleanup effects are drained
before the task exits. A core `CloseSession` effect terminates the WebSocket.

The session and namespace remain memory-resident. Process or gateway loss ends
all live sessions, so this is connection-affine demonstration behavior rather
than transparent migration.

## Executables and tests

The standalone integration package is renamed from its TCP-specific name and
defines two explicit binaries:

- `w9pt-tcp-seaweedfs-test`
- `w9pt-websocket-seaweedfs-test`

The new integration test starts the HTTP router on an ephemeral loopback port,
checks `/healthz`, verifies missing-subprotocol rejection, performs the complete
binary 9P lifecycle, verifies the selected subprotocol and S3 bytes, then opens
a separate connection to verify text-message rejection. A bounded graceful
shutdown prevents background tasks from escaping the test.

