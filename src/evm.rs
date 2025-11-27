use crate::errors::{EvmError, OverrideError};
use crate::simulation::CallTrace;
use alloy::primitives::{hex, Address, Bytes as AlloyBytes, Log, B256, U256};
use serde_json;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Instant;

use alloy::providers::Identity;
use alloy::providers::{
    ext::TraceApi,
    fillers::{BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller},
};
use alloy::rpc::types::trace::parity::TraceResults;
use alloy::rpc::types::TransactionInputKind;
use alloy::serde::WithOtherFields;
use alloy::{
    consensus::BlockHeader,
    eips::BlockId,
    network::{AnyNetwork, AnyRpcBlock, TransactionBuilder},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
};
use alloy_eip2930::AccessList;
use alloy_evm::{eth::EthEvmContext, EthEvm, Evm as AlloyEvm};
use eyre::Result;
use foundry_evm::traces::CallKind;
use foundry_evm::traces::CallTraceNode;
use foundry_evm::traces::SparsedTraceArena;
use foundry_fork_db::{cache::BlockchainDbMeta, BlockchainDb, SharedBackend};
use revm::context::result::ExecutionResult;
use revm::state::EvmState;
use revm::{
    context::{BlockEnv, Evm as RevmEvm, TxEnv},
    context_interface::block::BlobExcessGasAndPrice,
    database::WrapDatabaseRef,
    handler::{instructions::EthInstructions, EthPrecompiles},
    inspector::NoOpInspector,
    DatabaseRef,
};
// use revm_inspectors::tracing::{TracingInspector};
// use revm::{db::CacheDB, DatabaseRef, Evm};
// use revm_primitives::{BlobExcessGasAndPrice, BlockEnv, TxEnv};

// Utility function to convert U256 to revm's U256
#[allow(dead_code)]
fn u256_to_ru256(value: U256) -> foundry_evm::revm::primitives::U256 {
    foundry_evm::revm::primitives::U256::from_be_bytes(value.to_be_bytes::<32>())
}

// Utility function to convert Address (just return as-is for now)
#[allow(dead_code)]
fn h160_to_b160(addr: Address) -> Address {
    addr
}
// use revm::{
//     DatabaseRef,
// };
use foundry_evm::revm::interpreter::InstructionResult;

#[derive(Debug, Clone)]
pub struct CallRawRequest {
    pub from: Address,
    pub to: Address,
    pub value: Option<U256>,
    pub data: Option<AlloyBytes>,
    pub access_list: Option<AccessList>,
    pub format_trace: bool,
    pub gas_limit: u64,
    pub gas_price: u128,
}

#[derive(Debug, Clone)]
pub struct CallRawResult {
    pub gas_used: u64,
    pub block_number: u64,
    pub success: bool,
    pub trace: Option<SparsedTraceArena>,
    pub call_traces: Vec<CallTrace>, // New field for direct CallTrace format
    pub logs: Vec<Log>,
    pub exit_reason: InstructionResult,
    pub return_data: AlloyBytes,
    pub formatted_trace: Option<String>,
    pub result: Option<ExecutionResult>,
    pub state: Option<EvmState>,
    pub state_diff: Option<serde_json::Value>, // State diff from trace
}

impl From<CallTraceNode> for CallTrace {
    fn from(item: CallTraceNode) -> Self {
        let function_signature = match item.trace.kind {
            CallKind::Call | CallKind::StaticCall => {
                let first_4_bytes: Vec<u8> = item.trace.data.iter().take(4).cloned().collect();
                AlloyBytes::from_iter(first_4_bytes)
            }
            _ => AlloyBytes::from(vec![0]),
        };
        CallTrace {
            call_type: item.trace.kind,
            from: item.trace.caller,
            to: item.trace.address,
            value: format!("0x{:x}", item.trace.value), // ✅ Convert U256 to hex string
            function_signature: function_signature,
        }
    }
}

