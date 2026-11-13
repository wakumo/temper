# Direct fork RPC behavior

This change restricts simulation chains and removes the Anvil subprocess from
Temper's local fallback workflow. It restores the upstream architecture of
executing transactions locally against a cached RPC-backed database, without
downgrading the current REVM/Alloy stack or removing QuickNode features.

## Source comparison (2026-11-13)

The inspected upstream is [EnsoBuild/temper at ee12a28c](https://github.com/EnsoBuild/temper/blob/ee12a28c0e3d0610c826c72b0e99ca213aa43e58/src/evm.rs).
Its `Evm::new` constructs `CreateFork` with caching enabled, calls
`Backend::spawn`, and builds a local Foundry executor. It does not spawn Anvil.
Its lockfile pins Foundry to `12ea9f61a344087989f884d2ab735893c77ea576`.

In that Foundry revision:

- [Fork initialization](https://github.com/foundry-rs/foundry/blob/12ea9f61a344087989f884d2ab735893c77ea576/evm/src/executor/fork/init.rs)
  reads the latest block number when needed, gas price, chain ID, and the block.
- [Fork backend](https://github.com/foundry-rs/foundry/blob/12ea9f61a344087989f884d2ab735893c77ea576/evm/src/executor/fork/backend.rs)
  reads balance, nonce and code concurrently for an uncached account; storage
  and block data are fetched on demand. Cached values and in-flight requests
  are reused. This path contains no `eth_getAccountInfo` probe.

Our previous Anvil path launched an external binary installed by unpinned
`foundryup` in Docker. Its actual version was independent of Cargo.lock.
The inspected Foundry v1.3.2 source uses fork-db 0.16.0, whose initial account
fetch races `eth_getAccountInfo` against balance/nonce/code requests. This
explains how such traffic can arise from an Anvil-backed workflow, but does not
identify the binary or caller involved in a historical production log.

## Current execution and methods

The direct backend is explicitly pinned to `foundry-fork-db = "=0.14.0"`.
Its `get_account_req` calls Alloy's balance, transaction count and code methods;
it does not use `get_account_info`. Fork-db 0.16.0 remains a transitive Foundry
dependency, but is not the backend constructed by `Evm::new`.

| Path | RPC methods |
| --- | --- |
| Direct fork initialization | `eth_chainId`, `eth_getBlockByNumber` |
| Uncached account | `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode` |
| Uncached storage slot | `eth_getStorageAt` |
| Uncached historical block hash needed by execution | `eth_getBlockByNumber` |
| Explicit block environment update | `eth_getBlockByNumber` |
| Optional state-difference retrieval | `trace_call` |
| Existing single QuickNode simulation | `eth_blockNumber` when needed, `debug_traceCall` |
| Existing v2 QuickNode bundle | `eth_chainId`, `eth_blockNumber` when needed, `trace_callMany`; `debug_traceCallMany` for supported recovery |

The initial state reads are pinned to the block returned during initialization,
including when the request omits a block number. The existing `client=temper`
query parameter is retained. `FORK_URL`, if set, is used as supplied.

## Request budget and evidence

For one newly created direct fork, excluding retries, approximate RPC count is:

`2 + 3*A + S + B + T`

- `A`: distinct accounts missing from the fork cache.
- `S`: distinct storage slots missing from the fork cache.
- `B`: additional block reads required by execution or explicit block updates.
- `T`: optional `trace_call` requests for state differences.

QuickNode attempts before fallback add their own calls. This is workload
proportional, not a fixed small cost for every transaction. Complex contracts
can touch many accounts and slots. Cache reuse is per backend; no assumption
of cache reuse across independent HTTP requests is needed for this estimate.

For each environment, if one replica handles `q` new forks/second and there are
`r` replicas, approximate direct-fork traffic is `q*r*(2+3*A+S+B+T)` RPC/second.
Idle forks do not poll for account/storage state. At 10x traffic and unchanged
workload/cache behavior, the estimate is 10x. Production rates and latency were
not measured. There is no proven overall multiplier versus Anvil: a successful
one-call account-info fetch can use fewer calls than three standard reads;
removing Anvil removes its probes and process startup, not all state reads.

`tests/fork_rpc.rs` runs the actual `Evm::new`, REVM execution, Alloy HTTP client,
and fork-db dependency against a counting local HTTP server. A contract reads
one storage slot and returns its value. Observed counts:

- First run: 12 RPC requests (2 initialization + 3 accounts * 3 + 1 storage).
- Second run on the same fork: 0 additional RPC requests, same return value.
- A further run requesting state differences adds only one `trace_call` and
  returns the same local execution output.
- All state reads use block 100, and all requests retain `client=temper`.
- The mock rejects unexpected methods, including `eth_getAccountInfo`.

This is a deterministic regression fixture, not a production benchmark or a
claim that all transactions cost 12 requests. No live private node was called.

## Whitelist behavior

`WHITELIST_CHAIN_IDS` defaults to `1,137,56,8217,97` only when absent. An empty
value denies all chains. Invalid entries fail startup; whitespace is accepted.
All POST simulation handlers check the entire request before initiating RPC or
looking up a stateful session. A rejected chain returns HTTP 400:

```json
{"code":400,"message":"CHAIN_ID_NOT_SUPPORTED"}
```

`tests/chain_whitelist.rs` verifies all five POST routes, mixed bundles with an
allowed first call and rejected later call, explicit `FORK_URL`, and zero RPC
requests for rejected inputs. Session cleanup remains available. Config tests
cover defaults, explicit overrides, empty lists and invalid input.
