use std::collections::HashMap;
use std::env;
use std::str::FromStr;

use std::sync::Arc;

use alloy::primitives::{Address, Bytes, Log, B256, U256};
use alloy_eip2930::AccessList;
use dashmap::mapref::one::RefMut;
use foundry_evm::revm::interpreter::InstructionResult;
use revm_inspectors::tracing::types::CallKind;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;
use warp::reply::Json;
use warp::Rejection;

use crate::errors::{
    EmptyBundleError, IncorrectChainIdError, InvalidBlockNumbersError, InvalidGasPriceError,
    MultipleChainIdsError, NoURLForChainIdError, StateNotFound,
};
use crate::evm::StorageOverride;
use crate::quicknode::simulate_with_quicknode;
use crate::SharedSimulationState;

use super::config::Config;
use super::evm::{CallRawRequest, Evm};
use std::time::Instant;

const DEFAULT_GAS_LIMIT: u64 = 30_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationRequest {
    pub chain_id: u64,
    pub from: Address,
    pub to: Address,
    pub data: Option<Bytes>,
    pub gas_limit: Option<u64>,
    pub value: Option<PermissiveUint>,
    pub access_list: Option<AccessList>,
    pub block_number: Option<u64>,
    pub state_overrides: Option<HashMap<Address, StateOverride>>,
    pub format_trace: Option<bool>,
    pub allow_insufficient_funds: Option<bool>,
    pub include_state_diff: Option<bool>,
    pub gas_price: Option<String>, // in gwei format
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
    pub gas_limit: Option<u64>,
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

        StorageOverride { slots, diff }
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

    #[test]
    fn simulation_request_accepts_include_state_diff() {
        let request: SimulationRequest = serde_json::from_value(serde_json::json!({
            "chainId": 1,
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "gasLimit": 21_000,
            "includeStateDiff": false
        }))
        .unwrap();

        assert_eq!(request.include_state_diff, Some(false));
    }

    #[test]
    fn chain_id_to_fork_url_uses_required_shared_base_env() {
        temp_env::with_var(
            "BASE_BLOCKCHAIN_NODE_URL",
            Some("https://nodes.example.com/"),
            || {
                let url = chain_id_to_fork_url(56).unwrap();
                assert_eq!(url, "https://nodes.example.com/56");
            },
        );
    }

    #[test]
    fn chain_id_to_fork_url_requires_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", None::<&str>, || {
            assert!(chain_id_to_fork_url(56).is_err());
        });
    }

    #[test]
    fn fork_url_for_uses_config_fork_url_without_shared_base_env() {
        temp_env::with_var("BASE_BLOCKCHAIN_NODE_URL", None::<&str>, || {
            let config = Config {
                port: 8080,
                fork_url: Some("https://custom.example.com".to_string()),
                etherscan_key: None,
                api_key: None,
            };

            assert_eq!(
                fork_url_for(&config, 56).unwrap(),
                "https://custom.example.com"
            );
        });
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
        let value = String::deserialize(deserializer)?;
        let parsed = U256::from_str(&value).map_err(serde::de::Error::custom)?;
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
    let base_url = env::var("BASE_BLOCKCHAIN_NODE_URL")
        .map_err(|_| warp::reject::custom(NoURLForChainIdError))?;
    construct_url(&format!("{}/{}", base_url.trim_end_matches('/'), chain_id))
}

fn fork_url_for(config: &Config, chain_id: u64) -> Result<String, Rejection> {
    match &config.fork_url {
        Some(fork_url) => Ok(fork_url.clone()),
        None => chain_id_to_fork_url(chain_id),
    }
}

async fn run_warm_stateless(
    evm: &mut Evm,
    transaction: SimulationRequest,
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
        Some(gas_price_str) => match gas_price_str.parse::<f64>() {
            Ok(gas_price_gwei) => (gas_price_gwei * 1_000_000_000.0) as u128,
            Err(e) => {
                log::error!("Invalid gas price format '{}': {}", gas_price_str, e);
                return Err(warp::reject::custom(InvalidGasPriceError(gas_price_str)));
            }
        },
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
        include_state_diff: transaction.include_state_diff.unwrap_or(true),
        gas_limit: transaction.gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
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
    match simulate_with_quicknode(&transaction).await {
        Ok(Some(response)) => return Ok(warp::reply::json(&response)),
        Ok(None) => {}
        Err(err) => log::warn!(
            target: "ts::api",
            "simulate handled by local workflow: QuickNode simulate failed: {}",
            err
        ),
    }

    let fork_url = fork_url_for(&config, transaction.chain_id)?;
    let mut evm = Evm::new(
        fork_url,
        transaction.block_number,
        transaction.gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
        config.etherscan_key,
    )
    .await;

    if evm.get_chain_id() != U256::from(transaction.chain_id) {
        return Err(warp::reject::custom(IncorrectChainIdError()));
    }

    let response = run_warm_stateless(&mut evm, transaction).await?;

    Ok(warp::reply::json(&response))
}

pub async fn simulate_bundle(
    transactions: Vec<SimulationRequest>,
    config: Config,
) -> Result<Json, Rejection> {
    if transactions.is_empty() {
        return Err(warp::reject::custom(EmptyBundleError()));
    }

    let first_chain_id = transactions[0].chain_id;
    let first_block_number = transactions[0].block_number;

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
                log::warn!(
                    target: "ts::api",
                    "simulate bundle handled by local workflow: QuickNode simulate failed: {}",
                    err
                );
                quicknode_failed = true;
                break;
            }
        }
    }

    if !quicknode_failed && quicknode_responses.len() == transactions.len() {
        return Ok(warp::reply::json(&quicknode_responses));
    }

    let fork_url = fork_url_for(&config, first_chain_id)?;
    let mut evm = Evm::new(
        fork_url,
        first_block_number,
        transactions[0].gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
        config.etherscan_key,
    )
    .await;

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
            if transaction.block_number < first_block_number
                || tx_block < evm.get_block().try_into().unwrap_or(0)
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
        response.push(run_warm_stateless(&mut evm, transaction).await?);
    }

    Ok(warp::reply::json(&response))
}

pub async fn simulate_stateful_new(
    stateful_simulation_request: StatefulSimulationRequest,
    config: Config,
    state: Arc<SharedSimulationState>,
) -> Result<Json, Rejection> {
    let fork_url = fork_url_for(&config, stateful_simulation_request.chain_id)?;
    let evm = Evm::new(
        fork_url,
        stateful_simulation_request.block_number,
        stateful_simulation_request
            .gas_limit
            .unwrap_or(DEFAULT_GAS_LIMIT),
        config.etherscan_key,
    )
    .await;
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
                if Some(tx_block_number) < first_block_number
                    || tx_block_number < evm.get_block().try_into().unwrap_or(0)
                {
                    return Err(warp::reject::custom(InvalidBlockNumbersError()));
                }
                if (evm.set_block(tx_block_number).await).is_err() {
                    log::error!("Failed to set block number to {}", tx_block_number);
                    return Err(warp::reject::custom(crate::errors::EvmError(eyre::eyre!(
                        "Failed to set block number"
                    ))));
                }
                let block_timestamp = evm.get_block_timestamp().try_into().unwrap_or(0);
                if (evm.set_block_timestamp(block_timestamp + 12).await).is_err() {
                    log::error!("Failed to set block timestamp");
                    return Err(warp::reject::custom(crate::errors::EvmError(eyre::eyre!(
                        "Failed to set block timestamp"
                    ))));
                }
            }
        }
        response.push(run_warm_stateless(&mut evm, transaction).await?);
    }

    Ok(warp::reply::json(&response))
}
