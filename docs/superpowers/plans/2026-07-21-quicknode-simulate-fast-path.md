# QuickNode Simulate Fast Path Implementation Plan

> Superseded config note: QuickNode URL generation no longer uses `QUICKNODE_API_KEY` or `QUICKNODE_BASE_URL_*`. Current behavior uses `BASE_BLOCKCHAIN_NODE_URL/{chainId}?provider=quicknode`; default workflow uses `BASE_BLOCKCHAIN_NODE_URL/{chainId}`. See `docs/superpowers/plans/2026-07-21-unified-node-url-generation.md`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prefer QuickNode `debug_traceCall` for stateless simulations, format its callTracer output into the current response shape, and fall back to the existing local fork workflow on any miss or error.

**Architecture:** Add a focused `src/quicknode.rs` module that owns env URL discovery, JSON-RPC request construction, callTracer parsing, and QuickNode simulation. `src/simulation.rs` stays responsible for route orchestration and calls QuickNode before constructing `Evm`, preserving existing fallback behavior. State diff remains unsupported in the QuickNode path and returns `null` unless callers explicitly set `includeStateDiff=true`, which forces local workflow.

**Tech Stack:** Rust 2021, Alloy providers JSON-RPC client, serde/serde_json, Warp handlers, cargo tests.

---

## File Structure

- Create: `src/quicknode.rs` - QuickNode config, request building, response formatting, and tests.
- Modify: `src/lib.rs` - expose `quicknode` module.
- Modify: `src/simulation.rs` - try QuickNode before old `Evm` flow for `simulate()` and `simulate_bundle()`.

---

### Task 1: Add QuickNode Config and Request Body Helpers

**Files:**
- Create: `src/quicknode.rs`

- [ ] **Step 1: Write failing helper tests**

Create `src/quicknode.rs` with this initial test module and minimal imports:

```rust
use std::env;

use alloy::primitives::{Address, Bytes, U256};
use serde_json::Value;

use crate::simulation::{PermissiveUint, SimulationRequest};

const BSC_CHAIN_ID: u64 = 56;
const DEFAULT_GAS_LIMIT: u64 = 30_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickNodeSkip {
    MissingConfig,
    UnsupportedChain,
    StateOverrides,
    StateDiffRequested,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn request(chain_id: u64) -> SimulationRequest {
        SimulationRequest {
            chain_id,
            from: Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            to: Address::from_str("0x0000000000000000000000000000000000000002").unwrap(),
            data: Some(Bytes::from(vec![0x12, 0x34, 0x56, 0x78, 0xaa])),
            gas_limit: Some(21_000),
            value: Some(PermissiveUint(U256::from(7_u64))),
            access_list: None,
            block_number: Some(111_247_671),
            state_overrides: None,
            format_trace: None,
            allow_insufficient_funds: None,
            include_state_diff: Some(false),
            gas_price: None,
        }
    }

    #[test]
    fn quicknode_url_uses_bsc_base_and_api_key() {
        temp_env::with_vars(
            [
                ("QUICKNODE_BASE_URL_BSC", Some("https://example.quiknode.pro/")),
                ("QUICKNODE_API_KEY", Some("secret")),
            ],
            || {
                let url = quicknode_url(56).unwrap();
                assert_eq!(url, "https://example.quiknode.pro/secret");
            },
        );
    }

    #[test]
    fn quicknode_url_skips_unsupported_chain() {
        temp_env::with_vars(
            [
                ("QUICKNODE_BASE_URL_BSC", Some("https://example.quiknode.pro")),
                ("QUICKNODE_API_KEY", Some("secret")),
            ],
            || {
                assert_eq!(quicknode_url(1), Err(QuickNodeSkip::UnsupportedChain));
            },
        );
    }

    #[test]
    fn debug_trace_call_body_matches_quicknode_curl_shape() {
        let body = debug_trace_call_body(&request(56));

        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(body["method"], "debug_traceCall");
        assert_eq!(body["params"][0]["from"], "0x0000000000000000000000000000000000000001");
        assert_eq!(body["params"][0]["to"], "0x0000000000000000000000000000000000000002");
        assert_eq!(body["params"][0]["value"], "0x7");
        assert_eq!(body["params"][0]["data"], "0x12345678aa");
        assert_eq!(body["params"][0]["gas"], "0x5208");
        assert_eq!(body["params"][1], "0x6a18137");
        assert_eq!(body["params"][2]["tracer"], "callTracer");
        assert_eq!(body["params"][2]["tracerConfig"]["withLog"], true);
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test quicknode_url_uses_bsc_base_and_api_key debug_trace_call_body_matches_quicknode_curl_shape`

