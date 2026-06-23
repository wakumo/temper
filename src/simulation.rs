use std::env;
use std::collections::HashMap;
use std::str::FromStr;

use std::sync::Arc;

use dashmap::mapref::one::RefMut;
use alloy::primitives::{
    Address, Bytes, U256, B256, Log,
};
use alloy_eip2930::AccessList;
use foundry_evm::revm::interpreter::InstructionResult;
use revm_inspectors::tracing::types::CallKind;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;
use warp::reply::Json;
use warp::{Rejection, Reply, Filter};

use crate::errors::{
    IncorrectChainIdError, InvalidBlockNumbersError, MultipleChainIdsError, NoURLForChainIdError,
    StateNotFound, InvalidGasPriceError,
};
use crate::evm::StorageOverride;
use crate::SharedSimulationState;

use super::config::Config;
use super::evm::{CallRawRequest, Evm};
use std::time::Instant;
use reqwest;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationRequest {
    pub chain_id: u64,
    pub from: Address,
    pub to: Address,
    pub data: Option<Bytes>,
    pub gas_limit: u64,
    pub value: Option<PermissiveUint>,
    pub access_list: Option<AccessList>,
    pub block_number: Option<u64>,
    pub state_overrides: Option<HashMap<Address, StateOverride>>,
    pub format_trace: Option<bool>,
    pub allow_insufficient_funds: Option<bool>,
    pub gas_price: Option<String>, // in gwei format
    // pub commit: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResponse {
    pub simulation_id: u64,
    pub gas_used: u64,
    pub block_number: u64,
    pub success: bool,
    pub trace: Vec<CallTrace>,
    pub logs: Vec<Log>,
    pub exit_reason: InstructionResult,
    pub return_data: Bytes,
    pub state_diff: Option<serde_json::Value>, // State changes from trace
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSimulationRequest {
    pub chain_id: u64,
    pub gas_limit: u64,
    pub block_number: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSimulationResponse {
    pub stateful_simulation_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatefulSimulationEndResponse {
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateOverride {
    pub balance: Option<PermissiveUint>,
    pub nonce: Option<u64>,
    pub code: Option<Bytes>,
    #[serde(flatten)]
    pub state: Option<State>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum State {
    Full {
        state: HashMap<B256, U256>,
    },
    #[serde(rename_all = "camelCase")]
    Diff {
        state_diff: HashMap<B256, U256>,
    },
}

impl From<State> for StorageOverride {
    fn from(value: State) -> Self {
        let (slots, diff) = match value {
            State::Full { state } => (state, false),
            State::Diff { state_diff } => (state_diff, true),
        };

        StorageOverride {
            slots: slots
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            diff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_request_accepts_allow_insufficient_funds() {
        let request: SimulationRequest = serde_json::from_value(serde_json::json!({
            "chainId": 1,
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "gasLimit": 21_000,
            "allowInsufficientFunds": true
        }))
        .unwrap();

        assert_eq!(request.allow_insufficient_funds, Some(true));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CallTrace {
    pub call_type: CallKind,
    pub from: Address,
    pub to: Address,
    pub function_signature: Bytes,
    pub value: String, // ✅ Changed to String to preserve hex format
}

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq)]
#[serde(transparent)]
pub struct PermissiveUint(pub U256);

impl From<PermissiveUint> for U256 {
    fn from(value: PermissiveUint) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for PermissiveUint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Accept value in hex or decimal formats
        let value = String::deserialize(deserializer)?;
        let parsed = if value.starts_with("0x") {
            U256::from_str(&value).map_err(serde::de::Error::custom)?
        } else {
            U256::from_str(&value).map_err(serde::de::Error::custom)?
        };
        Ok(Self(parsed))
    }
}

fn construct_url(base_url: &str) -> Result<String, Rejection> {
    let mut url = base_url.to_string();

    if url.starts_with("https://rpc.ankr.com") {
        if let Ok(token) = env::var("ANKR_ACCESS_TOKEN") {
            // Append the access token to the URL
            url.push_str(&format!("/{}", token));
        }
    }

    Ok(url)
}

fn chain_id_to_fork_url(chain_id: u64) -> Result<String, Rejection> {
    let url = match chain_id {
        // Ethereum
        1 => "https://rpc.ankr.com/eth",
        // 1 => "https://eth.llamarpc.com",
        5 => "https://rpc.ankr.com/eth_goerli",
        11155111 => "https://sepolia.gateway.tenderly.co",
        // Polygon
        137 => "https://rpc.ankr.com/polygon",
        // 137 => "https://polygon-rpc.com",
        80001 => "https://rpc.ankr.com/polygon_mumbai",
        // Polygon zkEVM
        1101 => "https://rpc.ankr.com/polygon_zkevm",
        1442 => "https://rpc.ankr.com/polygon_zkevm_testnet",
        // Avalanche
        43114 => "https://api.avax.network/ext/bc/C/rpc",
        43113 => "https://api.avax-test.network/ext/bc/C/rpc",
        // Fantom
        250 => "https://rpcapi.fantom.network/",
        4002 => "https://rpc.testnet.fantom.network/",
        // xDai
        100 => "https://rpc.xdaichain.com/",
        // BSC
        56 => "https://rpc.ankr.com/bsc",
        97 => "https://rpc.ankr.com/bsc_testnet_chapel",
        // Arbitrum
        42161 => "https://arb1.arbitrum.io/rpc",
        421613 => "https://goerli-rollup.arbitrum.io/rpc",
        // Optimism
        10 => "https://mainnet.optimism.io",
        420 => "https://rpc.ankr.com/optimism_sepolia",
        _ => return Err(NoURLForChainIdError.into()),
    };

    construct_url(url)
}

async fn run(
    evm: &mut Evm,
    transaction: SimulationRequest,
    _commit: bool,
) -> Result<SimulationResponse, Rejection> {
    let start_time = Instant::now();

    // ⏱️ STATE OVERRIDES
    let override_start = Instant::now();
    for (address, state_override) in transaction.state_overrides.into_iter().flatten() {
        evm.override_account(
            address,
            state_override.balance.map(|b| b.0),
            state_override.nonce,
            state_override.code,
            state_override.state.map(StorageOverride::from),
        )?;
    }
    let override_time = override_start.elapsed();

    // ⏱️ GAS PRICE PARSING
    let gas_parse_start = Instant::now();
    let gas_price = match transaction.gas_price {
        Some(gas_price_str) => {
            match gas_price_str.parse::<f64>() {
                Ok(gas_price_gwei) => {
                    let gas_price_wei = (gas_price_gwei * 1_000_000_000.0) as u128;
                    gas_price_wei
                }
                Err(e) => {
                    log::error!("Invalid gas price format '{}': {}", gas_price_str, e);
                    return Err(warp::reject::custom(InvalidGasPriceError(gas_price_str)));
                }
            }
        }
        None => 20_000_000_000u128, // Default: 20 Gwei
    };
    let gas_parse_time = gas_parse_start.elapsed();

    // ⏱️ CALL SETUP
    let call_setup_start = Instant::now();
    let call = CallRawRequest {
        from: transaction.from,
        to: transaction.to,
        value: transaction.value.map(|v| v.0),
        data: transaction.data,
        access_list: transaction.access_list,
        format_trace: transaction.format_trace.unwrap_or_default(),
        allow_insufficient_funds: transaction.allow_insufficient_funds.unwrap_or_default(),
        gas_limit: transaction.gas_limit,
        gas_price,
    };
    let call_setup_time = call_setup_start.elapsed();

    // ⏱️ EVM EXECUTION
    let execution_start = Instant::now();
    let result = evm.call_raw(call).await?;
    let execution_time = execution_start.elapsed();

    // ⏱️ RESPONSE BUILD
    let response_start = Instant::now();
    let response = SimulationResponse {
        simulation_id: 1,
        gas_used: result.gas_used,
        block_number: result.block_number,
        success: result.success,
        trace: result.call_traces,
        logs: result.logs,
        exit_reason: result.exit_reason,
        return_data: alloy::primitives::Bytes::from(result.return_data.to_vec()),
        state_diff: result.state_diff,
    };
    let response_time = response_start.elapsed();
    let total_time = start_time.elapsed();

    // 📊 CLEAN PERFORMANCE BENCHMARK
    println!("⏱️  SIMULATION PERFORMANCE:");
    println!("  ├─ State Overrides: {:?}", override_time);
    println!("  ├─ Gas Price Parse: {:?}", gas_parse_time);
    println!("  ├─ Call Setup:      {:?}", call_setup_time);
    println!("  ├─ EVM Execution:   {:?}", execution_time);
    println!("  ├─ Response Build:  {:?}", response_time);
    println!("  └─ Total Time:      {:?}", total_time);

    Ok(response)
}

pub async fn simulate(transaction: SimulationRequest, config: Config) -> Result<Json, Rejection> {
    let fork_url = config
        .fork_url
        .unwrap_or(chain_id_to_fork_url(transaction.chain_id)?);
    let mut evm = Evm::new(
        fork_url,
        transaction.block_number,
        transaction.gas_limit,
        config.etherscan_key,
    ).await;

    if evm.get_chain_id() != U256::from(transaction.chain_id) {
        return Err(warp::reject::custom(IncorrectChainIdError()));
    }

    let response = run(&mut evm, transaction, false).await?;

    Ok(warp::reply::json(&response))
}

pub async fn simulate_bundle(
    transactions: Vec<SimulationRequest>,
    config: Config,
) -> Result<Json, Rejection> {
    let first_chain_id = transactions[0].chain_id;
    let first_block_number = transactions[0].block_number;

    let fork_url = config
        .fork_url
        .unwrap_or(chain_id_to_fork_url(first_chain_id)?);
    let mut evm = Evm::new(
        fork_url,
        first_block_number,
        transactions[0].gas_limit,
        config.etherscan_key,
    ).await;

    if evm.get_chain_id() != U256::from(first_chain_id) {
        return Err(warp::reject::custom(IncorrectChainIdError()));
    }

    let mut response = Vec::with_capacity(transactions.len());
    for transaction in transactions {
        if transaction.chain_id != first_chain_id {
            return Err(warp::reject::custom(MultipleChainIdsError()));
        }
        if transaction.block_number != first_block_number {
            let tx_block = transaction
                .block_number
                .expect("Transaction has no block number");
            if transaction.block_number < first_block_number || tx_block < evm.get_block().try_into().unwrap_or(0)
            {
                return Err(warp::reject::custom(InvalidBlockNumbersError()));
            }
            evm.set_block(tx_block)
                .await
                .expect("Failed to set block number");
            evm.set_block_timestamp(evm.get_block_timestamp().try_into().unwrap_or(0) + 12)
                .await
                .expect("Failed to set block timestamp");
        }
        response.push(run(&mut evm, transaction, true).await?);
    }

    Ok(warp::reply::json(&response))
}

pub async fn simulate_stateful_new(
    stateful_simulation_request: StatefulSimulationRequest,
    config: Config,
    state: Arc<SharedSimulationState>,
) -> Result<Json, Rejection> {
    let fork_url = config
        .fork_url
        .unwrap_or(chain_id_to_fork_url(stateful_simulation_request.chain_id)?);
    let evm = Evm::new(
        fork_url,
        stateful_simulation_request.block_number,
        stateful_simulation_request.gas_limit,
        config.etherscan_key,
    ).await;
    let new_id = Uuid::new_v4();
    state.evms.insert(new_id, Arc::new(Mutex::new(evm)));

    let response = StatefulSimulationResponse {
        stateful_simulation_id: new_id,
    };
    Ok(warp::reply::json(&response))
}

pub async fn simulate_stateful_end(
    param: Uuid,
    state: Arc<SharedSimulationState>,
) -> Result<Json, Rejection> {
    if state.evms.contains_key(&param) {
        state.evms.remove(&param);
        let response = StatefulSimulationEndResponse { success: true };
        Ok(warp::reply::json(&response))
    } else {
        Err(warp::reject::custom(StateNotFound()))
    }
}

pub async fn simulate_stateful(
    param: Uuid,
    transactions: Vec<SimulationRequest>,
    state: Arc<SharedSimulationState>,
) -> Result<Json, Rejection> {
    let first_chain_id = transactions[0].chain_id;
    let first_block_number = transactions[0].block_number;

    let mut response = Vec::with_capacity(transactions.len());
    // Get a mutable reference to the EVM here.
    let evm_ref_mut: RefMut<'_, Uuid, Arc<Mutex<Evm>>> = state
        .evms
        .get_mut(&param)
        .ok_or_else(warp::reject::not_found)?;
    // Dereference to obtain the EVM.
    let evm = evm_ref_mut.value();
    let mut evm = evm.lock().await;
    if evm.get_chain_id() != U256::from(first_chain_id) {
        return Err(warp::reject::custom(IncorrectChainIdError()));
    }
    for transaction in transactions {
        if transaction.chain_id != first_chain_id {
            return Err(warp::reject::custom(MultipleChainIdsError()));
        }
        if let Some(tx_block_number) = transaction.block_number {
            if tx_block_number != first_block_number.unwrap_or(0)
                || tx_block_number != evm.get_block().try_into().unwrap_or(0)
            {
                if Some(tx_block_number) < first_block_number || tx_block_number < evm.get_block().try_into().unwrap_or(0)
                {
                    return Err(warp::reject::custom(InvalidBlockNumbersError()));
                }
                if let Err(_) = evm.set_block(tx_block_number).await {
                    log::error!("Failed to set block number to {}", tx_block_number);
                    return Err(warp::reject::custom(crate::errors::EvmError(eyre::eyre!("Failed to set block number"))));
                }
                let block_timestamp = evm.get_block_timestamp().try_into().unwrap_or(0);
                if let Err(_) = evm.set_block_timestamp(block_timestamp + 12).await {
                    log::error!("Failed to set block timestamp");
                    return Err(warp::reject::custom(crate::errors::EvmError(eyre::eyre!("Failed to set block timestamp"))));
                }
            }
        }
        response.push(run(&mut evm, transaction, true).await?);
    }

    Ok(warp::reply::json(&response))
}

// ===== RAW TRACE API (Direct JSON-RPC) =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectRawTraceRequest {
    pub rpc_url: String,
    pub transaction_request: serde_json::Value,
    pub trace_types: Vec<String>, // ["trace", "vmTrace", "stateDiff"]
    pub block_number: Option<String>, // Optional block number (hex format)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectRawTraceResponse {
    pub success: bool,
    pub trace_data: Option<serde_json::Value>,
    pub error: Option<String>,
    pub rpc_url: String,
}

// Direct JSON-RPC client for trace calls
pub struct DirectRpcClient {
    client: reqwest::Client,
}

impl DirectRpcClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub async fn trace_call(
        &self,
        rpc_url: &str,
        transaction_request: &serde_json::Value,
        trace_types: &[String],
        block_number: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        // Create params array with optional block number
        let mut params = vec![
            transaction_request.clone(),
            serde_json::json!(trace_types)
        ];

        // Add block number if provided
        if let Some(block) = block_number {
            params.push(serde_json::json!(block));
        }

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "trace_call",
            "params": params,
            "id": 1
        });


        let response = self
            .client
            .post(rpc_url)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;


        if !status.is_success() {
            return Err(format!("RPC call failed with status {}: {}", status, response_text));
        }

        let json_response: serde_json::Value = serde_json::from_str(&response_text)
            .map_err(|e| format!("Failed to parse JSON response: {}", e))?;

        if let Some(error) = json_response.get("error") {
            return Err(format!("RPC error: {}", error));
        }

        json_response
            .get("result")
            .cloned()
            .ok_or_else(|| "No result in RPC response".to_string())
    }
}

// Warp filter for direct raw trace API
pub fn direct_raw_trace() -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path("direct-raw-trace")
        .and(warp::post())
        .and(warp::body::json())
        .and_then(handle_direct_raw_trace)
}

async fn handle_direct_raw_trace(
    request: DirectRawTraceRequest,
) -> Result<impl Reply, Rejection> {

    let client = DirectRpcClient::new();

    // Extract and convert block number from transaction request if not provided
    let hex_block: Option<String> = if let Some(block) = &request.block_number {
        Some(block.clone())
    } else if let Some(block_num) = request.transaction_request.get("blockNumber") {
        // Convert decimal to hex if needed
        if let Some(block_decimal) = block_num.as_u64() {
            let hex_block = format!("0x{:x}", block_decimal);
            Some(hex_block)
        } else if let Some(block_str) = block_num.as_str() {
            Some(block_str.to_string())
        } else {
            None
        }
    } else {
        None
    };

    let block_number = hex_block.as_deref();

    match client
        .trace_call(&request.rpc_url, &request.transaction_request, &request.trace_types, block_number)
        .await
    {
        Ok(trace_data) => {
            let response = DirectRawTraceResponse {
                success: true,
                trace_data: Some(trace_data),
                error: None,
                rpc_url: request.rpc_url,
            };
            Ok(warp::reply::json(&response))
        }
        Err(error_msg) => {
            let response = DirectRawTraceResponse {
                success: false,
                trace_data: None,
                error: Some(error_msg),
                rpc_url: request.rpc_url,
            };
            Ok(warp::reply::json(&response))
        }
    }
}
