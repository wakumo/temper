# Request ID Echo Design

## Goal

Simulation endpoints should accept an optional `request_id` on transaction
requests and echo the same value in each matching simulation response. Missing
IDs serialize as `null`.

## Scope

Affected endpoints:

- `POST /api/v1/simulate`
- `POST /api/v1/simulate-bundle`
- `POST /api/v1/simulate-stateful/{statefulSimulationId}`

Not affected:

- `POST /api/v1/simulate-stateful` session creation. It does not accept or
  return `request_id`.

## API Shape

The JSON field name is exactly `request_id`.

`request_id` is optional and string-valued. The simulator does not validate
format or uniqueness.

For single simulation, the response echoes the request value.

For bundle and stateful transaction arrays, each output item echoes the
`request_id` from the transaction that produced it. This lets callers correlate
results even if they do not rely on array position.

If the request omits `request_id`, the response contains `"request_id": null`.

## Implementation

Add `request_id: Option<String>` to `SimulationRequest` with a serde rename for
the snake_case wire field.

Add `request_id: Option<String>` to `SimulationResponse` with the same wire name.

Every `SimulationResponse` builder copies `transaction.request_id.clone()` into
the response. This includes local EVM simulation, QuickNode simulation, and tests
that construct responses directly.

## Tests

Add tests for:

- request deserialization from `request_id`
- response serialization includes `request_id` when present
- response serialization includes `request_id: null` when missing
- QuickNode formatted response echoes `request_id`

Existing API tests should continue to pass after expected responses include the
new nullable response field.