Expected: FAIL because `quicknode_url` and `debug_trace_call_body` are not defined.

- [ ] **Step 3: Add minimal helper implementation**

Add this code above the test module in `src/quicknode.rs`:

```rust
fn quicknode_base_env(chain_id: u64) -> Result<&'static str, QuickNodeSkip> {
    match chain_id {
        BSC_CHAIN_ID => Ok("QUICKNODE_BASE_URL_BSC"),
        _ => Err(QuickNodeSkip::UnsupportedChain),
    }
}

pub fn quicknode_url(chain_id: u64) -> Result<String, QuickNodeSkip> {
    let base_env = quicknode_base_env(chain_id)?;
    let base_url = env::var(base_env).map_err(|_| QuickNodeSkip::MissingConfig)?;
    let api_key = env::var("QUICKNODE_API_KEY").map_err(|_| QuickNodeSkip::MissingConfig)?;
    Ok(format!("{}/{}", base_url.trim_end_matches('/'), api_key))
}

fn hex_u64(value: u64) -> String {
    format!("0x{value:x}")
}

fn hex_u256(value: U256) -> String {
    format!("0x{value:x}")
}

fn hex_bytes(value: Option<&Bytes>) -> String {
    value.map(ToString::to_string).unwrap_or_else(|| "0x".to_string())
}

pub fn debug_trace_call_body(transaction: &SimulationRequest) -> Value {
    let block = transaction
        .block_number
        .map(hex_u64)
        .unwrap_or_else(|| "latest".to_string());

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "debug_traceCall",
        "params": [
            {
                "from": transaction.from,
                "to": transaction.to,
                "value": hex_u256(transaction.value.map(|value| value.0).unwrap_or_default()),
                "data": hex_bytes(transaction.data.as_ref()),
                "gas": hex_u64(transaction.gas_limit.unwrap_or(DEFAULT_GAS_LIMIT))
            },
            block,
            {
                "tracer": "callTracer",
                "tracerConfig": { "withLog": true }
            }
        ]
    })
}
```

- [ ] **Step 4: Run helper tests**

Run: `cargo test quicknode_url_uses_bsc_base_and_api_key quicknode_url_skips_unsupported_chain debug_trace_call_body_matches_quicknode_curl_shape`

Expected: PASS.

---

### Task 2: Format QuickNode callTracer Result

**Files:**
- Modify: `src/quicknode.rs`

- [ ] **Step 1: Write failing formatter tests**

Add these tests inside `#[cfg(test)] mod tests` in `src/quicknode.rs`:

```rust
    #[test]
    fn quicknode_result_flattens_calls_and_collects_logs() {
        let result = serde_json::json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "type": "CALL",
            "value": "0x7",
            "input": "0x12345678aa",
            "output": "0xabcdef",
            "gasUsed": "0x63e41",
            "calls": [{
                "from": "0x0000000000000000000000000000000000000002",
                "to": "0x0000000000000000000000000000000000000003",
                "type": "STATICCALL",
                "input": "0x70a082310000000000000000000000000000000000000001",
                "gasUsed": "0x213",
                "logs": [{
                    "address": "0x0000000000000000000000000000000000000003",
                    "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                    "data": "0x000000000000000000000000000000000000000000000000000000000000002a",
                    "index": "0x1",
                    "position": "0x0"
                }]
            }],
            "logs": [{
                "address": "0x0000000000000000000000000000000000000002",
                "topics": ["0xe1fffcc4923d04b559f4d29a8bfc6cda04eb5b0d3c460751c2402c5c5cc9109c"],
                "data": "0x0000000000000000000000000000000000000000000000000000000000000007",
                "index": "0x0",
                "position": "0x0"
            }]
        });

        let response = format_quicknode_result(&request(56), 111_247_671, result).unwrap();

        assert_eq!(response.simulation_id, 1);
        assert_eq!(response.gas_used, 409_153);
        assert_eq!(response.block_number, 111_247_671);
        assert!(response.success);
        assert_eq!(response.trace.len(), 2);
        assert_eq!(response.trace[0].call_type, revm_inspectors::tracing::types::CallKind::Call);
        assert_eq!(response.trace[0].function_signature, Bytes::from(vec![0x12, 0x34, 0x56, 0x78]));
        assert_eq!(response.trace[1].call_type, revm_inspectors::tracing::types::CallKind::StaticCall);
        assert_eq!(response.logs.len(), 2);
        assert_eq!(response.return_data, Bytes::from(vec![0xab, 0xcd, 0xef]));
        assert_eq!(response.state_diff, None);
    }

    #[test]
    fn quicknode_result_marks_error_as_revert() {
        let result = serde_json::json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "type": "CALL",
            "input": "0x12345678",
            "gasUsed": "0x10",
            "error": "execution reverted",
            "output": "0x"
        });

        let response = format_quicknode_result(&request(56), 111_247_671, result).unwrap();

        assert!(!response.success);
        assert_eq!(response.exit_reason, foundry_evm::revm::interpreter::InstructionResult::Revert);
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test quicknode_result_flattens_calls_and_collects_logs quicknode_result_marks_error_as_revert`

