//! Ordered, shared-state QuickNode simulations. Independent bundles keep their existing route.
use crate::simulation::{CallTrace, SimulationRequest, SimulationResponse};
use alloy::primitives::{Address, Bytes, Log, B256, U256};
use alloy::transports::http::reqwest;
use foundry_evm::revm::interpreter::InstructionResult;
use revm_inspectors::tracing::types::CallKind;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use warp::{reply::Json, Filter, Rejection};

#[derive(Serialize)]
struct BundleV2Response {
    #[serde(flatten)]
    simulation: SimulationResponse,
    error: Option<String>,
    #[serde(rename = "gasAccounting")]
    gas_accounting: &'static str,
}

#[derive(Debug)]
pub struct BundleV2Error {
    pub status: u16,
    pub message: String,
}
impl warp::reject::Reject for BundleV2Error {}

#[derive(Debug)]
enum NormalizeError {
    Invalid(String),
    VmLogDecode(String),
}

impl NormalizeError {
    fn into_message(self) -> String {
        match self {
            Self::Invalid(message) | Self::VmLogDecode(message) => message,
        }
    }
}

impl From<&str> for NormalizeError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}

impl From<String> for NormalizeError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

fn rejection(status: u16, message: impl Into<String>) -> Rejection {
    warp::reject::custom(BundleV2Error {
        status,
        message: message.into(),
    })
}

const MAX_CONCURRENT_DECODES: usize = 4;
static DECODE_SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();

async fn spawn_decode<F, T>(work: F) -> Result<T, Rejection>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let semaphore = DECODE_SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_DECODES)))
        .clone();
    spawn_decode_with(semaphore, work).await
}

async fn spawn_decode_with<F, T>(semaphore: Arc<Semaphore>, work: F) -> Result<T, Rejection>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| rejection(500, "BUNDLE_DECODE_GATE_CLOSED"))?;
    tokio::task::spawn_blocking(move || {
        // Keep the permit in the blocking task so cancellation of the caller
        // cannot admit more decode work before this task actually finishes.
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| rejection(500, "BUNDLE_DECODE_TASK_FAILED"))
}

