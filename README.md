# Temper

**Temper is an Ethereum Transaction Simulator. It provides a super simple HTTP API which simulates a given transaction request against a local EVM.**

[![test](https://github.com/EnsoFinance/temper/actions/workflows/test.yaml/badge.svg)](https://github.com/EnsoFinance/temper/actions/workflows/test.yaml)

![cover](cover.jpg)

## 📫 API 📫

### POST /api/v1/simulate

Simulates a single transaction against a local EVM.

[See the full request and response types below.](#types)

Example body:

```json
{
  "chainId": 1,
  "from": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
  "to": "0x66fc62c1748e45435b06cf8dd105b73e9855f93e",
  "data": "0xffa2ca3b44eea7c8e659973cbdf476546e9e6adfd1c580700537e52ba7124933a97904ea000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001d0e30db00300ffffffffffffc02aaa39b223fe8d0a0e5c4f27ead9083c756cc200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000186a0",
  "gasLimit": 500000,
  "value": "100000",
  "blockNumber": 16784600
}
```

Example response:

```json
{
  "gasUsed": 214622,
  "blockNumber": 16784600,
  "success": true,
  "trace": { ... },
  "logs": [ ... ],
  "exitReason": "Return"
}
```

Notes:

- `blockNumber` can be omitted and the latest block will be used, however providing a `blockNumber` is recommended where possible to use the cache.
- Simulation requests may include `request_id`; responses echo the same value, or `null` when omitted.

### POST /api/v1/simulate-bundle

Simulates a bundle of transactions.

The server first attempts to simulate every transaction through QuickNode. If any
transaction cannot be handled by QuickNode, the whole bundle falls back to the
local workflow and runs sequentially on the same fork, so later transactions can
observe state changes from earlier transactions.

[See the full request and response types below.](#types)

Example body:

```json
[
  {
    "chainId": 1,
    "from": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
    "to": "0x66fc62c1748e45435b06cf8dd105b73e9855f93e",
    "data": "0xffa2ca3b44eea7c8e659973cbdf476546e9e6adfd1c580700537e52ba7124933a97904ea000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001d0e30db00300ffffffffffffc02aaa39b223fe8d0a0e5c4f27ead9083c756cc200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000186a0",
    "gasLimit": 500000,
    "value": "100000",
    "blockNumber": 16784600
  }
]
```

Example response:

```json
[
  {
    "gasUsed": 214622,
    "blockNumber": 16784600,
    "success": true,
    "trace": [ ... ],
    "logs": [ ... ],
    "exitReason": "Return"
  }
]
```

Notes:

- The request body must contain at least one transaction.
- All transactions must use the same `chainId`.
- For local fallback, block numbers must be non-decreasing.
- Each transaction may include `request_id`; its response echoes the same value, or `null` when omitted.

### POST /api/v1/simulate-stateful

Starts a warm simulation session backed by a reusable fork/EVM context.

This endpoint is intentionally warm-stateless: it reuses fork/session setup for performance, but simulated transaction effects are not committed between requests. Use it for repeated independent simulations against the same chain/block context, not for sequential flows where transaction N+1 must observe state changes from transaction N.

[See the full request and response types below.](#types)

Example body:

```json
[
  {
    "chainId": 1,
    "gasLimit": 500000,
    "blockNumber": 16784600
  }
]
```

Example response:

```json
[{
  "statefulSimulationId": "aeb708a5-81d7-4126-a0b5-0f2a78b3830e",
}]
```


### POST /api/v1/simulate-stateful/{statefulSimulationId}

Runs simulations against the warmed EVM session referred to by the UUID in the URL.

Simulation results are returned to the caller, but transaction state changes are not committed back into the session. For example, an approval simulation followed by a swap simulation will not make the swap observe the approval unless that allowance already exists in the fork state or is provided through `stateOverrides`.

Each transaction may include `request_id`; its response echoes the same value, or `null` when omitted. The session creation endpoint does not use `request_id`.

[See the full request and response types below.](#types)

Example body:

```json
[
  {
    "chainId": 1,
    "from": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
    "to": "0x66fc62c1748e45435b06cf8dd105b73e9855f93e",
    "data": "0xffa2ca3b44eea7c8e659973cbdf476546e9e6adfd1c580700537e52ba7124933a97904ea000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001d0e30db00300ffffffffffffc02aaa39b223fe8d0a0e5c4f27ead9083c756cc200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000186a0",
    "gasLimit": 500000,
    "value": "100000",
    "blockNumber": 16784600
  }
]
```

Example response:

```json
[{
  "gasUsed": 214622,
  "blockNumber": 16784600,
  "success": true,
  "trace": { ... },
  "logs": [ ... ],
  "exitReason": "Return"
}]
```
Notes:

- `chainId` must be the same in all transactions.
- `blockNumber` can be included and incremented when a multi-block simulation is required, or omitted in all transactions to use latest.
- Transaction effects are not persisted between requests in this warm-stateless mode.


### DELETE /api/v1/simulate-stateful/{statefulSimulationId}

Ends a current stateful simulation, freeing associated memory.

[See the full request and response types below.](#types)

Example response:

```json
{
  "success": true
}
```



### Authentication

If you set an `API_KEY` environment variable then all calls to the API must be accompanied by a `X-API-KEY` header which contains this API Key.

## 🏃‍♂️ Running 🏃‍♂️

### Locally

Copy `.env.example` to `.env`, fill out required values and run:

```bash
$ cargo run
```

If you want the server to restart on any code changes run:

```bash
$ cargo watch -x run
```

## 🧪 Test 🧪

Run:

```bash
$ cargo test
```

### Manual Testing

`body.json` contains a simple request in the root of the project so once the API is running you can just run:

```bash
$ curl -H "Content-Type: application/json" --data @tests/body.json http://localhost:8080/api/v1/simulate
```

If you have `jq` installed, you can run this to see pretty traces:

```bash
$ curl -H "Content-Type: application/json" --data @tests/body.json http://localhost:8080/api/v1/simulate | jq -r ".formattedTrace"
```

## 🧭 Roadmap 🧭

- [x] Support any RPC endpoint, not just Alchemy
- [ ] Connect to local node via IPC
- [ ] Connect to local [reth](https://github.com/paradigmxyz/reth/) DB
- [ ] Reintroduce bundle simulation as a dedicated sequential feature if needed.
- [ ] Support more authentication methods

### Contributing

[See CONTRIBUTING.md](CONTRIBUTING.md).

## Types

```typescript
export type SimulationRequest = {
  request_id?: string;
  chainId: number;
  from: string;
  to: string;
  data?: string;
  gasLimit: number;
  value: string;
  accessList?: AccessListItem[];
  blockNumber?: number; // if not specified, latest used,
  stateOverrides?: Record<string, StateOverride>;
  formatTrace?: boolean;
};

export type AccessListItem = {
  address: string;
  storageKeys: string[];
};

export type StateOverride = {
  balance?: string;
  nonce?: number;
  code?: string;
  state?: Record<string, string>;
  stateDiff?: Record<string, string>;
};

export type SimulationResponse = {
  request_id: string | null;
  simulationId: string;
  gasUsed: number;
  blockNumber: number;
  success: boolean;
  trace: CallTrace[];
  logs?: Log[];
  exitReason?: InstructionResult;
  bytes: string;
  formattedTrace?: string;
};

export type Log = {
  topics: string[];
  data: string;
  address: string;
};

export type CallTrace = {
  callType: CallType;
  from: string;
  to: string;
  value: string;
};

export enum CallType {
  CALL,
  STATICCALL,
  CALLCODE,
  DELEGATECALL,
  CREATE,
  CREATE2,
}

export enum InstructionResult {
  //success codes
  Continue,
  Stop,
  Return,
  SelfDestruct,

  // revert code
  Revert, // revert opcode
  CallTooDeep,
  OutOfFund,

  // error codes
  OutOfGas,
  OpcodeNotFound,
  CallNotAllowedInsideStatic,
  InvalidOpcode,
  InvalidJump,
  InvalidMemoryRange,
  NotActivated,
  StackUnderflow,
  StackOverflow,
  OutOfOffset,
  FatalExternalError,
  GasMaxFeeGreaterThanPriorityFee,
  PrevrandaoNotSet,
  GasPriceLessThenBasefee,
  CallerGasLimitMoreThenBlock,
  /// EIP-3607 Reject transactions from senders with deployed code
  RejectCallerWithCode,
  LackOfFundForGasLimit,
  CreateCollision,
  OverflowPayment,
  PrecompileError,
  NonceOverflow,
  /// Create init code exceeds limit (runtime).
  CreateContractLimit,
  /// Error on created contract that begins with EF
  CreateContractWithEF,
}
```

## 🙏 Thanks 🙏

 - Leverages a lot of crates from [Foundry](https://github.com/foundry-rs/foundry)
 - Inspired by [gakonst's example pyrevm](https://github.com/gakonst/pyrevm)

### POST /api/v1/simulate_bundle_v2

Simulates an **ordered sequence with shared state** using QuickNode `trace_callMany`.
The body is an array of 1–20 `SimulationRequest` objects. Repeated senders are
allowed. Every call must use the same `chainId`; explicit `blockNumber` values
must agree. If any call specifies a block it is used for the whole sequence;
otherwise the server resolves `eth_blockNumber` once before simulation.

```json
[
  {
    "request_id": "approval",
    "chainId": 1,
    "from": "0x5EB168ef0481801CF87887DF0FcA1cbAC88a744b",
    "to": "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    "data": "0x095ea7b3000000000000000000000000610b463d2f57d2e0d9e785a7ff423fbae36f06240000000000000000000000000000000000000000000000000000000005f5e100",
    "gasLimit": 1000000,
    "value": "0"
  },
  {
    "request_id": "transfer",
    "chainId": 1,
    "from": "0x610b463D2f57d2e0D9E785A7ff423FbAe36f0624",
    "to": "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    "data": "0x23b872dd0000000000000000000000005eb168ef0481801cf87887df0fca1cbac88a744b000000000000000000000000610b463d2f57d2e0d9e785a7ff423fbae36f06240000000000000000000000000000000000000000000000000000000000989680",
    "gasLimit": 1000000,
    "value": "0"
  }
]
```

Returns `SimulationResponse[]` in input order, with `simulationId` starting at 1
and `request_id` echoed unchanged. `logs` are reconstructed from VM opcode traces
and have the standard `{address, topics, data}` shape. `trace` contains committed
execution frames suitable for native movement analysis; failed subtrees are
excluded and delegatecall/callcode values are zeroed because those frames do not
transfer native currency. Raw `vmTrace` is not returned.

If provider trace decoding fails (observed with inconsistent BSC Testnet VM
nesting), the server replays the entire ordered bundle once through
`debug_traceCallMany` with `callTracer` and `withLog: true`, at the same pinned
block and end-of-block state (`transactionIndex: -1`). Replay calls must match
all original Parity frames, inputs, values, success flags and outputs before
committed logs are accepted. The original trace, stateDiff and gas accounting
remain authoritative. Providers without this RPC or inconsistent replay results
return 502. Recovery currently verifies call frames; create/suicide recovery is
conservatively rejected. This can add one RPC round trip only on decode failure
and shares the existing overall 45-second deadline.

The calls are distinct simulated transactions: state from each successful call is
visible to the next, while a reverted call's execution effects are rolled back.
A failure is reported per call; it does not make earlier transactions atomic with
later ones. The provider continues the sequence using its transaction semantics.
Inspect each `success` before interpreting results.

This endpoint requires `BASE_BLOCKCHAIN_NODE_URL`; it uses
`{BASE_BLOCKCHAIN_NODE_URL}/{chainId}?provider=quicknode`. Provider errors return
502, absent configuration returns 503, and the overall RPC/decode deadline is
45 seconds (504). There is **no fallback to independent simulation or local
warm-stateless EVM**. Existing `/simulate` and `/simulate-bundle` remain unchanged.

Limits: 2 MiB request, 32 MiB provider response, 30 million gas per call (also the
omitted default), and 100 million total gas. Set explicit gas limits for larger
batches. State overrides and `allowInsufficientFunds: true` are rejected on this
endpoint. `includeStateDiff: true` requests provider state differences per call;
omitting it avoids that extra output. `gasPrice`, when supplied, uses decimal
gwei with at most 9 fractional digits. Chain support depends on the configured
QuickNode endpoint exposing `trace_callMany` and `vmTrace`.

Each result additionally includes `error` (the provider's root execution error,
otherwise `null`) and `gasAccounting: "execution_plus_intrinsic"`. `exitReason`
distinguishes known EVM failures such as `OutOfGas`, `InvalidFEOpcode`, and
`Revert`; unfamiliar execution errors use `FatalExternalError` with the original
`error` retained. Compatibility field `gasUsed` excludes refunds and transaction
gas-floor adjustments because `trace_callMany` does not expose charged total gas.
It must not be used as an exact transaction fee or universal upper bound.