Expected: FAIL because `format_quicknode_result` is not defined.

- [ ] **Step 3: Add formatter implementation**

Add these imports near the top of `src/quicknode.rs`:

```rust
use alloy::hex;
use alloy::primitives::{Log, B256};
use foundry_evm::revm::interpreter::InstructionResult;
use revm_inspectors::tracing::types::CallKind;

use crate::simulation::{CallTrace, SimulationResponse};
```

Add this implementation above the test module:

```rust
#[derive(Debug)]
pub struct QuickNodeError(String);

impl std::fmt::Display for QuickNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for QuickNodeError {}

fn parse_hex_u64(value: Option<&str>) -> Result<u64, QuickNodeError> {
    let value = value.unwrap_or("0x0").strip_prefix("0x").unwrap_or("0");
    u64::from_str_radix(value, 16).map_err(|err| QuickNodeError(format!("invalid hex u64: {err}")))
}

fn parse_hex_bytes(value: Option<&str>) -> Result<Bytes, QuickNodeError> {
    let value = value.unwrap_or("0x").strip_prefix("0x").unwrap_or("");
    hex::decode(value)
        .map(Bytes::from)
        .map_err(|err| QuickNodeError(format!("invalid hex bytes: {err}")))
}

fn parse_address(value: &Value, field: &str) -> Result<Address, QuickNodeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| QuickNodeError(format!("missing {field}")))?
        .parse()
        .map_err(|err| QuickNodeError(format!("invalid {field}: {err}")))
}

fn call_kind(value: Option<&str>) -> CallKind {
    match value.unwrap_or("CALL").to_ascii_uppercase().as_str() {
        "STATICCALL" => CallKind::StaticCall,
        "DELEGATECALL" => CallKind::DelegateCall,
        "CALLCODE" => CallKind::CallCode,
        "CREATE" => CallKind::Create,
        "CREATE2" => CallKind::Create2,
        _ => CallKind::Call,
    }
}

fn function_signature(input: Option<&str>) -> Result<Bytes, QuickNodeError> {
    let input = input.unwrap_or("0x").strip_prefix("0x").unwrap_or("");
    if input.len() < 8 {
        return Ok(Bytes::from(vec![0]));
    }
    hex::decode(&input[..8])
        .map(Bytes::from)
        .map_err(|err| QuickNodeError(format!("invalid function signature: {err}")))
}

fn trace_from_call(call: &Value) -> Result<CallTrace, QuickNodeError> {
    Ok(CallTrace {
        call_type: call_kind(call.get("type").and_then(Value::as_str)),
        from: parse_address(call, "from")?,
        to: parse_address(call, "to")?,
        function_signature: function_signature(call.get("input").and_then(Value::as_str))?,
        value: call
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("0x0")
            .to_string(),
    })
}

fn log_index(log: &Value) -> u64 {
    parse_hex_u64(log.get("index").and_then(Value::as_str)).unwrap_or(u64::MAX)
}

fn log_from_value(value: &Value) -> Result<Log, QuickNodeError> {
    let address = parse_address(value, "address")?;
    let data = parse_hex_bytes(value.get("data").and_then(Value::as_str))?;
    let topics = value
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(|| QuickNodeError("missing topics".to_string()))?
        .iter()
        .map(|topic| {
            topic
                .as_str()
                .ok_or_else(|| QuickNodeError("invalid topic".to_string()))?
                .parse::<B256>()
                .map_err(|err| QuickNodeError(format!("invalid topic: {err}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Log::new(address, topics, data).ok_or_else(|| QuickNodeError("too many log topics".to_string()))
}

fn collect_call(call: &Value, traces: &mut Vec<CallTrace>, logs: &mut Vec<(u64, Log)>) -> Result<(), QuickNodeError> {
    traces.push(trace_from_call(call)?);

    if let Some(call_logs) = call.get("logs").and_then(Value::as_array) {
        for log in call_logs {
            logs.push((log_index(log), log_from_value(log)?));
        }
    }

    if let Some(calls) = call.get("calls").and_then(Value::as_array) {
        for child in calls {
            collect_call(child, traces, logs)?;
        }
    }

    Ok(())
}

pub fn format_quicknode_result(
    _transaction: &SimulationRequest,
    block_number: u64,
    result: Value,
) -> Result<SimulationResponse, QuickNodeError> {
    let mut trace = Vec::new();
    let mut logs = Vec::new();
    collect_call(&result, &mut trace, &mut logs)?;
    logs.sort_by_key(|(index, _)| *index);

    let success = result.get("error").is_none() && result.get("revertReason").is_none();

    Ok(SimulationResponse {
        simulation_id: 1,
        gas_used: parse_hex_u64(result.get("gasUsed").and_then(Value::as_str))?,
        block_number,
        success,
        trace,
        logs: logs.into_iter().map(|(_, log)| log).collect(),
        exit_reason: if success {
            InstructionResult::Return
        } else {
            InstructionResult::Revert
        },
        return_data: parse_hex_bytes(result.get("output").and_then(Value::as_str))?,
        state_diff: None,
    })
}
```