pub fn route() -> impl Filter<Extract = (impl warp::Reply,), Error = Rejection> + Clone {
    warp::path!("simulate_bundle")
        .and(warp::post())
        .and(warp::body::content_length_limit(2 * 1024 * 1024))
        .and(warp::body::json::<Vec<SimulationRequest>>())
        .and_then(simulate)
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value, Rejection> {
    let mut response = client
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|_| rejection(502, "QUICKNODE_TRANSPORT_ERROR"))?;
    if !response.status().is_success() {
        return Err(rejection(502, "QUICKNODE_HTTP_ERROR"));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(rejection(502, "QUICKNODE_RESPONSE_TOO_LARGE"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| rejection(502, "QUICKNODE_RESPONSE_ERROR"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(rejection(502, "QUICKNODE_RESPONSE_TOO_LARGE"));
        }
        body.extend_from_slice(&chunk);
    }
    let mut body: Value =
        serde_json::from_slice(&body).map_err(|_| rejection(502, "INVALID_QUICKNODE_JSON"))?;
    if body.get("error").is_some() {
        return Err(rejection(502, format!("QUICKNODE_RPC_ERROR_{method}")));
    }
    body.get_mut("result")
        .map(Value::take)
        .filter(|v| !v.is_null())
        .ok_or_else(|| rejection(502, "MISSING_QUICKNODE_RESULT"))
}

pub async fn simulate(calls: Vec<SimulationRequest>) -> Result<Json, Rejection> {
    let requested_block = validate(&calls).map_err(|e| rejection(400, e))?;
    let url = crate::quicknode::quicknode_url(calls[0].chain_id)
        .map_err(|_| rejection(503, "QUICKNODE_NOT_CONFIGURED"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|_| rejection(500, "RPC_CLIENT_ERROR"))?;
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        let chain = rpc(&client, &url, "eth_chainId", json!([])).await?;
        if hex_number(&chain).map_err(|e| rejection(502, e))? != calls[0].chain_id {
            return Err(rejection(502, "INCORRECT_PROVIDER_CHAIN_ID"));
        }
        let block = match requested_block {
            Some(n) => n,
            None => hex_number(&rpc(&client, &url, "eth_blockNumber", json!([])).await?)
                .map_err(|e| rejection(502, e))?,
        };
        let params = rpc_params(&calls, block).map_err(|e| rejection(400, e))?;
        let result = rpc(&client, &url, "trace_callMany", params).await?;
        let results = result
            .as_array()
            .filter(|a| a.len() == calls.len())
            .ok_or_else(|| rejection(502, "INVALID_BUNDLE_RESULT_COUNT"))?;
        // CPU work does not block the async HTTP runtime. Trace size is bounded above.
        let results = results.clone();
        let (calls, results, decoded) = spawn_decode(move || {
            let decoded = normalize_batch(&calls, block, &results, None);
            (calls, results, decoded)
        })
        .await?;
        match decoded {
            Ok(response) => Ok(response),
            Err(NormalizeError::Invalid(error)) => {
                Err(rejection(502, format!("INVALID_BUNDLE_RESULT: {error}")))
            }
            Err(NormalizeError::VmLogDecode(error)) => {
                log::warn!(target: "ts::api", "trace_callMany decode failed on chain {}: {}; replaying pinned bundle with debug_traceCallMany", calls[0].chain_id, error);
                let params = rpc_params(&calls, block).map_err(|e| rejection(400, e))?;
                let transactions: Vec<Value> = params[0].as_array().expect("validated RPC params")
                    .iter().map(|entry| entry[0].clone()).collect();
                let debug = rpc(&client, &url, "debug_traceCallMany", json!([
                    [{"transactions": transactions}],
                    {"blockNumber": format!("0x{block:x}"), "transactionIndex": -1},
                    {"tracer": "callTracer", "tracerConfig": {"withLog": true}}
                ])).await?;
                spawn_decode(move || {
                    let logs = super::bundle_call_tracer::verified_logs(&results, &debug)?;
                    normalize_batch(&calls, block, &results, Some(&logs))
                        .map_err(NormalizeError::into_message)
                }).await?
                    .map_err(|e| rejection(502, format!("INVALID_BUNDLE_REPLAY: {e}")))
            }
        }
    })
    .await
    .map_err(|_| rejection(504, "BUNDLE_SIMULATION_TIMEOUT"))??;
    Ok(warp::reply::json(&result))
}

fn normalize_batch(
    calls: &[SimulationRequest],
    block: u64,
    results: &[Value],
    recovered_logs: Option<&[Vec<Value>]>,
) -> Result<Vec<BundleV2Response>, NormalizeError> {
    calls
        .iter()
        .zip(results)
        .enumerate()
        .map(|(i, (call, result))| {
            let error = result["trace"]
                .as_array()
                .and_then(|traces| traces.iter().find(|t| t["traceAddress"] == json!([])))
                .map(trace_error)
                .transpose()?
                .flatten()
                .map(str::to_owned);
            format_result_with_logs(call, block, i, result, recovered_logs.map(|logs| &logs[i]))
                .map(|simulation| BundleV2Response {
                    simulation,
                    error,
                    gas_accounting: "execution_plus_intrinsic",
                })
        })
        .collect()
}

fn hex_number(value: &Value) -> Result<u64, String> {
    let raw = value
        .as_str()
        .and_then(|v| v.strip_prefix("0x"))
        .filter(|v| !v.is_empty())
        .ok_or("INVALID_HEX_QUANTITY")?;
    u64::from_str_radix(raw, 16).map_err(|_| "INVALID_HEX_QUANTITY".into())
}
fn path(value: &Value) -> Result<Vec<u64>, String> {
    value["traceAddress"]
        .as_array()
        .ok_or("MISSING_TRACE_ADDRESS")?
        .iter()
        .map(|v| v.as_u64().ok_or_else(|| "INVALID_TRACE_ADDRESS".into()))
        .collect()
}
fn address(value: &Value) -> Result<Address, String> {
    value
        .as_str()
        .ok_or("MISSING_ADDRESS")?
        .parse()
        .map_err(|_| "INVALID_ADDRESS".into())
}
fn bytes(value: &Value) -> Result<Bytes, String> {
    value
        .as_str()
        .ok_or("MISSING_BYTES")?
        .parse()
        .map_err(|_| "INVALID_BYTES".into())
}

const DEFAULT_GAS: u64 = 30_000_000;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

fn validate(calls: &[SimulationRequest]) -> Result<Option<u64>, String> {
    if calls.is_empty() || calls.len() > 20 {
        return Err("BUNDLE_REQUIRES_1_TO_20_CALLS".into());
    }
    let mut block = None;
    let mut gas = 0u64;
    for call in calls {
        if call.chain_id == 0 || call.chain_id != calls[0].chain_id {
            return Err("MULTIPLE_OR_INVALID_CHAIN_IDS".into());
        }
        if let Some(n) = call.block_number {
            if block.is_some_and(|b| b != n) {
                return Err("MULTIPLE_BLOCK_NUMBERS".into());
            }
            block = Some(n);
        }
        let limit = call.gas_limit.unwrap_or(DEFAULT_GAS);
        if limit == 0 || limit > DEFAULT_GAS {
            return Err("INVALID_GAS_LIMIT".into());
        }
        gas += limit;
        if gas > 100_000_000 {
            return Err("BUNDLE_GAS_LIMIT_EXCEEDED".into());
        }
        if call.state_overrides.is_some() || call.allow_insufficient_funds == Some(true) {
            return Err("BUNDLE_OVERRIDES_NOT_SUPPORTED".into());
        }
        if let Some(price) = &call.gas_price {
            gas_price(price)?;
        }
    }
    Ok(block)
}

fn gas_price(price: &str) -> Result<String, String> {
    // Avoid floating point rounding of wei. Input follows existing API units (gwei).
    let (whole, fraction) = price.split_once('.').unwrap_or((price, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("INVALID_GAS_PRICE".into());
    }
    let whole = U256::from_str(whole).map_err(|_| "INVALID_GAS_PRICE")?;
    let fraction = U256::from_str(&format!("{fraction:0<9}")).map_err(|_| "INVALID_GAS_PRICE")?;
    let value = whole
        .checked_mul(U256::from(1_000_000_000u64))
        .and_then(|v| v.checked_add(fraction))
        .ok_or("INVALID_GAS_PRICE")?;
    Ok(format!("0x{value:x}"))
}
fn rpc_params(calls: &[SimulationRequest], block: u64) -> Result<Value, String> {
    let calls = calls.iter().map(|call| {
        let mut tx = json!({"from":call.from,"to":call.to,"data":call.data.as_ref().map(ToString::to_string).unwrap_or_else(|| "0x".into()),"value":format!("0x{:x}",call.value.map(|v| v.0).unwrap_or_default()),"gas":format!("0x{:x}",call.gas_limit.unwrap_or(DEFAULT_GAS))});
        if let Some(price) = &call.gas_price { tx["gasPrice"] = json!(gas_price(price)?); }
        if let Some(list) = &call.access_list { tx["accessList"] = json!(list); }
        let mut kinds = vec!["trace", "vmTrace"];
        if call.include_state_diff == Some(true) { kinds.push("stateDiff"); }
        Ok(json!([tx,kinds]))
    }).collect::<Result<Vec<Value>,String>>()?;
    Ok(json!([calls, format!("0x{block:x}")]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call() -> SimulationRequest {
        serde_json::from_value(json!({"chainId":1,"from":"0x0000000000000000000000000000000000000001","to":"0x0000000000000000000000000000000000000002","data":"0x12345678","value":"0","gasLimit":100000,"request_id":"a"})).unwrap()
    }
    #[test]
    fn rejects_empty_mixed_chain_and_conflicting_blocks() {
        assert!(validate(&[]).is_err());
        let a = call();
        let mut b = call();
        b.chain_id = 56;
        assert!(validate(&[a.clone(), b.clone()]).is_err());
        b.chain_id = 1;
        b.block_number = Some(100);
        assert_eq!(validate(&[a.clone(), b.clone()]).unwrap(), Some(100));
        let mut a = a;
        a.block_number = Some(101);
        assert!(validate(&[a, b]).is_err());
    }
    #[test]
    fn enforces_limits_and_rejects_unimplemented_overrides() {
        assert!(validate(&vec![call(); 21]).is_err());
        let mut a = call();
        a.gas_limit = Some(30_000_001);
        assert!(validate(&[a.clone()]).is_err());
        a.gas_limit = Some(30_000_000);
        assert!(validate(&vec![a.clone(); 4]).is_err());
        a.gas_limit = Some(100000);
        a.allow_insufficient_funds = Some(true);
        assert!(validate(&[a.clone()]).is_err());
        a.allow_insufficient_funds = None;
        a.state_overrides = Some(Default::default());
        assert!(validate(&[a]).is_err());
    }
    #[test]
    fn uses_one_pinned_block_and_preserves_duplicate_senders() {
        let a = call();
        let mut b = call();
        b.data = Some("0xaabbccdd".parse().unwrap());
        b.include_state_diff = Some(true);
        b.gas_price = Some("1.25".into());
        let p = rpc_params(&[a, b], 123).unwrap();
        assert_eq!(p[1], "0x7b");
        assert_eq!(p[0].as_array().unwrap().len(), 2);
        assert_eq!(p[0][0][0]["data"], "0x12345678");
        assert_eq!(p[0][1][0]["data"], "0xaabbccdd");
        assert_eq!(p[0][1][0]["gasPrice"], "0x4a817c80");
        assert_eq!(p[0][0][1], json!(["trace", "vmTrace"]));
        assert_eq!(p[0][1][1], json!(["trace", "vmTrace", "stateDiff"]));
    }
}

#[cfg(test)]
mod normalization_tests {
    use super::*;
    fn fixtures() -> (Vec<SimulationRequest>, Vec<Value>) {
        let calls = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_requests.json"
        ))
        .unwrap();
        let response: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_usdt_response.json"
        ))
        .unwrap();
        (calls, response["result"].as_array().unwrap().clone())
    }

    #[test]
    fn rejects_reordered_results_with_same_sender_and_recipient() {
        let (calls, mut results) = fixtures();
        results.swap(1, 2);
        assert_eq!(
            normalize_batch(&calls, 25936975, &results, None)
                .err()
                .map(NormalizeError::into_message),
            Some("ROOT_CALL_MISMATCH".into())
        );
    }

    #[test]
    fn rejects_root_value_mismatch_even_when_input_matches() {
        let (mut calls, results) = fixtures();
        calls[0].value = Some(crate::simulation::PermissiveUint(U256::from(1)));
        assert_eq!(
            format_result(&calls[0], 25936975, 0, results[0].clone()).unwrap_err(),
            "ROOT_CALL_MISMATCH"
        );
    }

    #[test]
    fn rejects_non_call_root_types() {
        let (calls, results) = fixtures();
        for (field, value) in [
            ("callType", "delegatecall"),
            ("callType", "staticcall"),
            ("callType", "callcode"),
            ("type", "create"),
        ] {
            let mut result = results[0].clone();
            let root = result["trace"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|t| t["traceAddress"] == json!([]))
                .unwrap();
            if field == "type" {
                root[field] = json!(value);
            } else {
                root["action"][field] = json!(value);
            }
            assert_eq!(
                format_result(&calls[0], 25936975, 0, result).unwrap_err(),
                "ROOT_CALL_MISMATCH",
                "{field}={value}"
            );
        }
    }

    #[test]
    fn identical_calls_keep_each_results_position_and_optional_request_id() {
        let (calls, results) = fixtures();
        let mut first = calls[0].clone();
        first.request_id = Some("first".into());
        let mut second = first.clone();
        second.request_id = None;
        let mut failed = results[0].clone();
        let root = failed["trace"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|t| t["traceAddress"] == json!([]))
            .unwrap();
        root["error"] = json!("Out of gas");
        root.as_object_mut().unwrap().remove("result");
        failed["output"] = json!("0x");
        let responses = normalize_batch(
            &[first, second],
            25936975,
            &[results[0].clone(), failed],
            None,
        )
        .unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].simulation.request_id.as_deref(), Some("first"));
        assert_eq!(responses[1].simulation.request_id, None);
        assert_eq!(responses[0].simulation.simulation_id, 1);
        assert_eq!(responses[1].simulation.simulation_id, 2);
        assert!(responses[0].simulation.success);
        assert!(!responses[1].simulation.success);
    }

    #[test]
    fn accepts_default_empty_data_and_zero_value_with_equivalent_hex() {
        let root = trace(
            json!([]),
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000002",
            "0x00",
            "call",
        );
        let result = json!({"output":"0x","trace":[root],"vmTrace":{"code":"0x00","ops":[{"op":"STOP","pc":0,"sub":null,"ex":{"push":[],"mem":null}}]}});
        assert!(format_result(&call(), 123, 0, result).unwrap().success);
    }

    #[test]
    fn treats_null_error_as_success_for_result_and_movement_traces() {
        let (calls, mut results) = fixtures();
        let root = results[0]["trace"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|trace| trace["traceAddress"] == json!([]))
            .unwrap();
        root["error"] = Value::Null;

        let result = format_result(&calls[0], 25936975, 0, results[0].clone()).unwrap();
        assert!(result.success);
        assert!(!result.trace.is_empty());
    }

    fn call() -> SimulationRequest {
        serde_json::from_value(json!({"chainId":1,"from":"0x0000000000000000000000000000000000000001","to":"0x0000000000000000000000000000000000000002","gasLimit":100000,"request_id":"step-1"})).unwrap()
    }
    fn trace(path: Value, from: &str, to: &str, value: &str, kind: &str) -> Value {
        json!({"type":"call","traceAddress":path,"subtraces":0,"action":{"callType":kind,"from":from,"to":to,"input":"0x","value":value,"gas":"0x13498"},"result":{"gasUsed":"0x0","output":"0x"}})
    }
    #[test]
    fn does_not_count_caught_reverts_or_delegatecall_value_as_native_movement() {
        let a = "0x0000000000000000000000000000000000000001";
        let b = "0x0000000000000000000000000000000000000002";
        let root = trace(json!([]), a, b, "0x1", "call");
        let mut failed = trace(json!([0]), b, a, "0x2", "call");
        failed["error"] = json!("Reverted");
        failed.as_object_mut().unwrap().remove("result");
        let nested = trace(json!([0, 0]), a, b, "0x3", "call");
        let delegate = trace(json!([1]), b, a, "0x4", "delegatecall");
        let r = json!({"output":"0x","trace":[root,failed,nested,delegate],"vmTrace":{"code":"0x00","ops":[{"op":"STOP","pc":0,"sub":null,"ex":{"push":[],"mem":null}}]}});
        let result = movement_traces(r["trace"].as_array().unwrap()).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].value, "0x0");
    }
    #[test]
    fn root_failure_has_no_asset_effects_and_keeps_output() {
        let a = "0x0000000000000000000000000000000000000001";
        let b = "0x0000000000000000000000000000000000000002";
        let mut root = trace(json!([]), a, b, "0x1", "call");
        root["error"] = json!("Reverted");
        root.as_object_mut().unwrap().remove("result");
        let r = json!({"output":"0x1234","trace":[root],"vmTrace":{"code":"0xfd","ops":[{"op":"REVERT","pc":0,"ex":{"used":123,"push":[],"mem":null},"sub":null}]}});
        let mut request = call();
        request.value = Some(crate::simulation::PermissiveUint(U256::from(1)));
        let result = format_result(&request, 123, 0, r).unwrap();
        assert!(!result.success);
        assert!(result.logs.is_empty());
        assert!(result.trace.is_empty());
        assert_eq!(result.return_data.to_string(), "0x1234");
    }
}
fn trace_error(trace: &Value) -> Result<Option<&str>, String> {
    match trace.get("error") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(error)) => Ok(Some(error)),
        Some(_) => Err("INVALID_TRACE_ERROR".into()),
    }
}

