# Restore Simulate Bundle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-enable `POST /simulate-bundle` with QuickNode fast path and legacy single-fork fallback.

**Architecture:** Restore the Warp route in `src/lib.rs`. Keep existing `simulation::simulate_bundle` workflow, adding only a guard for empty bundles and a registered-route test.

**Tech Stack:** Rust, Warp, Tokio, Alloy/Revm existing simulation stack, `cargo test`.

---

## File Structure

- Modify `src/lib.rs`: register `simulate_bundle(config.clone())` in `simulate_routes` and restore route builder.
- Modify `src/simulation.rs`: add empty bundle rejection before indexing `transactions[0]`.
- Modify `src/errors.rs`: add a small request rejection for empty bundle if no existing error fits.
- Modify `tests/api.rs`: replace disabled-route assertion with route-registered empty bundle assertion.

### Task 1: Route Registration Test

**Files:**
- Modify: `tests/api.rs`

- [ ] **Step 1: Write failing test**

Replace `post_simulate_bundle_is_not_registered` with:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn post_simulate_bundle_empty_body_is_bad_request() {
    let filter = filter();

    let res = warp::test::request()
        .method("POST")
        .path("/simulate-bundle")
        .json(&serde_json::json!([]))
        .reply(&filter)
        .await;

    assert_eq!(res.status(), 400);

    let body: ErrorMessage = serde_json::from_slice(res.body()).unwrap();
    assert_eq!(body.message, "EMPTY_BUNDLE".to_string());
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test post_simulate_bundle_empty_body_is_bad_request --test api`

Expected: FAIL because current route returns `404`.

### Task 2: Restore Route

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Restore route builder**

Change `simulate_routes` to include bundle route:

```rust
pub fn simulate_routes(
    config: Config,
    state: Arc<SharedSimulationState>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    simulate(config.clone())
        .or(simulate_bundle(config.clone()))
        .or(simulate_stateful_new(config, state.clone()))
        .or(simulate_stateful_end(state.clone()))
        .or(simulate_stateful(state))
}
```

Restore route function:

```rust
/// POST /simulate-bundle
pub fn simulate_bundle(
    config: Config,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    warp::path!("simulate-bundle")
        .and(warp::post())
        .and(json_body())
        .and(with_config(config))
        .and_then(simulation::simulate_bundle)
}
```

- [ ] **Step 2: Run focused test**

Run: `cargo test post_simulate_bundle_empty_body_is_bad_request --test api`

Expected: still FAIL because empty bundle panics or rejects with wrong error.

### Task 3: Empty Bundle Error

**Files:**
- Modify: `src/errors.rs`
- Modify: `src/simulation.rs`

- [ ] **Step 1: Add error type**

In `src/errors.rs`, add:

```rust
#[derive(Debug)]
pub struct EmptyBundleError();
impl warp::reject::Reject for EmptyBundleError {}
```

Add it to the recover mapping with status 400 and message `EMPTY_BUNDLE`.

- [ ] **Step 2: Guard before indexing**

At the top of `simulation::simulate_bundle`, before reading `transactions[0]`, add:

```rust
if transactions.is_empty() {
    return Err(warp::reject::custom(EmptyBundleError()));
}
```

Ensure `EmptyBundleError` is imported with other errors.

- [ ] **Step 3: Run focused test**

Run: `cargo test post_simulate_bundle_empty_body_is_bad_request --test api`

Expected: PASS.

### Task 4: Verify Existing Bundle Semantics

**Files:**
- Modify: none unless tests reveal compile issues

- [ ] **Step 1: Run route and existing unit tests**

Run: `cargo test simulate_bundle --test api`

Expected: empty bundle test passes; legacy live bundle tests remain ignored.

- [ ] **Step 2: Run full tests**

Run: `cargo test`

Expected: all non-ignored tests pass.

- [ ] **Step 3: Review diff**

Run: `git diff -- src/lib.rs src/simulation.rs src/errors.rs tests/api.rs docs/superpowers/specs/2026-07-22-restore-simulate-bundle-design.md docs/superpowers/plans/2026-07-22-restore-simulate-bundle.md`

Expected: diff only restores route, adds empty bundle guard/error, updates one API test, and adds docs.

---

## Self-Review

- Spec coverage: route restored, QuickNode-first existing flow kept, local fallback still uses one `Evm`, empty bundle no longer panics, tests cover registered route.
- Placeholder scan: no TBD/TODO placeholders.
- Type consistency: uses existing `SimulationRequest`, `simulate_bundle`, `ErrorMessage`, Warp rejection pattern.
