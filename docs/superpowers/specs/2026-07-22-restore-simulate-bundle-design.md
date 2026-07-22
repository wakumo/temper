# Restore Simulate Bundle Design

## Goal

Re-enable `POST /simulate-bundle` with the current workflow: prefer QuickNode for
fast stateless simulation, then fallback to the legacy local workflow on one fork
when QuickNode cannot handle the full bundle.

## API Behavior

`POST /simulate-bundle` accepts an array of `SimulationRequest` and returns an
array of `SimulationResponse`.

Empty bundles must return a request error instead of panicking.

All transactions must use the same `chainId`. Mixed chain IDs return the existing
`MULTIPLE_CHAIN_IDS` error.

Block numbers keep legacy ordering rules. A later transaction may advance the
fork block. A lower block number returns `INVALID_BLOCK_NUMBERS`.

## Execution Flow

The handler first tries QuickNode for every transaction. If every transaction is
handled by QuickNode, it returns those responses.

If any transaction is skipped or QuickNode returns an error, the handler discards
partial QuickNode results and runs the whole bundle locally.

Local fallback creates one `Evm` from the first transaction chain and block. It
executes every transaction through the existing warm stateless runner using the
same mutable EVM instance. This preserves legacy bundle semantics: later
transactions see state changes from earlier transactions.

## Logging

Logs keep the `ts::api` target and identify whether the bundle used QuickNode or
fell back to local workflow.

## Tests

Replace the disabled-route test with a registered-route test. Add a small empty
bundle test so invalid input returns an error instead of panicking. Keep live
legacy bundle tests ignored unless real RPC is available.