- [ ] **Step 4: Run formatter tests**

Run: `cargo test quicknode_result_flattens_calls_and_collects_logs quicknode_result_marks_error_as_revert`

Expected: PASS.

---

### Task 3: Add QuickNode RPC Execution

**Files:**
- Modify: `src/quicknode.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write failing skip-gate tests**

Add this test inside `#[cfg(test)] mod tests` in `src/quicknode.rs`:

```rust
    #[test]
    fn quicknode_skip_gate_rejects_state_diff_and_state_overrides() {
        let mut state_diff_request = request(56);
        state_diff_request.include_state_diff = Some(true);
        assert_eq!(quicknode_skip_reason(&state_diff_request), Some(QuickNodeSkip::StateDiffRequested));

        let mut override_request = request(56);
        override_request.state_overrides = Some(std::collections::HashMap::new());
        assert_eq!(quicknode_skip_reason(&override_request), Some(QuickNodeSkip::StateOverrides));

        assert_eq!(quicknode_skip_reason(&request(56)), None);
    }
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test quicknode_skip_gate_rejects_state_diff_and_state_overrides`

Expected: FAIL because `quicknode_skip_reason` is not defined.

- [ ] **Step 3: Add skip gate and RPC function**

Add these imports near the top of `src/quicknode.rs`:

```rust
use alloy::network::AnyNetwork;
use alloy::providers::{Provider, ProviderBuilder};
```

Add this code above the test module:

```rust
pub fn quicknode_skip_reason(transaction: &SimulationRequest) -> Option<QuickNodeSkip> {
    if transaction.state_overrides.is_some() {
        return Some(QuickNodeSkip::StateOverrides);
    }

    if transaction.include_state_diff.unwrap_or(true) {
        return Some(QuickNodeSkip::StateDiffRequested);
    }

    None
}

async fn quicknode_block_number(provider: &impl Provider<AnyNetwork>, requested: Option<u64>) -> Result<u64, QuickNodeError> {
    if let Some(block_number) = requested {
        return Ok(block_number);
    }

    provider
        .client()
        .request::<_, String>("eth_blockNumber", serde_json::json!([]))
        .await
        .map_err(|err| QuickNodeError(format!("eth_blockNumber failed: {err}")))
        .and_then(|hex| parse_hex_u64(Some(&hex)))
}

pub async fn simulate_with_quicknode(transaction: &SimulationRequest) -> Result<Option<SimulationResponse>, QuickNodeError> {
    if quicknode_skip_reason(transaction).is_some() {
        return Ok(None);
    }

    let url = match quicknode_url(transaction.chain_id) {
        Ok(url) => url,
        Err(QuickNodeSkip::MissingConfig | QuickNodeSkip::UnsupportedChain) => return Ok(None),
        Err(err) => return Err(QuickNodeError(format!("quicknode config error: {err:?}"))),
    };

    let provider = ProviderBuilder::new()
        .network::<AnyNetwork>()
        .connect_http(url.parse().map_err(|err| QuickNodeError(format!("invalid QuickNode URL: {err}")))?);

    let block_number = quicknode_block_number(&provider, transaction.block_number).await?;
    let body = debug_trace_call_body(transaction);
    let result = provider
        .client()
        .request::<_, Value>("debug_traceCall", body["params"].clone())
        .await
        .map_err(|err| QuickNodeError(format!("debug_traceCall failed: {err}")))?;

    format_quicknode_result(transaction, block_number, result).map(Some)
}
```