fn movement_traces(traces: &[Value]) -> Result<Vec<CallTrace>, String> {
    let mut failed = Vec::new();
    for trace in traces {
        if trace_error(trace)?.is_some() {
            failed.push(path(trace)?);
        }
    }
    let mut output = Vec::new();
    for t in traces {
        let p = path(t)?;
        if failed.iter().any(|f| p.starts_with(f)) {
            continue;
        }
        let a = &t["action"];
        let (kind, from, to, value, input) = match t["type"].as_str() {
            Some("call") => {
                let kind = match a["callType"].as_str() {
                    Some("call") => CallKind::Call,
                    Some("staticcall") => CallKind::StaticCall,
                    Some("delegatecall") => CallKind::DelegateCall,
                    Some("callcode") => CallKind::CallCode,
                    _ => return Err("UNKNOWN_CALL_TYPE".into()),
                };
                let value = if matches!(
                    kind,
                    CallKind::DelegateCall | CallKind::CallCode | CallKind::StaticCall
                ) {
                    "0x0".into()
                } else {
                    amount(&a["value"])?
                };
                (
                    kind,
                    address(&a["from"])?,
                    address(&a["to"])?,
                    value,
                    bytes(&a["input"])?,
                )
            }
            Some("create") => (
                CallKind::Create,
                address(&a["from"])?,
                address(&t["result"]["address"])?,
                amount(&a["value"])?,
                Bytes::new(),
            ),
            Some("suicide") => (
                CallKind::Call,
                address(&a["address"])?,
                address(&a["refundAddress"])?,
                amount(&a["balance"])?,
                Bytes::new(),
            ),
            _ => return Err("UNKNOWN_TRACE_TYPE".into()),
        };
        output.push(CallTrace {
            call_type: kind,
            from,
            to,
            value,
            function_signature: if input.len() >= 4 {
                Bytes::copy_from_slice(&input[..4])
            } else {
                Bytes::from(vec![0; 4])
            },
        });
    }
    Ok(output)
}
fn amount(v: &Value) -> Result<String, String> {
    let s = v.as_str().ok_or("MISSING_VALUE")?;
    U256::from_str(s).map_err(|_| "INVALID_VALUE")?;
    Ok(s.into())
}
#[cfg(test)]
fn format_result(
    call: &SimulationRequest,
    block: u64,
    index: usize,
    result: Value,
) -> Result<SimulationResponse, String> {
    format_result_with_logs(call, block, index, &result, None).map_err(NormalizeError::into_message)
}

