# Request ID Echo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Echo optional transaction `request_id` values in simulation responses.

**Architecture:** Add `request_id` to the existing request and response structs, then copy it in every response builder. Keep stateful session creation unchanged.

**Tech Stack:** Rust, Serde, Warp, Tokio, existing EVM and QuickNode simulation code.

---

## File Structure

- Modify `src/simulation.rs`: add request/response fields, tests, and local response propagation.
- Modify `src/quicknode.rs`: propagate request ID in QuickNode response builder and test helper.
- Modify `src/evm.rs`: update direct test response construction.
- Modify `tests/api.rs`: update expected `SimulationResponse` values and add no live-RPC API coverage if needed.
- Modify `README.md`: document `request_id` on transaction simulation responses.

### Task 1: Request/Response Serialization Tests

**Files:**
- Modify: `src/simulation.rs`

- [ ] **Step 1: Write failing tests**

Add tests in `mod tests`:

```rust
#[test]
fn simulation_request_accepts_request_id() {
    let request: SimulationRequest = serde_json::from_value(serde_json::json!({
        "request_id": "request-a",
        "chainId": 1,
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000002",
        "gasLimit": 21_000
    }))
    .unwrap();

    assert_eq!(request.request_id.as_deref(), Some("request-a"));
}

#[test]
fn simulation_response_serializes_request_id() {
    let response = SimulationResponse {
        request_id: Some("request-a".to_string()),
        simulation_id: 1,
        gas_used: 0,
        block_number: 1,
        success: true,
        trace: Vec::new(),
        logs: Vec::new(),
        exit_reason: InstructionResult::Return,
        return_data: Bytes::new(),
        state_diff: None,
    };

    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["request_id"], "request-a");
}

#[test]
fn simulation_response_serializes_missing_request_id_as_null() {
    let response = SimulationResponse {
        request_id: None,
        simulation_id: 1,
        gas_used: 0,
        block_number: 1,
        success: true,
        trace: Vec::new(),
        logs: Vec::new(),
        exit_reason: InstructionResult::Return,
        return_data: Bytes::new(),
        state_diff: None,
    };

    let json = serde_json::to_value(response).unwrap();
    assert!(json["request_id"].is_null());
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test request_id --lib`

Expected: FAIL because `request_id` fields do not exist.

### Task 2: Add Struct Fields and Local Propagation

**Files:**
- Modify: `src/simulation.rs`
- Modify: `src/evm.rs`

- [ ] **Step 1: Add fields**

In `SimulationRequest`:

```rust
#[serde(rename = "request_id")]
pub request_id: Option<String>,
```

In `SimulationResponse`:

```rust
#[serde(rename = "request_id")]
pub request_id: Option<String>,
```

- [ ] **Step 2: Propagate in local response builder**

In `run_warm_stateless`, set:

```rust
request_id: transaction.request_id.clone(),
```

inside `SimulationResponse`.

- [ ] **Step 3: Update direct response construction**

In `src/evm.rs`, add `request_id: None,` to any test `SimulationResponse` literals.

- [ ] **Step 4: Run focused tests**

Run: `cargo test request_id --lib`

Expected: PASS after all struct literals compile.

### Task 3: QuickNode Propagation

**Files:**
- Modify: `src/quicknode.rs`

- [ ] **Step 1: Write failing QuickNode test**

Add to QuickNode tests:

```rust
#[test]
fn quicknode_result_echoes_request_id() {
    let mut req = request(56);
    req.request_id = Some("request-a".to_string());
    let result = serde_json::json!({
        "type": "CALL",
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000002",
        "gas": "0x5208",
        "gasUsed": "0x5208",
        "input": "0x12345678",
        "output": "0x"
    });

    let response = format_quicknode_result(&req, 111_247_671, result).unwrap();

    assert_eq!(response.request_id.as_deref(), Some("request-a"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test quicknode_result_echoes_request_id --lib`

Expected: FAIL until QuickNode response builder copies the field.

- [ ] **Step 3: Propagate in QuickNode response builder**

In `format_quicknode_result`, set:

```rust
request_id: _transaction.request_id.clone(),
```

inside `SimulationResponse`.

Also update the `request()` helper to include `request_id: None,`.

- [ ] **Step 4: Run focused test**

Run: `cargo test quicknode_result_echoes_request_id --lib`

Expected: PASS.

### Task 4: API Expected Responses and Docs

**Files:**
- Modify: `tests/api.rs`
- Modify: `README.md`

- [ ] **Step 1: Update API expected literals**

Add `request_id: None,` to `SimulationResponse` expected values in `tests/api.rs`.

- [ ] **Step 2: Add API-level no-RPC coverage**

If existing invalid request tests deserialize `SimulationRequest`, add a test that posts an invalid transaction with `request_id` and confirms request parsing reaches validation. Do not add live RPC tests.

- [ ] **Step 3: Update README**

Document that transaction requests may include `request_id` and every simulation response includes `request_id`, or `null` when absent. State that `/simulate-stateful` creation does not use it.

### Task 5: Verification

**Files:**
- Modify: none unless verification finds issues

- [ ] **Step 1: Format**

Run: `cargo fmt --all --check`

Expected: PASS.

- [ ] **Step 2: Full tests**

Run: `cargo test --locked`

Expected: all non-ignored tests pass.

- [ ] **Step 3: Review diff**

Run: `git diff -- src/simulation.rs src/quicknode.rs src/evm.rs tests/api.rs README.md docs/superpowers/specs/2026-07-22-request-id-echo-design.md docs/superpowers/plans/2026-07-22-request-id-echo.md`

Expected: only request ID propagation, docs, and tests.

---

## Self-Review

- Spec coverage: single, bundle, stateful item simulations covered; stateful creation excluded.
- Placeholder scan: no TBD/TODO placeholders.
- Type consistency: Rust field is `request_id`; JSON key is explicitly `request_id` despite global camelCase.