Modify `src/lib.rs` to expose the module:

```rust
pub mod config;
pub mod errors;
pub mod evm;
pub mod quicknode;
pub mod simulation;
```

- [ ] **Step 4: Run skip-gate tests**

Run: `cargo test quicknode_skip_gate_rejects_state_diff_and_state_overrides`

Expected: PASS.

---

### Task 4: Hook QuickNode Into Simulate Handlers

**Files:**
- Modify: `src/simulation.rs:317-387`

- [ ] **Step 1: Update imports**

Add this import near existing crate imports in `src/simulation.rs`:

```rust
use crate::quicknode::simulate_with_quicknode;
```

- [ ] **Step 2: Try QuickNode first in `simulate()`**

Replace the start of `simulate()` with:

```rust
pub async fn simulate(transaction: SimulationRequest, config: Config) -> Result<Json, Rejection> {
    match simulate_with_quicknode(&transaction).await {
        Ok(Some(response)) => return Ok(warp::reply::json(&response)),
        Ok(None) => {}
        Err(err) => log::warn!("QuickNode simulate failed, falling back: {}", err),
    }

    let fork_url = config
        .fork_url
        .unwrap_or(chain_id_to_fork_url(transaction.chain_id)?);
```

Keep the rest of `simulate()` unchanged.

- [ ] **Step 3: Try QuickNode first in `simulate_bundle()`**

Insert this block after `let first_block_number = transactions[0].block_number;` in `simulate_bundle()`:

```rust
    let mut quicknode_responses = Vec::with_capacity(transactions.len());
    let mut quicknode_failed = false;
    for transaction in &transactions {
        if transaction.chain_id != first_chain_id {
            return Err(warp::reject::custom(MultipleChainIdsError()));
        }

        match simulate_with_quicknode(transaction).await {
            Ok(Some(response)) => quicknode_responses.push(response),
            Ok(None) => {
                quicknode_failed = true;
                break;
            }
            Err(err) => {
                log::warn!("QuickNode bundle simulate failed, falling back: {}", err);
                quicknode_failed = true;
                break;
            }
        }
    }

    if !quicknode_failed && quicknode_responses.len() == transactions.len() {
        return Ok(warp::reply::json(&quicknode_responses));
    }
```

Keep existing fallback code below it unchanged.

- [ ] **Step 4: Run compile check**

Run: `cargo test quicknode_ --lib`

Expected: PASS for QuickNode unit tests and no compile errors.

---

### Task 5: Verify End-to-End Behavior

**Files:**
- No code files expected unless tests reveal compile or behavior issues.

- [ ] **Step 1: Run focused tests**

Run: `cargo test quicknode_ --lib`

Expected: PASS.

- [ ] **Step 2: Run existing simulation tests**

Run: `cargo test simulation_request_accepts_allow_insufficient_funds simulation_request_accepts_include_state_diff`

Expected: PASS.

- [ ] **Step 3: Run full test suite if dependencies are available**

Run: `cargo test`

Expected: PASS. If an integration test requires network/env not available locally, record the exact failing command and error.

- [ ] **Step 4: Manual QuickNode smoke test**

Run the service with env:

```bash
BASE_BLOCKCHAIN_NODE_URL=<base-url> cargo run
```

Send `/api/v1/simulate` with `chainId:56`, `includeStateDiff:false`, no `stateOverrides`, and the same transaction fields from the tested curl. Expected: response contains non-empty `trace`, non-empty `logs`, `stateDiff:null`, and no local fork startup is required for the successful request.

---

## Self-Review

Spec coverage:

- QuickNode URL construction: Task 1.
- `debug_traceCall` request body: Task 1.
- Response formatting: Task 2.
- Fallback gates: Task 3.
- Handler integration for `/simulate` and `/simulate-bundle`: Task 4.
- Verification: Task 5.

Placeholder scan: no TBD/TODO/fill-later steps remain.

Type consistency: plan consistently uses `SimulationRequest`, `SimulationResponse`, `QuickNodeSkip`, `QuickNodeError`, `simulate_with_quicknode`, and `format_quicknode_result`.
