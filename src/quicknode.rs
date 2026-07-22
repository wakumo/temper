use std::env;

use alloy::hex;
use alloy::network::AnyNetwork;
use alloy::primitives::{Address, Bytes, Log, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use foundry_evm::revm::interpreter::InstructionResult;
use revm_inspectors::tracing::types::CallKind;
use serde_json::Value;

use crate::simulation::{CallTrace, SimulationRequest, SimulationResponse};

const DEFAULT_GAS_LIMIT: u64 = 30_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickNodeSkip {
    MissingConfig,
    StateOverrides,
    StateDiffRequested,
}

#[derive(Debug)]
pub struct QuickNodeError(String);

impl std::fmt::Display for QuickNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for QuickNodeError {}

pub fn quicknode_url(chain_id: u64) -> Result<String, QuickNodeSkip> {
    let base_url =
        env::var("BASE_BLOCKCHAIN_NODE_URL").map_err(|_| QuickNodeSkip::MissingConfig)?;
    Ok(format!(
        "{}/{}?provider=quicknode",
        base_url.trim_end_matches('/'),
        chain_id
    ))
}

fn hex_u64(value: u64) -> String {
    format!("0x{value:x}")
}

fn hex_u256(value: U256) -> String {
    format!("0x{value:x}")
}

fn hex_bytes(value: Option<&Bytes>) -> String {
    value
        .map(ToString::to_string)
        .unwrap_or_else(|| "0x".to_string())
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
        return Ok(Bytes::from(vec![0, 0, 0, 0]));
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
    parse_hex_u64(
        log.get("index")
            .or_else(|| log.get("position"))
            .and_then(Value::as_str),
    )
    .unwrap_or(u64::MAX)
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

fn collect_call(
    call: &Value,
    traces: &mut Vec<CallTrace>,
    logs: &mut Vec<(u64, Log)>,
) -> Result<(), QuickNodeError> {
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
        request_id: _transaction.request_id.clone(),
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

pub fn quicknode_skip_reason(transaction: &SimulationRequest) -> Option<QuickNodeSkip> {
    if transaction.state_overrides.is_some() {
        return Some(QuickNodeSkip::StateOverrides);
    }

    if transaction.include_state_diff == Some(true) {
        return Some(QuickNodeSkip::StateDiffRequested);
    }

    None
}

pub fn quicknode_skip_log(reason: Option<&QuickNodeSkip>) -> &'static str {
    match reason {
        None => "eligible",
        Some(QuickNodeSkip::StateOverrides) => "state overrides present",
        Some(QuickNodeSkip::StateDiffRequested) => "state diff requested",
        Some(QuickNodeSkip::MissingConfig) => "missing QuickNode config",
    }
}

async fn quicknode_block_number(
    provider: &impl Provider<AnyNetwork>,
    requested: Option<u64>,
) -> Result<u64, QuickNodeError> {
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

pub async fn simulate_with_quicknode(
    transaction: &SimulationRequest,
) -> Result<Option<SimulationResponse>, QuickNodeError> {
    let skip_reason = quicknode_skip_reason(transaction);
    if skip_reason.is_some() {
        log::info!(
            target: "ts::api",
            "QuickNode skipped: {}",
            quicknode_skip_log(skip_reason.as_ref())
        );
        return Ok(None);
    }

    let url = match quicknode_url(transaction.chain_id) {
        Ok(url) => url,
        Err(QuickNodeSkip::MissingConfig) => {
            log::info!(
                target: "ts::api",
                "simulate handled by local workflow: QuickNode unavailable"
            );
            return Ok(None);
        }
        Err(err) => return Err(QuickNodeError(format!("quicknode config error: {err:?}"))),
    };

    let provider = ProviderBuilder::new().network::<AnyNetwork>().connect_http(
        url.parse()
            .map_err(|err| QuickNodeError(format!("invalid QuickNode URL: {err}")))?,
    );

    let block_number = quicknode_block_number(&provider, transaction.block_number).await?;
    let body = debug_trace_call_body(transaction);
    let result = provider
        .client()
        .request::<_, Value>("debug_traceCall", body["params"].clone())
        .await
        .map_err(|err| QuickNodeError(format!("debug_traceCall failed: {err}")))?;

    let response = format_quicknode_result(transaction, block_number, result)?;
    log::info!(
        target: "ts::api",
        "simulate handled by QuickNode debug_traceCall"
    );
    Ok(Some(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::PermissiveUint;
    use std::str::FromStr;

    fn request(chain_id: u64) -> SimulationRequest {
        SimulationRequest {
            request_id: None,
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
    fn quicknode_url_uses_shared_base_with_provider_query() {
        temp_env::with_vars(
            [(
                "BASE_BLOCKCHAIN_NODE_URL",
                Some("https://nodes.example.com/"),
            )],
            || {
                let url = quicknode_url(56).unwrap();
                assert_eq!(url, "https://nodes.example.com/56?provider=quicknode");
            },
        );
    }

    #[test]
    fn quicknode_url_supports_any_chain_id() {
        temp_env::with_vars(
            [(
                "BASE_BLOCKCHAIN_NODE_URL",
                Some("https://nodes.example.com"),
            )],
            || {
                assert_eq!(
                    quicknode_url(1).unwrap(),
                    "https://nodes.example.com/1?provider=quicknode"
                );
                assert_eq!(
                    quicknode_url(8453).unwrap(),
                    "https://nodes.example.com/8453?provider=quicknode"
                );
            },
        );
    }

    #[test]
    fn quicknode_url_requires_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", None::<&str>, || {
            assert_eq!(quicknode_url(56), Err(QuickNodeSkip::MissingConfig));
        });
    }

    #[test]
    fn debug_trace_call_body_matches_quicknode_curl_shape() {
        let body = debug_trace_call_body(&request(56));

        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(body["method"], "debug_traceCall");
        assert_eq!(
            body["params"][0]["from"],
            "0x0000000000000000000000000000000000000001"
        );
        assert_eq!(
            body["params"][0]["to"],
            "0x0000000000000000000000000000000000000002"
        );
        assert_eq!(body["params"][0]["value"], "0x7");
        assert_eq!(body["params"][0]["data"], "0x12345678aa");
        assert_eq!(body["params"][0]["gas"], "0x5208");
        assert_eq!(body["params"][1], "0x6a18137");
        assert_eq!(body["params"][2]["tracer"], "callTracer");
        assert_eq!(body["params"][2]["tracerConfig"]["withLog"], true);
    }

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
        assert_eq!(
            response.trace[0].call_type,
            revm_inspectors::tracing::types::CallKind::Call
        );
        assert_eq!(
            response.trace[0].function_signature,
            Bytes::from(vec![0x12, 0x34, 0x56, 0x78])
        );
        assert_eq!(
            response.trace[1].call_type,
            revm_inspectors::tracing::types::CallKind::StaticCall
        );
        assert_eq!(response.logs.len(), 2);
        assert_eq!(response.return_data, Bytes::from(vec![0xab, 0xcd, 0xef]));
        assert_eq!(response.state_diff, None);
    }

    #[test]
    fn quicknode_result_sorts_logs_by_position_when_index_absent() {
        let result = serde_json::json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "type": "CALL",
            "input": "0x12345678",
            "gasUsed": "0x10",
            "logs": [{
                "address": "0x0000000000000000000000000000000000000002",
                "topics": ["0xe1fffcc4923d04b559f4d29a8bfc6cda04eb5b0d3c460751c2402c5c5cc9109c"],
                "data": "0x01",
                "position": "0x1"
            }],
            "calls": [{
                "from": "0x0000000000000000000000000000000000000002",
                "to": "0x0000000000000000000000000000000000000003",
                "type": "CALL",
                "input": "0x12345678",
                "gasUsed": "0x1",
                "logs": [{
                    "address": "0x0000000000000000000000000000000000000003",
                    "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                    "data": "0x00",
                    "position": "0x0"
                }]
            }]
        });

        let response = format_quicknode_result(&request(56), 111_247_671, result).unwrap();

        assert_eq!(response.logs[0].data.data, Bytes::from(vec![0]));
        assert_eq!(response.logs[1].data.data, Bytes::from(vec![1]));
    }

    #[test]
    fn quicknode_result_uses_four_zero_bytes_for_short_function_signature() {
        let result = serde_json::json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "type": "CALL",
            "input": "0x12",
            "gasUsed": "0x10"
        });

        let response = format_quicknode_result(&request(56), 111_247_671, result).unwrap();

        assert_eq!(
            response.trace[0].function_signature,
            Bytes::from(vec![0, 0, 0, 0])
        );
    }

    #[test]
    fn quicknode_result_echoes_request_id() {
        let mut req = request(56);
        req.request_id = Some("request-a".to_string());
        let result = serde_json::json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "type": "CALL",
            "input": "0x12345678",
            "gasUsed": "0x5208",
            "output": "0x"
        });

        let response = format_quicknode_result(&req, 111_247_671, result).unwrap();

        assert_eq!(response.request_id.as_deref(), Some("request-a"));
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
        assert_eq!(
            response.exit_reason,
            foundry_evm::revm::interpreter::InstructionResult::Revert
        );
    }

    #[test]
    fn quicknode_skip_gate_rejects_state_diff_and_state_overrides() {
        let mut state_diff_request = request(56);
        state_diff_request.include_state_diff = Some(true);
        assert_eq!(
            quicknode_skip_reason(&state_diff_request),
            Some(QuickNodeSkip::StateDiffRequested)
        );

        let mut override_request = request(56);
        override_request.state_overrides = Some(std::collections::HashMap::new());
        assert_eq!(
            quicknode_skip_reason(&override_request),
            Some(QuickNodeSkip::StateOverrides)
        );

        assert_eq!(quicknode_skip_reason(&request(56)), None);
    }

    #[test]
    fn quicknode_skip_gate_allows_missing_state_diff_flag_by_default() {
        let mut transaction = request(56);
        transaction.include_state_diff = None;

        assert_eq!(quicknode_skip_reason(&transaction), None);
    }

    #[test]
    fn quicknode_skip_reason_formats_for_logs() {
        assert_eq!(quicknode_skip_log(None), "eligible");
        assert_eq!(
            quicknode_skip_log(Some(&QuickNodeSkip::StateDiffRequested)),
            "state diff requested"
        );
        assert_eq!(
            quicknode_skip_log(Some(&QuickNodeSkip::StateOverrides)),
            "state overrides present"
        );
    }
}