fn format_result_with_logs(
    call: &SimulationRequest,
    block: u64,
    index: usize,
    result: &Value,
    recovered_logs: Option<&Vec<Value>>,
) -> Result<SimulationResponse, NormalizeError> {
    let traces = result["trace"]
        .as_array()
        .filter(|t| !t.is_empty())
        .ok_or("MISSING_TRACE")?;
    let roots = traces
        .iter()
        .filter(|t| t["traceAddress"] == json!([]))
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err("INVALID_ROOT_TRACE".into());
    }
    let root = roots[0];
    // Results stay paired by index, including identical calls. Validate the root
    // against that request before using its execution result or echoing its ID.
    let action = &root["action"];
    if root["type"] != "call"
        || action["callType"] != "call"
        || address(&action["from"])? != call.from
        || address(&action["to"])? != call.to
        || bytes(&action["input"])? != call.data.clone().unwrap_or_default()
        || U256::from_str(action["value"].as_str().ok_or("MISSING_VALUE")?)
            .map_err(|_| "INVALID_VALUE")?
            != call.value.map(|v| v.0).unwrap_or_default()
    {
        return Err("ROOT_CALL_MISMATCH".into());
    }
    let root_error = trace_error(root)?;
    let success = root_error.is_none();
    let gas_limit = call.gas_limit.unwrap_or(DEFAULT_GAS);
    let gas_used = if success {
        let intrinsic = gas_limit
            .checked_sub(hex_number(&root["action"]["gas"])?)
            .ok_or("INVALID_TRACE_GAS")?;
        intrinsic
            .checked_add(hex_number(&root["result"]["gasUsed"])?)
            .ok_or("INVALID_TRACE_GAS")?
    } else if root_error.is_some_and(|error| error.to_ascii_lowercase().contains("revert")) {
        let used = &result["vmTrace"]["ops"]
            .as_array()
            .and_then(|ops| ops.last())
            .ok_or("MISSING_REVERT_TRACE")?["ex"]["used"];
        let remaining = used.as_u64().map(Ok).unwrap_or_else(|| hex_number(used))?;
        gas_limit
            .checked_sub(remaining)
            .ok_or("INVALID_REVERT_GAS")?
    } else {
        gas_limit
    };
    if gas_used > gas_limit {
        return Err("INVALID_TRACE_GAS".into());
    }
    let raw_logs = match recovered_logs {
        Some(logs) => logs.clone(),
        None => super::vm_trace::extract_logs(&result["vmTrace"], traces)
            .map_err(NormalizeError::VmLogDecode)?,
    };
    let logs = raw_logs
        .iter()
        .map(|l| {
            let topics = l["topics"]
                .as_array()
                .ok_or("MISSING_LOG_TOPICS")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or("INVALID_TOPIC")?
                        .parse::<B256>()
                        .map_err(|_| "INVALID_TOPIC")
                })
                .collect::<Result<Vec<_>, _>>()?;
            Log::new(address(&l["address"])?, topics, bytes(&l["data"])?)
                .ok_or_else(|| "INVALID_LOG".to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(SimulationResponse {
        request_id: call.request_id.clone(),
        simulation_id: (index + 1) as u64,
        block_number: block,
        gas_used,
        success,
        trace: movement_traces(traces)?,
        logs,
        exit_reason: if success {
            InstructionResult::Return
        } else {
            failure_reason(root_error.ok_or("INVALID_TRACE_ERROR")?)
        },
        return_data: bytes(&result["output"])?,
        state_diff: if call.include_state_diff == Some(true) {
            result.get("stateDiff").cloned().filter(|v| !v.is_null())
        } else {
            None
        },
    })
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[tokio::test]
    async fn validates_before_any_provider_call() {
        let api = route().recover(crate::errors::handle_rejection);
        let res = warp::test::request()
            .method("POST")
            .path("/simulate_bundle")
            .json(&json!([]))
            .reply(&api)
            .await;
        assert_eq!(res.status(), 400);
        let mut calls: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_requests.json"
        ))
        .unwrap();
        calls[2]["chainId"] = json!(56);
        let res = warp::test::request()
            .method("POST")
            .path("/simulate_bundle")
            .json(&calls)
            .reply(&api)
            .await;
        assert_eq!(res.status(), 400);
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn shared_state_route_uses_one_rpc_and_preserves_events_gas_and_ids() {
        let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = observed.clone();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_usdt_response.json"
        ))
        .unwrap();
        let rpc_route = warp::post()
            .and(warp::path("1"))
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(warp::body::json())
            .map(
                move |query: std::collections::HashMap<String, String>, body: Value| {
                    assert_eq!(query.get("provider").map(String::as_str), Some("quicknode"));
                    seen.lock().unwrap().push(body.clone());
                    warp::reply::json(&match body["method"].as_str().unwrap() {
                        "eth_chainId" => json!({"jsonrpc":"2.0","id":1,"result":"0x1"}),
                        "eth_blockNumber" => json!({"jsonrpc":"2.0","id":1,"result":"0x18bc44f"}),
                        "trace_callMany" => fixture.clone(),
                        _ => panic!("unexpected RPC"),
                    })
                },
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(warp::serve(rpc_route).incoming(listener).run());
        temp_env::async_with_vars(
            [("BASE_BLOCKCHAIN_NODE_URL", Some(format!("http://{addr}")))],
            async {
                let mut calls: Value = serde_json::from_str(include_str!(
                    "../../../tests/fixtures/bundle_v2_requests.json"
                ))
                .unwrap();
                for c in calls.as_array_mut().unwrap() {
                    c.as_object_mut().unwrap().remove("blockNumber");
                }
                let res = warp::test::request()
                    .method("POST")
                    .path("/simulate_bundle")
                    .json(&calls)
                    .reply(&route().recover(crate::errors::handle_rejection))
                    .await;
                assert_eq!(res.status(), 200, "{}", String::from_utf8_lossy(res.body()));
                let actual: Value = serde_json::from_slice(res.body()).unwrap();
                for (i, gas) in [48549, 68788, 51700].iter().enumerate() {
                    assert_eq!(actual[i]["success"], true);
                    assert_eq!(actual[i]["gasUsed"], *gas);
                    assert_eq!(actual[i]["request_id"], format!("call-{}", i + 1));
                    assert_eq!(actual[i]["simulationId"], i + 1);
                    assert_eq!(actual[i]["logs"].as_array().unwrap().len(), 1);
                }
                assert_eq!(
                    actual[0]["logs"][0]["data"],
                    "0x0000000000000000000000000000000000000000000000000000000005f5e100"
                );
                assert_eq!(
                    actual[1]["logs"][0]["data"],
                    "0x0000000000000000000000000000000000000000000000000000000000989680"
                );
                assert_eq!(
                    actual[2]["logs"][0]["data"],
                    "0x0000000000000000000000000000000000000000000000000000000001c9c380"
                );
                let seen = observed.lock().unwrap();
                assert_eq!(seen.len(), 3);
                assert_eq!(seen[2]["method"], "trace_callMany");
                assert_eq!(seen[2]["params"][1], "0x18bc44f");
                assert_eq!(seen[2]["params"][0].as_array().unwrap().len(), 3);
            },
        )
        .await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_provider_shape_fails_without_debug_replay() {
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = observed.clone();
        let mut fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/vm_trace_usdt_response.json"
        ))
        .unwrap();
        fixture["result"][0]["trace"][0]["action"]["input"] = json!("0xdeadbeef");
        let rpc_route = warp::post()
            .and(warp::body::json())
            .map(move |body: Value| {
                let method = body["method"].as_str().unwrap().to_owned();
                seen.lock().unwrap().push(method.clone());
                warp::reply::json(&match method.as_str() {
                    "eth_chainId" => json!({"result":"0x1"}),
                    "trace_callMany" => fixture.clone(),
                    "debug_traceCallMany" => json!({"result": []}),
                    other => panic!("unexpected RPC {other}"),
                })
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(warp::serve(rpc_route).incoming(listener).run());
        let calls: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_requests.json"
        ))
        .unwrap();

        temp_env::async_with_vars(
            [("BASE_BLOCKCHAIN_NODE_URL", Some(format!("http://{addr}")))],
            async {
                let response = warp::test::request()
                    .method("POST")
                    .path("/simulate_bundle")
                    .json(&calls)
                    .reply(&route().recover(crate::errors::handle_rejection))
                    .await;
                assert_eq!(response.status(), 502);
                let body: Value = serde_json::from_slice(response.body()).unwrap();
                assert_eq!(body["message"], "INVALID_BUNDLE_RESULT: ROOT_CALL_MISMATCH");
                assert_eq!(*observed.lock().unwrap(), ["eth_chainId", "trace_callMany"]);
            },
        )
        .await;
        server.abort();
    }
}

