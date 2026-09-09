//! Recover logs from a whole-bundle callTracer replay only after verifying its
//! execution tree against trace_callMany. Never repair a corrupt VM tree by guesswork.
use alloy::primitives::{Address, Bytes, B256, U256};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

const MAX_DEPTH: usize = 128;
const MAX_FRAMES: usize = 100_000;
const MAX_LOGS: usize = 10_000;

pub fn verified_logs(original: &[Value], replay: &Value) -> Result<Vec<Vec<Value>>, String> {
    let bundles = replay
        .as_array()
        .filter(|b| b.len() == 1)
        .ok_or("INVALID_REPLAY_BUNDLE_COUNT")?;
    let calls = bundles[0]
        .as_array()
        .filter(|c| c.len() == original.len())
        .ok_or("INVALID_REPLAY_CALL_COUNT")?;
    original
        .iter()
        .zip(calls)
        .map(|(parity, debug)| {
            let traces = parity["trace"]
                .as_array()
                .ok_or("MISSING_REPLAY_REFERENCE_TRACE")?;
            let mut reference = BTreeMap::new();
            for trace in traces {
                let path = trace["traceAddress"]
                    .as_array()
                    .ok_or("INVALID_REFERENCE_PATH")?
                    .iter()
                    .map(number)
                    .collect::<Result<Vec<_>, _>>()?;
                if path.len() > MAX_DEPTH || reference.insert(path, trace).is_some() {
                    return Err("INVALID_REFERENCE_TREE".into());
                }
            }
            if reference.len() > MAX_FRAMES {
                return Err("REPLAY_FRAME_LIMIT".into());
            }
            let mut logs = Vec::new();
            let mut visited = 0;
            verify_frame(debug, &[], &reference, false, &mut visited, &mut logs)?;
            if visited != reference.len() {
                return Err("REPLAY_TREE_MISMATCH".into());
            }
            if hex_bytes(&debug["output"], true)? != hex_bytes(&parity["output"], false)? {
                return Err("REPLAY_OUTPUT_MISMATCH".into());
            }
            // Geth positions count preceding child calls within a frame, not global
            // log indices. verify_frame already interleaves them in execution order.
            // Erigon/BSC may additionally provide a global index; verify uniqueness
            // and use it when present on every committed log.
            if logs.iter().all(|(index, _)| index.is_some()) {
                let mut indices = HashSet::new();
                if !logs.iter().all(|(index, _)| indices.insert(*index)) {
                    return Err("DUPLICATE_REPLAY_LOG_INDEX".into());
                }
                logs.sort_by_key(|(index, _)| *index);
            }
            Ok(logs.into_iter().map(|(_, log)| log).collect())
        })
        .collect()
}

