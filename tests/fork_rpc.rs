//! Exercise the real EVM and fork database against a local counting JSON-RPC server.
use alloy::{
    eips::BlockId,
    network::primitives::BlockTransactions,
    network::{AnyHeader, AnyRpcBlock, AnyRpcHeader},
    primitives::{Address, Bytes, B256, U256},
    rpc::types::Block,
};
use enso_temper::evm::{CallRawRequest, Evm};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use warp::Filter;

#[tokio::test(flavor = "multi_thread")]
async fn direct_fork_uses_standard_rpc_and_reuses_cached_account_and_storage() {
    let block = AnyRpcBlock::new(
        Block::new(
            AnyRpcHeader::from_sealed(
                AnyHeader {
                    number: 100,
                    gas_limit: 30_000_000,
                    timestamp: 1_700_000_000,
                    mix_hash: Some(B256::ZERO),
                    ..Default::default()
                }
                .seal(B256::ZERO),
            ),
            BlockTransactions::Full(vec![]),
        )
        .into(),
    );
    let target: Address = "0x1000000000000000000000000000000000000001"
        .parse()
        .unwrap();
    let sender: Address = "0x2000000000000000000000000000000000000002"
        .parse()
        .unwrap();
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let records = observed.clone();
    let route = warp::post()
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::body::json())
        .map(move |query: HashMap<String, String>, request: Value| {
            assert_eq!(query.get("client").map(String::as_str), Some("temper"));
            records.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "eth_chainId" => json!("0x1"),
                "eth_getBlockByNumber" => serde_json::to_value(&block).unwrap(),
                "eth_getBalance" => json!("0xde0b6b3a7640000"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getCode" => {
                    if request["params"][0]
                        .as_str()
                        .unwrap()
                        .parse::<Address>()
                        .unwrap()
                        == target
                    {
                        // SLOAD(0), store in memory, return the 32-byte value.
                        json!("0x60005460005260206000f3")
                    } else {
                        json!("0x")
                    }
                }
                "eth_getStorageAt" => json!(format!("0x{:064x}", 42)),
                "trace_call" => json!({"trace": [], "stateDiff": {}}),
                other => panic!("unexpected upstream RPC: {other}"),
            };
            warp::reply::json(&json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/1?client=temper", listener.local_addr().unwrap());
    let server = tokio::spawn(warp::serve(route).incoming(listener).run());
    let mut evm = Evm::new(url, None, 100_000, None).await;
    let request = || CallRawRequest {
        from: sender,
        to: target,
        value: Some(U256::ZERO),
        data: Some(Bytes::new()),
        access_list: None,
        format_trace: false,
        allow_insufficient_funds: false,
        include_state_diff: false,
        gas_limit: 100_000,
        gas_price: 0,
    };
    let first = evm.call_raw(request()).await.unwrap();
    assert!(first.success);
    assert_eq!(
        first.return_data.as_ref(),
        U256::from(42).to_be_bytes::<32>()
    );
    let cold_count = observed.lock().unwrap().len();
    let second = evm.call_raw(request()).await.unwrap();
    assert!(second.success);
    assert_eq!(first.return_data, second.return_data);
    let requests = observed.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        cold_count,
        "warm execution must not refetch cached state"
    );
    // Two initialization reads, at most three accounts, one storage slot.
    assert!(
        cold_count <= 2 + 3 * 3 + 1,
        "unexpected RPC amplification: {requests:?}"
    );
    let mut account_reads = HashMap::new();
    let mut storage_reads = 0;
    for req in &requests {
        match req["method"].as_str().unwrap() {
            "eth_getBalance" | "eth_getTransactionCount" | "eth_getCode" => {
                assert_eq!(req["params"][1], json!(BlockId::number(100)));
                *account_reads
                    .entry((req["method"].to_string(), req["params"][0].to_string()))
                    .or_insert(0) += 1;
            }
            "eth_getStorageAt" => {
                assert_eq!(req["params"][2], json!(BlockId::number(100)));
                storage_reads += 1;
            }
            _ => {}
        }
    }
    assert!(account_reads.values().all(|count| *count == 1));
    assert_eq!(storage_reads, 1);
    let mut traced_request = request();
    traced_request.include_state_diff = true;
    let traced = evm.call_raw(traced_request).await.unwrap();
    assert!(traced.success);
    assert_eq!(traced.return_data, first.return_data);
    assert_eq!(traced.state_diff, Some(json!({})));
    let traced_requests = observed.lock().unwrap().clone();
    assert_eq!(traced_requests.len(), cold_count + 1);
    assert_eq!(traced_requests.last().unwrap()["method"], "trace_call");
    assert_eq!(traced_requests.last().unwrap()["params"][2], "0x64");
    println!("cold RPC count: {cold_count}; warm additional RPC count: 0; state diff additional RPC count: 1");
    server.abort();
}
