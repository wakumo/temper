# Unified Node URL Generation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use one required `BASE_BLOCKCHAIN_NODE_URL` env var to build both default-provider and QuickNode URLs from `chainId`.

**Architecture:** Keep URL generation in the existing modules with the smallest diff: `simulation.rs` builds default workflow URLs, `quicknode.rs` builds QuickNode URLs. Remove per-chain QuickNode env mapping and hardcoded fallback node URLs; missing base env becomes a request error for the workflow path and makes QuickNode fast path unavailable only until fallback also reports the required env error.

**Tech Stack:** Rust 2021, serde/serde_json, Warp rejections, cargo tests.

---

## File Structure

- Modify: `src/quicknode.rs` - build QuickNode URL from `BASE_BLOCKCHAIN_NODE_URL/{chainId}?provider=quicknode` and update tests.
- Modify: `src/simulation.rs` - build default workflow URL from `BASE_BLOCKCHAIN_NODE_URL/{chainId}` and remove hardcoded fallback URLs.
- Modify: `docs/superpowers/specs/2026-07-21-quicknode-simulate-fast-path-design.md` - update config description.

---

### Task 1: Update QuickNode URL Generation

**Files:**
- Modify: `src/quicknode.rs`

- [ ] **Step 1: Write failing QuickNode URL tests**

Replace `quicknode_url_uses_bsc_base_and_api_key`, `quicknode_url_skips_unsupported_chain`, and `quicknode_url_picks_base_env_by_chain_id` with:

```rust
    #[test]
    fn quicknode_url_uses_shared_base_with_provider_query() {
        temp_env::with_vars(
            [("BASE_BLOCKCHAIN_NODE_URL", Some("https://nodes.example.com/"))],
            || {
                let url = quicknode_url(56).unwrap();
                assert_eq!(url, "https://nodes.example.com/56?provider=quicknode");
            },
        );
    }

    #[test]
    fn quicknode_url_supports_any_chain_id() {
        temp_env::with_vars(
            [("BASE_BLOCKCHAIN_NODE_URL", Some("https://nodes.example.com"))],
            || {
                assert_eq!(quicknode_url(1).unwrap(), "https://nodes.example.com/1?provider=quicknode");
                assert_eq!(quicknode_url(8453).unwrap(), "https://nodes.example.com/8453?provider=quicknode");
            },
        );
    }

    #[test]
    fn quicknode_url_requires_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", None::<&str>, || {
            assert_eq!(quicknode_url(56), Err(QuickNodeSkip::MissingConfig));
        });
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test quicknode_url_ --lib`

Expected: FAIL because current code expects `QUICKNODE_BASE_URL_*` and `QUICKNODE_API_KEY`.

- [ ] **Step 3: Implement shared-base QuickNode URL**

Remove `quicknode_base_env()` and change `quicknode_url()` to:

```rust
pub fn quicknode_url(chain_id: u64) -> Result<String, QuickNodeSkip> {
    let base_url = env::var("BASE_BLOCKCHAIN_NODE_URL").map_err(|_| QuickNodeSkip::MissingConfig)?;
    Ok(format!(
        "{}/{}?provider=quicknode",
        base_url.trim_end_matches('/'),
        chain_id
    ))
}
```

- [ ] **Step 4: Run QuickNode URL tests**

Run: `cargo test quicknode_url_ --lib`

Expected: PASS.

---

### Task 2: Update Default Workflow URL Generation

**Files:**
- Modify: `src/simulation.rs`

- [ ] **Step 1: Write failing simulation URL tests**

Add these tests to `mod tests` in `src/simulation.rs`:

```rust
    #[test]
    fn chain_id_to_fork_url_uses_required_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", Some("https://nodes.example.com/"), || {
            let url = chain_id_to_fork_url(56).unwrap();
            assert_eq!(url, "https://nodes.example.com/56");
        });
    }

    #[test]
    fn chain_id_to_fork_url_requires_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", None::<&str>, || {
            assert!(chain_id_to_fork_url(56).is_err());
        });
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test chain_id_to_fork_url_ --lib`

Expected: one test fails because current code falls back to hardcoded URLs when env is missing.

- [ ] **Step 3: Implement required shared base URL**

Replace `chain_id_to_fork_url()` in `src/simulation.rs` with:

```rust
fn chain_id_to_fork_url(chain_id: u64) -> Result<String, Rejection> {
    let base_url = env::var("BASE_BLOCKCHAIN_NODE_URL")
        .map_err(|_| warp::reject::custom(NoURLForChainIdError))?;
    construct_url(&format!("{}/{}", base_url.trim_end_matches('/'), chain_id))
}
```

Keep `construct_url()` for now; it is harmless and avoids unrelated cleanup.

- [ ] **Step 4: Run simulation URL tests**

Run: `cargo test chain_id_to_fork_url_ --lib`

Expected: PASS.

---

### Task 3: Update Spec Note and Verify All Tests

**Files:**
- Modify: `docs/superpowers/specs/2026-07-21-quicknode-simulate-fast-path-design.md`

- [ ] **Step 1: Update config section**

Replace the QuickNode config section with:

```markdown
QuickNode and the default workflow share one required env var:

- `BASE_BLOCKCHAIN_NODE_URL`

Default workflow URL: `{BASE_BLOCKCHAIN_NODE_URL}/{chainId}`.

QuickNode URL: `{BASE_BLOCKCHAIN_NODE_URL}/{chainId}?provider=quicknode`.

If `BASE_BLOCKCHAIN_NODE_URL` is missing, simulation returns a config error instead of using hardcoded provider URLs.
```

- [ ] **Step 2: Run focused tests**

Run: `cargo test quicknode_url_ --lib && cargo test chain_id_to_fork_url_ --lib`

Expected: PASS.

- [ ] **Step 3: Run full test suite**

Run: `cargo test`

Expected: PASS.

---

## Self-Review

Spec coverage:

- Shared base URL for default provider: Task 2.
- QuickNode query-provider URL: Task 1.
- Missing env errors: Task 2.
- Removal of per-chain QuickNode env mapping: Task 1.
- No hardcoded fallback URLs: Task 2.

Placeholder scan: no placeholders remain.

Type consistency: `quicknode_url`, `chain_id_to_fork_url`, `QuickNodeSkip::MissingConfig`, and `NoURLForChainIdError` are consistently referenced.