fn number(value: &Value) -> Result<usize, String> {
    if let Some(n) = value.as_u64() {
        return usize::try_from(n).map_err(|_| "INVALID_REPLAY_NUMBER".into());
    }
    value
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| "INVALID_REPLAY_NUMBER".into())
}
fn addr(value: &Value) -> Result<Address, String> {
    value
        .as_str()
        .ok_or("MISSING_REPLAY_ADDRESS")?
        .parse()
        .map_err(|_| "INVALID_REPLAY_ADDRESS".into())
}
fn hex_bytes(value: &Value, optional: bool) -> Result<Bytes, String> {
    if optional && value.is_null() {
        return Ok(Bytes::new());
    }
    value
        .as_str()
        .ok_or("MISSING_REPLAY_BYTES")?
        .parse()
        .map_err(|_| "INVALID_REPLAY_BYTES".into())
}
fn amount(value: &Value) -> Result<U256, String> {
    if value.is_null() {
        return Ok(U256::ZERO);
    }
    U256::from_str(value.as_str().ok_or("INVALID_REPLAY_VALUE")?)
        .map_err(|_| "INVALID_REPLAY_VALUE".into())
}
fn optional_array<'a>(object: &'a Value, key: &str) -> Result<&'a [Value], String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        _ => Err(format!("INVALID_REPLAY_{key}")),
    }
}
fn verify_frame(
    frame: &Value,
    path: &[usize],
    reference: &BTreeMap<Vec<usize>, &Value>,
    ancestor_failed: bool,
    visited: &mut usize,
    logs: &mut Vec<(Option<usize>, Value)>,
) -> Result<(), String> {
    if path.len() > MAX_DEPTH || *visited >= MAX_FRAMES {
        return Err("REPLAY_FRAME_LIMIT".into());
    }
    let expected = reference.get(path).ok_or("REPLAY_TREE_MISMATCH")?;
    *visited += 1;
    let kind = frame["type"]
        .as_str()
        .ok_or("MISSING_REPLAY_CALL_TYPE")?
        .to_ascii_lowercase();
    let action = &expected["action"];
    // A recovery must be conservative. Trace types with provider-specific
    // representations (e.g. SELFDESTRUCT) remain errors unless verified here.
    if expected["type"] != "call"
        || action["callType"].as_str() != Some(kind.as_str())
        || addr(&frame["from"])? != addr(&action["from"])?
        || addr(&frame["to"])? != addr(&action["to"])?
        || hex_bytes(&frame["input"], false)? != hex_bytes(&action["input"], false)?
        || amount(&frame["value"])? != amount(&action["value"])?
    {
        return Err(format!("REPLAY_CALL_MISMATCH at {path:?}"));
    }
    let failed = frame.get("error").is_some_and(|e| !e.is_null());
    if failed != expected.get("error").is_some_and(|e| !e.is_null()) {
        return Err(format!("REPLAY_STATUS_MISMATCH at {path:?}"));
    }
    if !failed
        && hex_bytes(&frame["output"], true)? != hex_bytes(&expected["result"]["output"], false)?
    {
        return Err(format!("REPLAY_OUTPUT_MISMATCH at {path:?}"));
    }
    let reverted = ancestor_failed || failed;
    let children = optional_array(frame, "calls")?;
    let frame_logs = optional_array(frame, "logs")?;
    if frame_logs.len() > MAX_LOGS {
        return Err("REPLAY_LOG_LIMIT".into());
    }
    if children.len() > MAX_FRAMES {
        return Err("REPLAY_FRAME_LIMIT".into());
    }
    let mut positions: Vec<Vec<&Value>> =
        vec![Vec::new(); children.len().checked_add(1).ok_or("REPLAY_FRAME_LIMIT")?];
    if !reverted {
        for log in frame_logs {
            let position = match log.get("position") {
                Some(p) => number(p)?,
                None if children.is_empty() => 0,
                None => return Err("MISSING_REPLAY_LOG_POSITION".into()),
            };
            positions
                .get_mut(position)
                .ok_or("INVALID_REPLAY_LOG_POSITION")?
                .push(log);
        }
    }
    for (i, at_position) in positions.into_iter().enumerate() {
        for log in at_position {
            if logs.len() >= MAX_LOGS {
                return Err("REPLAY_LOG_LIMIT".into());
            }
            let topics = log["topics"]
                .as_array()
                .filter(|t| t.len() <= 4)
                .ok_or("INVALID_REPLAY_LOG_TOPICS")?
                .iter()
                .map(|t| {
                    t.as_str()
                        .ok_or("INVALID_REPLAY_LOG_TOPIC")?
                        .parse::<B256>()
                        .map_err(|_| "INVALID_REPLAY_LOG_TOPIC")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let context = if matches!(kind.as_str(), "delegatecall" | "callcode") {
                addr(&frame["from"])?
            } else {
                addr(&frame["to"])?
            };
            if addr(&log["address"])? != context {
                return Err(format!("REPLAY_LOG_ADDRESS_MISMATCH at {path:?}"));
            }
            let index = log.get("index").map(number).transpose()?;
            logs.push((index,json!({"address":addr(&log["address"])?,"topics":topics,"data":hex_bytes(&log["data"],false)?})));
        }
        if let Some(child) = children.get(i) {
            let mut child_path = path.to_vec();
            child_path.push(i);
            verify_frame(child, &child_path, reference, reverted, visited, logs)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> (Vec<Value>, Value) {
        let original: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/bundle_v2_bsc97_invalid_vm.json"
        ))
        .unwrap();
        let replay: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/bundle_v2_bsc97_debug_many.json"
        ))
        .unwrap();
        (
            original["result"].as_array().unwrap().clone(),
            replay["result"].clone(),
        )
    }
    #[test]
    fn rejects_reordered_calls_and_different_nested_execution() {
        let (original, replay) = fixtures();
        let mut reordered = replay.clone();
        reordered[0].as_array_mut().unwrap().swap(0, 1);
        assert!(verified_logs(&original, &reordered)
            .unwrap_err()
            .contains("CALL_MISMATCH"));
        let mut changed = replay.clone();
        changed[0][2]["calls"][0]["calls"][0]["output"] = json!("0xdead");
        assert!(verified_logs(&original, &changed)
            .unwrap_err()
            .contains("OUTPUT_MISMATCH"));
        let mut missing = replay.clone();
        missing[0][2]["calls"].as_array_mut().unwrap().pop();
        assert!(verified_logs(&original, &missing)
            .unwrap_err()
            .contains("TREE_MISMATCH"));
    }
    #[test]
    fn interleaves_parent_and_child_logs_without_global_indices() {
        let (original, mut replay) = fixtures();
        let expected = verified_logs(&original, &replay).unwrap();
        fn remove_indices(frame: &mut Value) {
            if let Some(logs) = frame.get_mut("logs").and_then(Value::as_array_mut) {
                for log in logs {
                    log.as_object_mut().unwrap().remove("index");
                }
            }
            if let Some(calls) = frame.get_mut("calls").and_then(Value::as_array_mut) {
                for child in calls {
                    remove_indices(child);
                }
            }
        }
        for call in replay[0].as_array_mut().unwrap() {
            remove_indices(call);
        }
        assert_eq!(verified_logs(&original, &replay).unwrap(), expected);
    }
    #[test]
    fn drops_logs_from_caught_revert_and_all_its_descendants() {
        let (mut original, mut replay) = fixtures();
        original[2]["trace"][1]["error"] = json!("Reverted");
        original[2]["trace"][1]
            .as_object_mut()
            .unwrap()
            .remove("result");
        replay[0][2]["calls"][0]["error"] = json!("execution reverted");
        let logs = verified_logs(&original, &replay).unwrap();
        assert_eq!(logs[2].len(), 2);
        assert_eq!(
            logs[2][0]["topics"][0],
            "0x7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65"
        );
    }
    #[test]
    fn rejects_status_mismatch_invalid_positions_and_duplicate_indices() {
        let (original, replay) = fixtures();
        let mut failed = replay.clone();
        failed[0][2]["error"] = json!("execution reverted");
        assert!(verified_logs(&original, &failed)
            .unwrap_err()
            .contains("STATUS_MISMATCH"));
        let mut invalid = replay.clone();
        invalid[0][2]["calls"][0]["logs"][0]["position"] = json!(999);
        assert!(verified_logs(&original, &invalid)
            .unwrap_err()
            .contains("LOG_POSITION"));
        let mut duplicate = replay.clone();
        duplicate[0][2]["calls"][0]["logs"][0]["index"] = json!("0x0");
        assert!(verified_logs(&original, &duplicate)
            .unwrap_err()
            .contains("DUPLICATE_REPLAY_LOG_INDEX"));
    }
    #[test]
    fn rejects_a_log_from_the_wrong_execution_address() {
        let (original, mut replay) = fixtures();
        replay[0][2]["calls"][0]["logs"][0]["address"] =
            json!("0x0000000000000000000000000000000000000001");
        assert!(verified_logs(&original, &replay)
            .unwrap_err()
            .contains("LOG_ADDRESS"));
    }
}