// Parse raw JSON trace data into CallTrace format
fn parse_alloy_traces(raw_trace: &serde_json::Value) -> Vec<CallTrace> {
    let mut traces = Vec::new();

    // Try to parse as raw trace_call response first (has "trace", "vmTrace", "stateDiff")
    if let Some(trace_array) = raw_trace.get("trace").and_then(|t| t.as_array()) {
        for (_i, trace_entry) in trace_array.iter().enumerate() {
            // Process each trace entry from raw JSON response

            if let Some(action) = trace_entry.get("action") {
                if let Some(call_action) = action.as_object() {
                    // Parse call type
                    let call_kind = match call_action.get("callType").and_then(|ct| ct.as_str()) {
                        Some("call") => CallKind::Call,
                        Some("staticcall") => CallKind::StaticCall,
                        Some("delegatecall") => CallKind::DelegateCall,
                        Some("callcode") => CallKind::CallCode,
                        _ => CallKind::Call, // Default
                    };

                    // Parse addresses and value
                    let from = call_action
                        .get("from")
                        .and_then(|f| f.as_str())
                        .and_then(|s| s.parse::<Address>().ok())
                        .unwrap_or_default();

                    let to = call_action
                        .get("to")
                        .and_then(|t| t.as_str())
                        .and_then(|s| s.parse::<Address>().ok())
                        .unwrap_or_default();

                    let value = call_action
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()) // ✅ Keep as hex string
                        .unwrap_or_else(|| "0x0".to_string());

                    // Value is now preserved as hex string from the raw trace response

                    // Parse function signature from input
                    let function_signature = call_action
                        .get("input")
                        .and_then(|inp| inp.as_str())
                        .and_then(|s| {
                            let hex_str = s.strip_prefix("0x").unwrap_or(s);
                            if hex_str.len() >= 8 {
                                // At least 4 bytes = 8 hex chars
                                // Parse hex using alloy hex decode
                                hex::decode(&hex_str[0..8]).ok().map(AlloyBytes::from)
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| AlloyBytes::from(vec![0u8; 4]));

                    let trace = CallTrace {
                        call_type: call_kind,
                        from,
                        to,
                        value,
                        function_signature,
                    };

                    // Added raw trace entry
                    traces.push(trace);
                }
            }
        }
    }
    // Fallback: try to parse as TraceResults format (for fallback trace_call)
    else if let Ok(trace_results) = serde_json::from_value::<TraceResults>(raw_trace.clone()) {
        for (_i, tx_trace) in trace_results.trace.iter().enumerate() {
            match &tx_trace.action {
                alloy::rpc::types::trace::parity::Action::Call(call_action) => {
                    let call_kind = match call_action.call_type {
                        alloy::rpc::types::trace::parity::CallType::Call => CallKind::Call,
                        alloy::rpc::types::trace::parity::CallType::StaticCall => {
                            CallKind::StaticCall
                        }
                        alloy::rpc::types::trace::parity::CallType::DelegateCall => {
                            CallKind::DelegateCall
                        }
                        alloy::rpc::types::trace::parity::CallType::CallCode => CallKind::CallCode,
                        alloy::rpc::types::trace::parity::CallType::None => CallKind::Call,
                        alloy::rpc::types::trace::parity::CallType::AuthCall => CallKind::Call,
                    };

                    let function_signature = if call_action.input.len() >= 4 {
                        AlloyBytes::from(call_action.input[0..4].to_vec())
                    } else {
                        AlloyBytes::from(vec![0u8; 4])
                    };

                    let trace = CallTrace {
                        call_type: call_kind,
                        from: call_action.from,
                        to: call_action.to,
                        value: format!("0x{:x}", call_action.value), // ✅ Convert U256 to hex string
                        function_signature,
                    };

                    traces.push(trace);
                }
                alloy::rpc::types::trace::parity::Action::Create(create_action) => {
                    let trace = CallTrace {
                        call_type: CallKind::Create,
                        from: create_action.from,
                        to: Address::ZERO,
                        value: format!("0x{:x}", create_action.value), // ✅ Convert U256 to hex string
                        function_signature: AlloyBytes::from(vec![0u8; 4]),
                    };

                    traces.push(trace);
                }
                _ => {}
            }
        }
    } else {
    }

    traces
}

// Create basic trace from transaction info - quick fix for tracing
fn create_basic_trace_from_tx(call: &CallRawRequest) -> Vec<CallTrace> {
    let function_signature = if let Some(ref data) = call.data {
        if data.len() >= 4 {
            AlloyBytes::from(data[0..4].to_vec())
        } else {
            AlloyBytes::from(vec![0u8; 4])
        }
    } else {
        AlloyBytes::from(vec![0u8; 4])
    };

    let trace = CallTrace {
        call_type: CallKind::Call,
        from: call.from,
        to: call.to,
        value: call
            .value
            .map(|v| format!("0x{:x}", v))
            .unwrap_or_else(|| "0x0".to_string()), // ✅ Convert to hex string
        function_signature,
    };

    vec![trace]
}

#[derive(Debug, Clone, PartialEq)]
pub struct StorageOverride {
    pub slots: HashMap<B256, U256>,
    pub diff: bool,
}

fn configure_evm(
    block: AnyRpcBlock,
    shared: SharedBackend,
) -> EthEvm<WrapDatabaseRef<SharedBackend>, NoOpInspector> {
    let block_env = BlockEnv {
        number: block.header.number(),
        beneficiary: block.header.beneficiary(),
        timestamp: block.header.timestamp(),
        gas_limit: block.header.gas_limit(),
        basefee: block.header.base_fee_per_gas().unwrap_or(0),
        prevrandao: block.header.mix_hash(),
        difficulty: block.header.difficulty(),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new(
            block.header.excess_blob_gas().unwrap_or_default(),
            true,
        )),
    };

    let context = EthEvmContext::new(
        WrapDatabaseRef(shared),
        revm_primitives::hardfork::SpecId::PRAGUE,
    )
    .with_block(block_env);

    let evm = RevmEvm::new(
        context,
        EthInstructions::default(),
        EthPrecompiles::default(),
    )
    .with_inspector(NoOpInspector);

    EthEvm::new(evm, true)
}

