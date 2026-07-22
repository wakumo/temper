# QuickNode Simulate Fast Path Design

## Goal

Prefer QuickNode `debug_traceCall` for stateless `/simulate` when configured. Return the existing `SimulationResponse` shape. If QuickNode config is missing, the request needs local-only features, or QuickNode returns an unusable response, fall back to the current local fork/REVM workflow.

## Configuration

QuickNode and the default workflow share one required env var:

- `BASE_BLOCKCHAIN_NODE_URL`

Default workflow URL: `{BASE_BLOCKCHAIN_NODE_URL}/{chainId}`.

QuickNode URL: `{BASE_BLOCKCHAIN_NODE_URL}/{chainId}?provider=quicknode`.

If `BASE_BLOCKCHAIN_NODE_URL` is missing, simulation returns a config error instead of using hardcoded provider URLs.

## Request Flow

`simulate()` attempts QuickNode before constructing `Evm`, so successful QuickNode simulations avoid spawning Anvil or REVM state backends.

`simulate_bundle()` has the same QuickNode-first behavior in the legacy handler, but the public `/simulate-bundle` route is currently disabled. If any transaction is skipped or fails through QuickNode, the handler falls back to the current bundle workflow for the whole bundle to preserve existing sequential semantics.

QuickNode is skipped for requests with `stateOverrides`, because `debug_traceCall` does not apply the existing local override model. It is also skipped when a caller explicitly requests `includeStateDiff = true`; if the field is omitted, QuickNode remains eligible and returns `stateDiff: null`.

## JSON-RPC Call

The request uses `debug_traceCall`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "debug_traceCall",
  "params": [
    {
      "from": "0x...",
      "to": "0x...",
      "value": "0x0",
      "data": "0x...",
      "gas": "0x..."
    },
    "latest",
    {
      "tracer": "callTracer",
      "tracerConfig": { "withLog": true }
    }
  ]
}
```

The block parameter is the request `blockNumber` encoded as hex when provided, otherwise `latest`.

## Response Formatting

The QuickNode `callTracer` tree is converted into the existing response shape:

- `simulationId`: `1`
- `gasUsed`: root `gasUsed`, hex-decoded
- `blockNumber`: request `blockNumber` when present; for `latest`, fetch `eth_blockNumber` from QuickNode and decode it
- `success`: `true` when root has no `error` and no `revertReason`
- `trace`: depth-first flatten of root and nested `calls`
- `logs`: all `logs` from root and nested calls, sorted by their tracer `index` when present
- `exitReason`: `Return` on success, `Revert` on failure
- `returnData`: root `output`, default `0x`
- `stateDiff`: `null`

Each trace item maps:

- `callType`: uppercase `type`, default `CALL`
- `from`: call `from`
- `to`: call `to`
- `functionSignature`: first 4 bytes of `input`, or `0x00000000` if unavailable
- `value`: call `value`, default `0x0`

## Fallback

QuickNode fallback is triggered by:

- missing env config
- request uses state overrides
- request explicitly asks for state diff
- HTTP/RPC error
- malformed or incomplete tracer result

Fallback reuses the current handlers unchanged.

## Tests

Add focused tests for:

- shared-base default workflow and QuickNode URL construction
- `debug_traceCall` request body shape
- flattening nested `calls` into trace items
- collecting nested logs and removing tracer-only fields
- fallback gating for state overrides and `includeStateDiff = true`