#[cfg(test)]
mod rpc_error_tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn rpc_failure_is_reported_and_never_converted_to_empty_success() {
        let rpc_route=warp::post().map(||warp::reply::json(&json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}})));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(warp::serve(rpc_route).incoming(listener).run());
        let result = rpc(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            "trace_callMany",
            json!([]),
        )
        .await;
        let err = result.unwrap_err();
        let err = err.find::<BundleV2Error>().unwrap();
        assert_eq!(err.status, 502);
        assert_eq!(err.message, "QUICKNODE_RPC_ERROR_trace_callMany");
        server.abort();
    }
}

#[cfg(test)]
mod decode_gate_tests {
    use super::*;
    use std::sync::mpsc;

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_caller_does_not_release_permit_before_decode_finishes() {
        let semaphore = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_gate = semaphore.clone();
        let first = tokio::spawn(async move {
            spawn_decode_with(first_gate, move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .await
        });
        started_rx.await.unwrap();
        first.abort();

        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            spawn_decode_with(semaphore.clone(), || ()),
        )
        .await;
        assert!(blocked.is_err());

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), spawn_decode_with(semaphore, || ()))
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(test)]
mod exit_reason_tests {
    use super::*;
    #[test]
    fn distinguishes_runtime_revert_out_of_gas_and_invalid_opcode() {
        assert_eq!(failure_reason("Reverted"), InstructionResult::Revert);
        assert_eq!(failure_reason("Out of gas"), InstructionResult::OutOfGas);
        assert_eq!(
            failure_reason("invalid opcode: INVALID"),
            InstructionResult::InvalidFEOpcode
        );
        assert_eq!(
            failure_reason("Stack underflow"),
            InstructionResult::StackUnderflow
        );
    }
}
fn failure_reason(error: &str) -> InstructionResult {
    let error = error.to_ascii_lowercase();
    if error.contains("revert") {
        InstructionResult::Revert
    } else if error.contains("out of gas") {
        InstructionResult::OutOfGas
    } else if error.contains("invalid opcode: invalid") || error.contains("bad instruction") {
        InstructionResult::InvalidFEOpcode
    } else if error.contains("invalid opcode") {
        InstructionResult::OpcodeNotFound
    } else if error.contains("stack underflow") {
        InstructionResult::StackUnderflow
    } else if error.contains("stack overflow") {
        InstructionResult::StackOverflow
    } else if error.contains("jump") {
        InstructionResult::InvalidJump
    } else if error.contains("static") {
        InstructionResult::StateChangeDuringStaticCall
    } else {
        InstructionResult::FatalExternalError
    }
}