fn configure_tx_env(tx_req: TransactionRequest) -> TxEnv {
    let tx_env = TxEnv {
        caller: tx_req.from.unwrap(),
        kind: tx_req.kind().unwrap(),
        value: tx_req.value.unwrap(),
        gas_price: tx_req.gas_price.unwrap_or_default(),
        gas_limit: tx_req.gas.unwrap_or_default(),
        nonce: tx_req.nonce.unwrap_or_default(),
        data: tx_req.input.data.unwrap_or_default(),
        ..Default::default()
    };
    tx_env
}

pub struct Evm {
    // executor:  EthEvm<WrapDatabaseRef<SharedBackend>, NoOpInspector>,
    shared: SharedBackend,
    block: AnyRpcBlock,
    provider: FillProvider<
        JoinFill<
            Identity,
            JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
        >,
        alloy::providers::RootProvider<AnyNetwork>,
        AnyNetwork,
    >, // executor: Executor,
       // decoder: CallTraceDecoder,
       // etherscan_identifier: Option<EtherscanIdentifier>,
}

impl Evm {
    pub async fn new(
        fork_url: String,
        fork_block_number: Option<u64>,
        _gas_limit: u64,
        _etherscan_key: Option<String>,
    ) -> Self {
        // let anvil = Anvil::new().fork(fork_url).fork_block_number(fork_block_number.unwrap()).try_spawn().unwrap();
        // let provider = Provid erBuilder::new().network::<AnyNetwork>().connect_http(anvil.endpoint_url());
        let provider = ProviderBuilder::new()
            .network::<AnyNetwork>()
            .connect_http(fork_url.parse().unwrap());

        let block = provider
            .get_block(BlockId::number(fork_block_number.unwrap()))
            .await
            .unwrap()
            .unwrap();
        let meta = BlockchainDbMeta::default()
            .with_block(&block)
            .with_url(fork_url.as_str());
        let db = BlockchainDb::new(meta, foundry_config::Config::foundry_cache_dir());
        let shared = SharedBackend::spawn_backend(
            provider.clone(),
            db,
            Some(BlockId::number(fork_block_number.unwrap())),
        )
        .await;
        // let shared: SharedBackend = SharedBackend::spawn_backend(provider.clone(), db, Some(BlockId::Number(BlockNumberOrTag::Number(fork_block_number.unwrap())))).await;
        // let shared: SharedBackend = SharedBackend::spawn_backend(provider.clone(), db, Some(BlockId::number(fork_block_number.unwrap()))).await;
        // let evm: EthEvm<WrapDatabaseRef<SharedBackend>, NoOpInspector> = configure_evm(block.clone(), shared.clone());
        Evm {
            // executor: evm,
            shared,
            block,
            provider,
        }
    }

