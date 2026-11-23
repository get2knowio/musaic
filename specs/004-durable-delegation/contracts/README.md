# Work and Task contracts
The [specification](../spec.md) defines all nine registered work tools, required fields, schemas, result limits and error codes. The tool catalog is generated from the registry. Agent-facing operation handles carry an operation ID usable across reconnect and a suggested polling interval. Work identifiers never double as operation IDs or Task IDs.

A caller advertises the Tasks extension in `params._meta["io.modelcontextprotocol/clientCapabilities"].extensions`. A long supported call may return `resultType: "task"` with queryable task state. Short calls return their ordinary result. Only the owning principal may get, update or cancel. Terminal tool-domain errors remain completed tool results with `isError`. JSON-RPC execution failures map to failed. Repeated keyed input responses cannot perform the same action twice. Host input cannot authorize a desktop edit.

Protocol conformance uses stdio and an isolated desktop bridge; domain conformance uses race, retry, transaction interruption and restore cases. The [recorded stdio fixtures](fixtures/README.md) identify the client, SDK versions and synthetic inputs.

The [recorded stdio responses](fixtures/README.md) cover both supported profiles and include an input replay fixture.