#[cfg(test)]
mod bsc97_recovery_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test(flavor = "multi_thread")]
    async fn recovers_corrupt_vm_trace_by_replaying_the_whole_pinned_bundle() {
        let parity: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_bsc97_invalid_vm.json"
        ))
        .unwrap();
        let debug: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_bsc97_debug_many.json"
        ))
        .unwrap();
        let calls: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/bundle_v2_bsc97_requests.json"
        ))
        .unwrap();
        let invalid = &parity["result"][2];
        assert!(crate::api::v2::vm_trace::extract_logs(
            &invalid["vmTrace"],
            invalid["trace"].as_array().unwrap()
        )
        .unwrap_err()
        .contains("4 unmapped child"));
        let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = observed.clone();
        let rpc_route = warp::post()
            .and(warp::body::json())
            .map(move |body: Value| {
                seen.lock().unwrap().push(body.clone());
                warp::reply::json(&match body["method"].as_str().unwrap() {
                    "eth_chainId" => json!({"result":"0x61"}),
                    "trace_callMany" => parity.clone(),
                    "debug_traceCallMany" => debug.clone(),
                    other => panic!("unexpected RPC {other}"),
                })
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(warp::serve(rpc_route).incoming(listener).run());
        temp_env::async_with_vars(
            [("BASE_BLOCKCHAIN_NODE_URL", Some(format!("http://{addr}")))],
            async {
                let res = warp::test::request()
                    .method("POST")
                    .path("/simulate_bundle")
                    .json(&calls)
                    .reply(&route().recover(crate::errors::handle_rejection))
                    .await;
                assert_eq!(res.status(), 200, "{}", String::from_utf8_lossy(res.body()));
                let result: Value = serde_json::from_slice(res.body()).unwrap();
                let expected_logs: Value = serde_json::from_str(include_str!(
                    "../../../tests/fixtures/bundle_v2_bsc97_expected_logs.json"
                ))
                .unwrap();
                for (i, n) in [1, 1, 6].iter().enumerate() {
                    assert_eq!(result[i]["logs"], expected_logs[i]);
                    assert_eq!(result[i]["logs"].as_array().unwrap().len(), *n);
                    assert_eq!(result[i]["success"], true);
                    assert_eq!(result[i]["request_id"], format!("repro:{i}"));
                    assert_eq!(result[i]["simulationId"], i + 1);
                    assert_eq!(result[i]["blockNumber"], 129961533);
                    assert_eq!(result[i]["gasAccounting"], "execution_plus_intrinsic");
                }
                assert_eq!(
                    result[2]["logs"][0]["address"],
                    "0xae13d989dac2f0debff460ac112a837c89baa7cd"
                );
                assert_eq!(
                    result[2]["logs"][1]["address"],
                    "0x8d008b313c1d6c7fe2982f62d32da7507cf43551"
                );
                assert_eq!(
                    result[2]["logs"][5]["address"],
                    "0xbbfe33dffe3ca5bf5c55e333b1ec8f71518b9cfc"
                );
                let seen = observed.lock().unwrap();
                assert_eq!(seen.len(), 3);
                assert_eq!(seen[2]["method"], "debug_traceCallMany");
                let transactions = seen[2]["params"][0][0]["transactions"].as_array().unwrap();
                assert_eq!(transactions.len(), 3);
                for (i, tx) in transactions.iter().enumerate() {
                    assert_eq!(tx, &seen[1]["params"][0][i][0]);
                }
                assert_eq!(
                    seen[2]["params"][1],
                    json!({"blockNumber":"0x7bf0e3d","transactionIndex":-1})
                );
                assert_eq!(
                    seen[2]["params"][2],
                    json!({"tracer":"callTracer","tracerConfig":{"withLog":true}})
                );
            },
        )
        .await;
        server.abort();
    }
}