    pub async fn call_raw(&mut self, call: CallRawRequest) -> Result<CallRawResult, EvmError> {
        let total_start = Instant::now();

        // ⏱️ NONCE LOOKUP
        let nonce_start = Instant::now();
        let current_nonce = match self.shared.basic_ref(call.from) {
            Ok(Some(account)) => account.nonce,
            Ok(None) => {
                println!("⚠️  Account {:?} not found, using nonce 0", call.from);
                0
            }
            Err(e) => {
                println!("⚠️  Failed to get account {:?}: {}, using nonce 0", call.from, e);
                0
            }
        };
        let nonce_time = nonce_start.elapsed();

        // ⏱️ TRANSACTION REQUEST BUILD
        let tx_build_start = Instant::now();
        let tx_req = TransactionRequest::default()
            .with_from(call.from)
            .with_to(call.to)
            .with_value(call.value.unwrap_or_default())
            .with_input_kind(
                call.data.clone().unwrap_or_default(),
                TransactionInputKind::from_str("data").unwrap(),
            )
            .with_nonce(current_nonce)
            .with_gas_price(call.gas_price)
            .with_gas_limit(call.gas_limit);
        let with_other: WithOtherFields<TransactionRequest> = tx_req.clone().into();
        let tx_build_time = tx_build_start.elapsed();

        // ⏱️ TRACE DATA RETRIEVAL
        let trace_start = Instant::now();
        let trace_data = if call.format_trace {
            let hex_block = format!("0x{:x}", self.block.header.number);
            let params = serde_json::json!([
                with_other,
                ["trace", "stateDiff"],
                hex_block
            ]);

            match self
                .provider
                .client()
                .request::<_, serde_json::Value>("trace_call", params)
                .await
            {
                Ok(result) => Some(result),
                Err(_e) => {
                    match self.provider.trace_call(&with_other).await {
                        Ok(tracing) => Some(serde_json::to_value(tracing).unwrap_or_default()),
                        Err(_e2) => None,
                    }
                }
            }
        } else {
            None
        };
        let trace_retrieval_time = trace_start.elapsed();

        // ⏱️ EVM EXECUTOR CONFIGURATION
        let evm_config_start = Instant::now();
        let mut executor = configure_evm(self.block.clone(), self.shared.clone());
        let evm_config_time = evm_config_start.elapsed();

        // ⏱️ TRANSACTION EXECUTION
        let execution_start = Instant::now();
        let res = match executor.transact(configure_tx_env(tx_req.clone())) {
            Ok(result) => result,
            Err(e) => {
                println!("⚠️  Transaction execution failed: {}", e);
                println!("⚠️  Transaction details: from={:?}, to={:?}, value={:?}",
                    call.from, call.to, call.value);
                return Err(EvmError(eyre::eyre!("Failed to apply transaction: {}", e)));
            }
        };
        let execution_time = execution_start.elapsed();

        // ⏱️ TRACE PARSING
        let parse_start = Instant::now();
        let call_traces = if call.format_trace {
            if let Some(ref trace_data) = trace_data {
                parse_alloy_traces(trace_data)
            } else {
                create_basic_trace_from_tx(&call)
            }
        } else {
            Vec::new()
        };
        let parse_time = parse_start.elapsed();

        // ⏱️ STATE DIFF EXTRACTION
        let state_diff_start = Instant::now();
        let state_diff = if call.format_trace {
            trace_data
                .as_ref()
                .and_then(|data| data.get("stateDiff").cloned())
        } else {
            None
        };
        let state_diff_time = state_diff_start.elapsed();

        // ⏱️ FORMATTED TRACE BUILD
        let format_start = Instant::now();
        let formatted_trace = if call.format_trace && !call_traces.is_empty() {
            let mut output = String::new();
            for (i, trace) in call_traces.iter().enumerate() {
                output.push_str(&format!(
                    "{}. {:?} from {} to {} value {} sig 0x{}\n",
                    i + 1,
                    trace.call_type,
                    trace.from,
                    trace.to,
                    trace.value,
                    trace.function_signature
                ));
            }
            Some(output)
        } else {
            None
        };
        let format_time = format_start.elapsed();

        let total_time = total_start.elapsed();

        // 📊 EVM DETAILED PERFORMANCE BENCHMARK
        println!("⏱️  EVM DETAILED PERFORMANCE:");
        println!("  ├─ Nonce Lookup:     {:?}", nonce_time);
        println!("  ├─ TX Build:         {:?}", tx_build_time);
        println!("  ├─ Trace Retrieval:  {:?}", trace_retrieval_time);
        println!("  ├─ EVM Config:       {:?}", evm_config_time);
        println!("  ├─ TX Execution:     {:?}", execution_time);
        println!("  ├─ Trace Parsing:    {:?}", parse_time);
        println!("  ├─ State Diff:       {:?}", state_diff_time);
        println!("  ├─ Format Trace:     {:?}", format_time);
        println!("  └─ Total EVM:        {:?}", total_time);

        Ok(CallRawResult {
            gas_used: res.result.gas_used(),
            block_number: 123,
            success: res.result.is_success(),
            trace: None,
            call_traces,
            logs: res.result.logs().to_vec(),
            exit_reason: InstructionResult::Return,
            return_data: res.result.output().unwrap().clone(),
            formatted_trace,
            result: Some(res.result),
            state: Some(res.state),
            state_diff,
        })
    }

