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
                    base_fee_per_gas: Some(100),
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
                "eth_getBalance" => json!("0x2a"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getCode" => {
                    if request["params"][0]
                        .as_str()
                        .unwrap()
                        .parse::<Address>()
                        .unwrap()
                        == target
                    {
                        // Return SLOAD(0), GASPRICE, CALLVALUE and BASEFEE.
                        json!("0x6000546000523a602052346040524860605260806000f3")
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
    let mut evm = Evm::new(url.clone(), None, 100_000, None).await;
    let request = || CallRawRequest {
        from: sender,
        to: target,
        value: Some(U256::from(42)),
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
    let expected: Vec<u8> = [42u64, 0, 42, 100]
        .into_iter()
        .flat_map(|n| U256::from(n).to_be_bytes::<32>())
        .collect();
    assert_eq!(first.return_data.as_ref(), expected.as_slice());
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
    assert_eq!(
        traced_requests.last().unwrap()["params"][0]["gasPrice"],
        "0x0"
    );

    // Local execution keeps value funding checks, even for legacy bypass callers.
    for (options, status, expected_gas_price) in [
        (json!({}), 200, 0),
        (
            json!({"allowInsufficientFunds": false, "gasPrice": "0"}),
            200,
            0,
        ),
        (
            json!({"allowInsufficientFunds": true, "value": "43"}),
            500,
            0,
        ),
        (json!({"value": "43"}), 500, 0),
        (json!({"gasPrice": "20"}), 500, 0),
        (json!({"gasPrice": "invalid"}), 400, 0),
        (json!({"gasPrice": "-1"}), 400, 0),
        (json!({"gasPrice": "NaN"}), 400, 0),
        (
            json!({"gasPrice": "0.000000001", "stateOverrides": {
                sender.to_string(): {"balance": "100042"}
            }}),
            200,
            1,
        ),
        (
            json!({"value": "0", "stateOverrides": {
                sender.to_string(): {"balance": "0"}
            }}),
            200,
            0,
        ),
    ] {
        let config = enso_temper::config::Config {
            fork_url: Some(url.clone()),
            ..Default::default()
        };
        let route = warp::body::json()
            .and_then(move |request| enso_temper::simulation::simulate(request, config.clone()))
            .recover(enso_temper::errors::handle_rejection);
        let mut body = json!({"chainId":1,"from":sender,"to":target,"value":"42","gasLimit":100000,"includeStateDiff":false});
        body.as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let response = warp::test::request()
            .method("POST")
            .json(&body)
            .reply(&route)
            .await;
        assert_eq!(
            response.status(),
            status,
            "options={options}: {:?}",
            response.body()
        );
        let result: Value = serde_json::from_slice(response.body()).unwrap();
        if status == 200 {
            assert_eq!(result["success"], true);
            let value = body["value"].as_str().unwrap().parse::<u64>().unwrap();
            let output: Vec<u8> = [42, expected_gas_price, value, 100]
                .into_iter()
                .flat_map(|n| U256::from(n).to_be_bytes::<32>())
                .collect();
            assert_eq!(result["returnData"], json!(Bytes::from(output)));
        } else if status == 500 {
            assert_eq!(result["message"], "EVM_ERROR");
        }
    }
    server.abort();
}
