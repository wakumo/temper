# BSC Testnet corrupt vmTrace regression

Captured on 2026-09-09 from the configured QuickNode provider, chain 97,
block 129961533 (`0x7bf0e3d`), reported client `Geth/v1.7.7/linux-amd64/go1.25.12`.

- `bundle_v2_bsc97_requests.json`: exact three-call reproduction (ERC20 approve,
  Permit2 approve, Universal Router execute); no transactions were broadcast.
- `bundle_v2_bsc97_debug_many.json`: complete `debug_traceCallMany` response,
  one bundle, all three calls, pinned block and `transactionIndex: -1`, with
  `callTracer` and `withLog: true`.
- `bundle_v2_bsc97_invalid_vm.json`: recorded `trace_callMany` response trimmed
  for tests. All Parity frames are preserved. VM bytecode and unused fields are
  removed because opcode names are present. The third call's root VM ops end
  immediately after its first CALL (848 ops). That child has 34 ops and no CALL,
  while its matching Parity frame `[0]` has four children. The decoder errors at
  this child before reaching the omitted root suffix, identically to the full
  response. This is an intentionally invalid VM fixture, not a complete VM trace.

The full raw 15 MB response and probe responses are retained locally under
`artifacts/trace-callmany-bsc97-2026-09-09/`. Both vmTrace-only and
trace+vmTrace+stateDiff returned the same inconsistent nesting. The recovery
must verify replay frame identity, input/value, success and output, then use
committed callTracer logs; it must not guess how to rearrange corrupted opcodes.

`bundle_v2_bsc97_expected_logs.json` contains independent expected logs captured
from `eth_simulateV1` for the same three calls on top of the same pinned state.
All eight logs exactly match the recovered Temper response. This second RPC is
used only as a verification oracle, not by the production recovery path.