    pub fn override_account(
        &mut self,
        _address: Address,
        balance: Option<U256>,
        nonce: Option<u64>,
        code: Option<AlloyBytes>,
        storage: Option<StorageOverride>,
    ) -> Result<(), OverrideError> {
        // Try to override account via executor environment
        if let Some(nonce_val) = nonce {
            // Override the nonce in the EVM environment
            // This is a simulation, so we can set any nonce we want
            let mut executor = configure_evm(self.block.clone(), self.shared.clone());
            executor.ctx_mut().tx.nonce = nonce_val;
        }

        if let Some(_balance_val) = balance {
            // TODO: Implement balance override when needed
        }

        if let Some(_code_val) = code {
            // TODO: Implement code override when needed
        }

        if let Some(_storage_val) = storage {
            // TODO: Implement storage override when needed
        }

        Ok(())
    }

    // pub async fn call_raw_committing(
    //     &mut self,
    //     call: CallRawRequest,
    //     gas_limit: u64,
    // ) -> Result<CallRawResult, EvmError> {

    //     self.executor.set_gas_limit(gas_limit.into());
    //     self.set_access_list(call.access_list);

    //     // Auto-override sender's nonce to 0 for simulation
    //     self.override_account(call.from, None, Some(0), None, None)
    //         .map_err(|_| EvmError(eyre::eyre!("Failed to override account")))?;

    //     let res = self
    //         .executor
    //         .call_raw(
    //             call.from,
    //             call.to,
    //             call.data.unwrap_or_default(),
    //             call.value.unwrap_or_default(),
    //         )
    //         .map_err(|err| {
    //             dbg!(&err);
    //             EvmError(err)
    //         })?;

    //     let formatted_trace = if call.format_trace {
    //         let mut output = String::new();
    //         for trace in &mut res.traces.clone() {
    //             if let Some(identifier) = &mut self.etherscan_identifier {
    //                 self.decoder.identify(trace, identifier);
    //             }
    //             // self.decoder.decode(trace).await; // Method may not exist in current version
    //             output.push_str(format!("{:?}", trace).as_str());
    //         }
    //         Some(output)
    //     } else {
    //         None
    //     };

    //     Ok(CallRawResult {
    //         gas_used: res.gas_used,
    //         block_number: res.env.evm_env.block_env.number.to(),
    //         success: !res.reverted,
    //         trace: res.traces,
    //         logs: res.logs,
    //         exit_reason: res.exit_reason.unwrap_or(InstructionResult::Stop),
    //         return_data: WarpBytes::from(res.result.to_vec()),
    //         formatted_trace,
    //     })
    // }

    pub async fn set_block(&mut self, _number: u64) -> Result<(), EvmError> {
        // self.executor.env_mut().evm_env.block_env.number = U256::from(number).into();
        Ok(())
    }

    pub fn get_block(&self) -> U256 {
        U256::from(3)
        // self.executor.env().evm_env.block_env.number.into()
    }

    pub async fn set_block_timestamp(&mut self, _timestamp: u64) -> Result<(), EvmError> {
        // self.executor.env_mut().evm_env.block_env.timestamp = U256::from(timestamp).into();
        Ok(())
    }

    pub fn get_block_timestamp(&self) -> U256 {
        U256::from(2)
        // self.executor.env().evm_env.block_env.timestamp.into()
    }

    pub fn get_chain_id(&self) -> U256 {
        // U256::from(1)
        // U256::from(self.executor.env().evm_env.cfg_env.chain_id)
        // U256::from(self.block.header.chain_id())
        // U256::from(self.block.header.inner.)
        U256::from(1)
    }

    #[allow(dead_code)]
    fn set_access_list(&mut self, _access_list: Option<AccessList>) {
        // Access list setting needs to be implemented with current API
        // For now, skipping this functionality
        // self.executor.env_mut().tx.access_list = access_list.unwrap_or_default();
    }
}
