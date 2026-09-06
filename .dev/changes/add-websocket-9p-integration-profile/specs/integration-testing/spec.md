# Delta for Integration Testing

## ADDED Requirements

### Requirement: HTTP WebSocket 9P Profile

The standalone integration application SHALL expose an HTTP server whose `/9p`
route upgrades clients offering the `9p` WebSocket subprotocol and carries one
complete 9P frame per binary WebSocket message in each direction.

#### Scenario: HTTP health endpoint

- WHEN a client sends a normal HTTP request to `/healthz`
- THEN the server responds successfully without upgrading or accessing object storage

#### Scenario: Required WebSocket subprotocol

- WHEN a valid WebSocket upgrade request to `/9p` offers the `9p` subprotocol
- THEN the server responds with HTTP 101 and selects `9p`

#### Scenario: Missing WebSocket subprotocol

- WHEN a WebSocket upgrade request to `/9p` does not offer the `9p` subprotocol
- THEN the server rejects the request without creating a 9P session

#### Scenario: Binary 9P lifecycle

- GIVEN digest-pinned SeaweedFS and the WebSocket profile are running
- WHEN a client sends complete binary 9P2000.L frames to negotiate, attach, create, write, reopen, and read a file
- THEN every 9P response is returned as one complete binary WebSocket message
- AND the read bytes exactly match the bytes independently observed through S3

#### Scenario: Independent upgraded connections

- WHEN two clients upgrade separate `/9p` connections
- THEN the host allocates distinct `SessionId` values and independent protocol session state

### Requirement: Bounded WebSocket Transport Behavior

The WebSocket integration adapter SHALL bound message and frame input to the
core frame limit, process data messages sequentially, and close invalid data
without weakening the Sans-I/O session lifecycle.

#### Scenario: Text data rejected

- WHEN an upgraded client sends a text data message
- THEN the server closes the WebSocket with unsupported-data status
- AND it does not pass the text bytes to the 9P session

#### Scenario: Malformed binary frame rejected

- WHEN an upgraded client sends a malformed binary 9P frame
- THEN the server closes the WebSocket with protocol-error status

#### Scenario: Peer closes connection

- WHEN the WebSocket peer closes or disconnects
- THEN the adapter reports transport closure to the session
- AND drains transport-independent cleanup effects before discarding session state

#### Scenario: Dependency isolation

- WHEN the WebSocket profile is built
- THEN HTTP, WebSocket, runtime, and S3 dependencies remain confined to the standalone test workspace

