use super::call_result::{CallResult, ResultContext};
use ethers::abi::{Abi, Function, FunctionExt};
use ethers::types::U256;
use serde_json::{json, Value};
use std::time::Duration;
#[cfg(test)]
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_TRACE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct Request {
    pub context: ResultContext,
    pub function: Function,
    pub abi: Abi,
    pub rpc: String,
    pub call: Value,
}

pub fn prepare(
    contract: &str,
    address: &str,
    signature: &str,
    abi: &Abi,
    args: &str,
    value: &str,
    from: &str,
    workspace_epoch: u64,
    network_epoch: u64,
    rpc: String,
) -> Result<Request, String> {
    let address = address
        .parse::<ethers::types::Address>()
        .map_err(|_| "Select a deployment or attach an exact contract address.".to_string())?;
    let function = abi
        .functions()
        .find(|function| function.abi_signature() == signature)
        .cloned()
        .ok_or("The function's full ABI is missing or ambiguous; compile the contract again.")?;
    let values = split_arguments(args, function.inputs.len())?;
    let mut remaining = 4096;
    let tokens = function
        .inputs
        .iter()
        .zip(&values)
        .enumerate()
        .map(|(index, (param, value))| {
            super::deployment::parse_field(&param.kind, value, &mut remaining)
                .map_err(|error| format!("Argument {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = function
        .encode_input(&tokens)
        .map_err(|_| "Could not encode function arguments.".to_string())?;
    let wei = parse_wei(value)?;
    if !wei.is_zero() && function.state_mutability != ethers::abi::StateMutability::Payable {
        return Err("This function is not payable; value must be zero.".into());
    }
    let mut call = json!({"to":format!("{address:#x}"),"data":format!("0x{}",ethers::utils::hex::encode(data)),"value":format!("{wei:#x}"),"gas":"0x989680"});
    if !from.is_empty() {
        let from = from
            .parse::<ethers::types::Address>()
            .map_err(|_| "The selected public caller address is invalid.".to_string())?;
        call["from"] = json!(format!("{from:#x}"));
    }
    Ok(Request {
        context: ResultContext {
            contract: contract.into(),
            address: format!("{address:#x}"),
            signature: signature.into(),
            request: json!({"method":"eth_call","params":[call.clone(),"latest"]}),
            workspace_epoch,
            network_epoch,
            source: Value::Null,
        },
        function,
        abi: abi.clone(),
        rpc,
        call,
    })
}

fn split_arguments(raw: &str, count: usize) -> Result<Vec<String>, String> {
    if raw.len() > 64 * 1024 {
        return Err("Arguments exceed64KiB.".into());
    }
    if count == 0 {
        return if raw.trim().is_empty() || raw.trim() == "[]" {
            Ok(Vec::new())
        } else {
            Err("This function has no arguments.".into())
        };
    }
    if let Ok(values) = serde_json::from_str::<Vec<Value>>(raw) {
        if values.len() == count {
            return Ok(values
                .into_iter()
                .map(|value| match value {
                    Value::String(text) => text,
                    _ => value.to_string(),
                })
                .collect());
        }
    }
    let values = super::session::split_args(raw);
    if values.len() != count {
        return Err(format!("Expected {count} arguments. Use a positional JSON array for exact strings and nested values."));
    }
    Ok(values)
}

pub async fn simulate(mut request: Request) -> Result<CallResult, String> {
    validate_rpc_url(&request.rpc)?;
    let source = identity(&request.rpc).await?;
    let block = source["block"]["number"]
        .as_str()
        .ok_or("The node did not return a block number.")?
        .to_string();
    request.context.source = source.clone();
    request.context.request = json!({"method":"eth_call","params":[request.call,block]});
    let raw = rpc(
        &request.rpc,
        "eth_call",
        request.context.request["params"].clone(),
    )
    .await?;
    verify(&request.rpc, &source).await?;
    Ok(CallResult::from_call(
        request.context,
        &request.function,
        &request.abi,
        raw,
    ))
}

struct TransactionPlan {
    context: ResultContext,
    abi: Abi,
    rpc: String,
    to: Option<ethers::types::Address>,
    data: Vec<u8>,
    value: U256,
    exact_gas: Option<u64>,
    expected_source: Option<super::macro_runner::SourceIdentity>,
}

pub async fn send(
    request: Request,
    key: String,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
) -> Result<CallResult, String> {
    send_with_gas(request, key, None, owned).await
}

pub async fn send_with_gas(
    request: Request,
    key: String,
    exact_gas: Option<u64>,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
) -> Result<CallResult, String> {
    if matches!(
        request.function.state_mutability,
        ethers::abi::StateMutability::View | ethers::abi::StateMutability::Pure
    ) {
        return Err("Read-only functions use Simulate/Read, not a broadcast transaction.".into());
    }
    let address = request
        .context
        .address
        .parse::<ethers::types::Address>()
        .map_err(|_| "Invalid target address.")?;
    let text = request.call["data"].as_str().ok_or("Missing calldata.")?;
    let digits = text.strip_prefix("0x").ok_or("Invalid calldata.")?;
    let data = ethers::utils::hex::decode(digits).map_err(|_| "Invalid calldata.")?;
    let value = parse_quantity(&request.call["value"])?;
    if !value.is_zero()
        && request.function.state_mutability != ethers::abi::StateMutability::Payable
    {
        return Err("This function is not payable; value must be zero.".into());
    }
    submit_transaction(
        TransactionPlan {
            context: request.context,
            abi: request.abi,
            rpc: request.rpc,
            to: Some(address),
            data,
            value,
            exact_gas,
            expected_source: None,
        },
        key,
        owned,
    )
    .await
}

/// Deploys pre-encoded creation bytecode using only the owned local node.
pub async fn deploy(
    rpc: String,
    key: String,
    creation: Vec<u8>,
    value: String,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
) -> Result<CallResult, String> {
    deploy_with_gas(rpc, key, creation, value, None, owned).await
}

pub async fn deploy_with_gas(
    rpc: String,
    key: String,
    creation: Vec<u8>,
    value: String,
    exact_gas: Option<u64>,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
) -> Result<CallResult, String> {
    let value = parse_wei(&value)?;
    if creation.is_empty() {
        return Err("Contract creation bytecode is empty.".into());
    }
    if creation.len() > MAX_BYTES / 2 {
        return Err("Contract creation bytecode is too large.".into());
    }
    let context = ResultContext {
        contract: String::new(),
        address: String::new(),
        signature: "constructor".into(),
        request: Value::Null,
        workspace_epoch: 0,
        network_epoch: 0,
        source: Value::Null,
    };
    submit_transaction(
        TransactionPlan {
            context,
            abi: Abi::default(),
            rpc,
            to: None,
            data: creation,
            value,
            exact_gas,
            expected_source: None,
        },
        key,
        owned,
    )
    .await
}

pub async fn guarded_deploy(
    guard: &(dyn Fn() -> bool + Send + Sync),
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
    key: String,
    creation: &str,
    value: &str,
    exact_gas: u64,
) -> Result<CallResult, ()> {
    let digits = creation.strip_prefix("0x").ok_or(())?;
    let creation = ethers::utils::hex::decode(digits).map_err(|_| ())?;
    let value = parse_wei(value).map_err(|_| ())?;
    let context = ResultContext {
        contract: String::new(), address: String::new(), signature: "constructor".into(),
        request: Value::Null, workspace_epoch: 0, network_epoch: 0, source: Value::Null,
    };
    submit_transaction_guarded(TransactionPlan {
        context, abi: Abi::default(), rpc: "http://127.0.0.1:8545".into(), to: None,
        data: creation, value, exact_gas: Some(exact_gas), expected_source: None,
    }, key, owned, guard).await.map_err(|_| ())
}

pub async fn guarded_abi_send(
    guard: &(dyn Fn() -> bool + Send + Sync),
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
    key: String,
    request: Request,
    exact_gas: u64,
) -> Result<CallResult, ()> {
    if matches!(request.function.state_mutability, ethers::abi::StateMutability::View | ethers::abi::StateMutability::Pure) {
        return Err(());
    }
    let to = request.context.address.parse::<ethers::types::Address>().map_err(|_| ())?;
    let data = request.call["data"].as_str().and_then(|text| text.strip_prefix("0x")).ok_or(())?;
    let data = ethers::utils::hex::decode(data).map_err(|_| ())?;
    let value = parse_quantity(&request.call["value"]).map_err(|_| ())?;
    submit_transaction_guarded(TransactionPlan {
        context: request.context, abi: request.abi, rpc: request.rpc, to: Some(to), data, value,
        exact_gas: Some(exact_gas), expected_source: None,
    }, key, owned, guard).await.map_err(|_| ())
}

pub async fn guarded_raw_send(
    guard: &(dyn Fn() -> bool + Send + Sync),
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
    key: String,
    request: &super::macro_runner::RawSendRequest,
) -> Result<CallResult, ()> {
    let to = request.transaction.to.parse::<ethers::types::Address>().map_err(|_| ())?;
    let data = request.transaction.data.strip_prefix("0x").ok_or(())?;
    let data = ethers::utils::hex::decode(data).map_err(|_| ())?;
    let value = parse_wei(&request.transaction.value).map_err(|_| ())?;
    let context = ResultContext {
        contract: String::new(), address: format!("{to:#x}"), signature: "raw_send".into(),
        request: Value::Null, workspace_epoch: 0, network_epoch: 0, source: Value::Null,
    };
    submit_transaction_guarded(TransactionPlan {
        context, abi: Abi::default(), rpc: "http://127.0.0.1:8545".into(), to: Some(to), data,
        value, exact_gas: Some(request.exact_gas), expected_source: request.expected_source.clone(),
    }, key, owned, guard).await.map_err(|_| ())
}

fn parse_wei(text: &str) -> Result<U256, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(U256::zero());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Value must be an exact uint256 decimal wei amount.".into());
    }
    U256::from_dec_str(text)
        .map_err(|_| "Value must be an exact uint256 decimal wei amount.".into())
}

fn unsigned_transaction(
    plan: &TransactionPlan,
    from: ethers::types::Address,
) -> ethers::types::TransactionRequest {
    let transaction = ethers::types::TransactionRequest::new()
        .from(from)
        .data(ethers::types::Bytes::from(plan.data.clone()))
        .value(plan.value)
        .chain_id(31337u64);
    match plan.to {
        Some(to) => transaction.to(to),
        None => transaction,
    }
}

fn public_transaction(plan: &TransactionPlan, from: ethers::types::Address) -> Value {
    let mut transaction = json!({"from":format!("{from:#x}"),"data":format!("0x{}", ethers::utils::hex::encode(&plan.data)),"value":format!("{:#x}", plan.value)});
    if let Some(to) = plan.to {
        transaction["to"] = json!(format!("{to:#x}"));
    }
    transaction
}

fn mark_confirmed(context: &mut ResultContext, receipt: &Value, source: Value, creation: bool) {
    context.source = source;
    context.request["submissionState"] = json!("confirmed");
    if creation && receipt_status(receipt) == Some(true) {
        if let Some(address) = receipt["result"]["contractAddress"].as_str() {
            context.address = address.to_owned();
        }
    }
}

async fn submit_transaction(
    plan: TransactionPlan,
    key: String,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
) -> Result<CallResult, String> {
    submit_transaction_guarded(plan, key, owned, &|| true).await
}

async fn submit_transaction_guarded(
    mut plan: TransactionPlan,
    key: String,
    owned: std::sync::Arc<super::owned_node::OwnedNode>,
    guard: &(dyn Fn() -> bool + Send + Sync),
) -> Result<CallResult, String> {
    use ethers::signers::Signer;
    if !guard() {
        return Err("The macro execution context changed before signing. No transaction was submitted.".into());
    }
    if !owned.authorizes_loopback(8545) {
        return Err("Signing authority for the owned local node has expired.".into());
    }
    if plan.rpc != "http://127.0.0.1:8545" {
        return Err("Native signing is restricted to the owned local Anvil node.".into());
    }
    let chain = rpc(&plan.rpc, "eth_chainId", json!([])).await?;
    if parse_quantity(&chain["result"]).ok() != Some(U256::from(31337u64)) {
        return Err(
            "Native signing is restricted to the owned local Anvil node on chain 31337.".into(),
        );
    }
    let pre_submit_source = identity(&plan.rpc).await?;
    if pre_submit_source["chainId"] != "0x7a69" {
        return Err(
            "The node identity changed before signing. No transaction was submitted.".into(),
        );
    }
    let signer = key
        .parse::<ethers::signers::LocalWallet>()
        .map_err(|_| "The selected signer is invalid.".to_string())?
        .with_chain_id(31337u64);
    let from = signer.address();
    let mut transaction = unsigned_transaction(&plan, from);
    let mut public_transaction = public_transaction(&plan, from);
    let nonce = rpc(
        &plan.rpc,
        "eth_getTransactionCount",
        json!([format!("{from:#x}"), "pending"]),
    )
    .await?;
    let gas_price = rpc(&plan.rpc, "eth_gasPrice", json!([])).await?;
    let gas = match plan.exact_gas {
        Some(gas) if gas > 0 => U256::from(gas),
        Some(_) => return Err("Exact gas must be a positive u64 quantity.".into()),
        None => {
            let estimate = rpc(
                &plan.rpc,
                "eth_estimateGas",
                json!([public_transaction.clone()]),
            )
            .await?;
            preflight_quantity(&estimate, "eth_estimateGas", &plan.abi)?
        }
    };
    let nonce = preflight_quantity(&nonce, "eth_getTransactionCount", &plan.abi)?;
    let gas_price = preflight_quantity(&gas_price, "eth_gasPrice", &plan.abi)?;
    public_transaction["nonce"] = json!(format!("{nonce:#x}"));
    public_transaction["gasPrice"] = json!(format!("{gas_price:#x}"));
    public_transaction["gas"] = json!(format!("{gas:#x}"));
    public_transaction["chainId"] = json!("0x7a69");
    transaction = transaction.nonce(nonce).gas_price(gas_price).gas(gas);
    let typed: ethers::types::transaction::eip2718::TypedTransaction = transaction.into();
    if !guard() {
        return Err("The macro execution context changed before signing. No transaction was submitted.".into());
    }
    if !owned.authorizes_loopback(8545) {
        return Err("The owned node exited before signing. No transaction was submitted.".into());
    }
    let signature = signer
        .sign_transaction(&typed)
        .await
        .map_err(|_| "Local transaction signing failed.".to_string())?;
    let signed = typed.rlp_signed(&signature);
    if !guard() {
        return Err("The macro execution context changed before submission. No transaction was submitted.".into());
    }
    if !owned.authorizes_loopback(8545) {
        return Err(
            "The owned node exited before submission. No transaction was submitted.".into(),
        );
    }
    verify(&plan.rpc, &pre_submit_source).await.map_err(|_| {
        "The node identity changed before submission. No transaction was submitted."
    })?;
    if let Some(expected) = &plan.expected_source {
        let head = identity(&plan.rpc).await.map_err(|_| {
            "The selected sweep head could not be verified. No transaction was submitted."
        })?;
        if !matches_expected_source(&head, expected) {
            return Err("The selected sweep head changed. No transaction was submitted.".into());
        }
    }
    if !guard() {
        return Err("The macro execution context changed before submission. No transaction was submitted.".into());
    }
    if !owned.authorizes_loopback(8545) {
        return Err(
            "The owned node exited before submission. No transaction was submitted.".into(),
        );
    }
    let hash = format!(
        "0x{}",
        ethers::utils::hex::encode(ethers::utils::keccak256(&signed))
    );
    plan.context.request = json!({"method":"eth_sendRawTransaction","params":["[signed payload omitted]"],"transaction":public_transaction,"transactionHash":hash,"submissionState":"unknown"});
    let submitted = match rpc(&plan.rpc, "eth_sendRawTransaction", json!([format!("0x{}", ethers::utils::hex::encode(signed))])).await {
        Ok(response) => response,
        Err(_) => return Ok(receipt_unavailable(plan, None, "Submission outcome is unknown; the transaction may have been accepted. Check the transaction hash before retrying.")),
    };
    let accepted_hash = submitted
        .get("result")
        .and_then(Value::as_str)
        .filter(|value| valid_hash(value));
    if !accepted_hash.is_some_and(|value| value.eq_ignore_ascii_case(&hash)) {
        return Ok(receipt_unavailable(plan, Some(submitted), "Submission was not confirmed with the expected transaction hash. Check the transaction before retrying."));
    }
    plan.context.request["submissionResponse"] = submitted;
    let mut last_receipt = None;
    for _ in 0..60 {
        let receipt = match rpc(&plan.rpc, "eth_getTransactionReceipt", json!([hash])).await {
            Ok(response) => response,
            Err(_) => return Ok(receipt_unavailable(plan, last_receipt, "The transaction was submitted, but its receipt could not be retrieved. Check status before retrying.")),
        };
        if receipt.get("error").is_some() {
            return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but receipt retrieval failed. Check status before retrying."));
        }
        if !receipt["result"].is_null() {
            let block = match receipt_block(&receipt, &hash) {
                Ok(block) => block.to_owned(),
                Err(reason) => return Ok(receipt_unavailable(plan, Some(receipt), reason)),
            };
            let status = match receipt_status(&receipt) {
                Some(status) => status,
                None => return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but its receipt status is missing or invalid. Check status before retrying.")),
            };
            if plan.to.is_none() && status && !valid_created_address(&receipt) {
                return Ok(receipt_unavailable(plan, Some(receipt), "The deployment was submitted, but the successful receipt has no valid contract address. Check status before retrying."));
            }
            let source = match identity_at(&plan.rpc, &block).await {
                Ok(source) => source,
                Err(_) => return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but its receipt block could not be verified. Check status before retrying.")),
            };
            if source["chainId"] != pre_submit_source["chainId"]
                || source["genesisHash"] != pre_submit_source["genesisHash"]
            {
                return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but the node identity changed before receipt verification. Check status before retrying."));
            }
            if !receipt_matches_source(&receipt, &hash, &source) {
                return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but its receipt block hash no longer matches the node. Check status before retrying."));
            }
            if verify(&plan.rpc, &source).await.is_err() {
                return Ok(receipt_unavailable(plan, Some(receipt), "The transaction was submitted, but its receipt block changed or became unavailable. Check status before retrying."));
            }
            mark_confirmed(&mut plan.context, &receipt, source, plan.to.is_none());
            return Ok(CallResult::from_receipt(plan.context, &plan.abi, receipt));
        }
        last_receipt = Some(receipt);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(receipt_unavailable(
        plan,
        last_receipt,
        "The transaction was submitted, but its receipt is still unavailable. Do not resend.",
    ))
}

fn receipt_status(receipt: &Value) -> Option<bool> {
    match parse_quantity(&receipt["result"]["status"]).ok()? {
        status if status == U256::one() => Some(true),
        status if status.is_zero() => Some(false),
        _ => None,
    }
}

fn valid_created_address(receipt: &Value) -> bool {
    let Some(text) = receipt["result"]["contractAddress"].as_str() else {
        return false;
    };
    let Some(digits) = text.strip_prefix("0x") else {
        return false;
    };
    digits.len() == 40
        && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        && digits.bytes().any(|byte| byte != b'0')
}

fn receipt_unavailable(mut plan: TransactionPlan, raw: Option<Value>, reason: &str) -> CallResult {
    let hash = plan.context.request["transactionHash"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    plan.context.request["submissionState"] = json!("unknown");
    let mut result = CallResult::from_receipt(plan.context, &plan.abi, raw.unwrap_or(Value::Null));
    result.simple = format!("{reason}\nTransaction hash: {hash}");
    result
}

fn receipt_block<'a>(receipt: &'a Value, expected_hash: &str) -> Result<&'a str, &'static str> {
    let value = receipt.get("result").and_then(Value::as_object)
        .ok_or("The transaction was submitted, but its receipt is incomplete. Check status before retrying.")?;
    let transaction_hash = value.get("transactionHash").and_then(Value::as_str).filter(|hash| valid_hash(hash))
        .ok_or("The transaction was submitted, but the receipt transaction hash is missing or invalid. Check status before retrying.")?;
    if !transaction_hash.eq_ignore_ascii_case(expected_hash) {
        return Err("The transaction was submitted, but the receipt belongs to a different transaction. Check status before retrying.");
    }
    let block = value.get("blockNumber").and_then(Value::as_str)
        .filter(|number| parse_quantity_text(number).is_ok())
        .ok_or("The transaction was submitted, but the receipt block number is missing or invalid. Check status before retrying.")?;
    value.get("blockHash").and_then(Value::as_str).filter(|hash| valid_hash(hash))
        .ok_or("The transaction was submitted, but the receipt block hash is missing or invalid. Check status before retrying.")?;
    Ok(block)
}

fn receipt_matches_source(receipt: &Value, hash: &str, source: &Value) -> bool {
    if receipt.get("error").is_some_and(|error| !error.is_null()) {
        return false;
    }
    let Ok(block) = receipt_block(receipt, hash) else {
        return false;
    };
    let Some(source_block) = source["block"]["number"].as_str() else {
        return false;
    };
    let Ok(source_number) = parse_quantity_text(source_block) else {
        return false;
    };
    if parse_quantity_text(block).ok() != Some(source_number) {
        return false;
    }
    let Some(expected_hash) = source["block"]["hash"]
        .as_str()
        .filter(|hash| valid_hash(hash))
    else {
        return false;
    };
    receipt["result"]["blockHash"]
        .as_str()
        .is_some_and(|hash| hash.eq_ignore_ascii_case(expected_hash))
}

/// Read every slot the layout needs at one block number, base slots first and
/// then dynamic array elements. Returns the words and the block read.
pub async fn storage_words(
    rpc_url: &str,
    address: &str,
    layout: &super::storage_layout::Layout,
) -> Result<(std::collections::HashMap<U256, ethers::types::H256>, u64), String> {
    validate_rpc_url(rpc_url)?;
    use super::chain_read::{query, BlockRef, ReadRequest};
    // The first read pins the block; every later read uses that same number.
    let mut number = None;
    let mut words = std::collections::HashMap::new();
    for pass in 0..2 {
        let slots = if pass == 0 {
            layout.base_slots()
        } else {
            let read = |slot: U256| words.get(&slot).copied();
            layout.array_slots(&read)
        };
        for slot in slots {
            if words.contains_key(&slot) {
                continue;
            }
            let block = number.map(BlockRef::Number).unwrap_or(BlockRef::Latest);
            let result = query(rpc_url, ReadRequest::Storage { address: address.into(), slot: format!("{slot:#x}"), block })
                .await
                .map_err(|error| error.to_string())?;
            if number.is_none() {
                number = result["block"]["number"].as_str().and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok());
            }
            let text = result["data"]["value"].as_str().ok_or("The node returned no storage value.")?;
            let bytes = ethers::utils::hex::decode(text.trim_start_matches("0x")).map_err(|_| "Invalid storage word.")?;
            let mut word = [0u8; 32];
            let take = bytes.len().min(32);
            word[32 - take..].copy_from_slice(&bytes[bytes.len() - take..]);
            words.insert(slot, ethers::types::H256::from(word));
        }
    }
    Ok((words, number.unwrap_or(0)))
}

pub async fn storage(rpc_url: &str, address: &str, slot: &str) -> Result<(String, Value), String> {
    let result = super::chain_read::query(
        rpc_url,
        super::chain_read::ReadRequest::Storage {
            address: address.into(),
            slot: if slot.trim().is_empty() {
                "0".into()
            } else {
                slot.into()
            },
            block: super::chain_read::BlockRef::Latest,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let text = |value: &Value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
    let word = text(&result["data"]["value"]);
    let decimal = U256::from_str_radix(word.trim_start_matches("0x"), 16).map(|value| value.to_string()).unwrap_or_default();
    let block = text(&result["block"]["number"]);
    let block = u64::from_str_radix(block.trim_start_matches("0x"), 16).map(|number| number.to_string()).unwrap_or(block);
    Ok((
        format!(
            "Slot     {}\nWord     {word}\nAs uint  {decimal}\nBlock    {block} ({})",
            text(&result["data"]["slot"]),
            text(&result["block"]["hash"]),
        ),
        result,
    ))
}

pub async fn trace(rpc_url: &str, result: &CallResult) -> Result<String, String> {
    verify(rpc_url, &result.context.source).await?;
    let transaction_hash = result
        .context
        .request
        .get("transactionHash")
        .and_then(Value::as_str);
    if transaction_hash.is_some_and(|hash| !valid_hash(hash)) {
        return Err("Trace transaction hash is invalid.".into());
    }
    if let Some(hash) = transaction_hash {
        if !receipt_matches_source(&result.raw, hash, &result.context.source) {
            return Err(
                "Trace requires the original receipt at its pinned transaction and block.".into(),
            );
        }
    }
    validate_rpc_url(rpc_url)?;
    if let Some(hash) = transaction_hash {
        let current_receipt = rpc(rpc_url, "eth_getTransactionReceipt", json!([hash]))
            .await
            .map_err(|_| "Trace receipt could not be verified against the pinned block.")?;
        if !receipt_matches_source(&current_receipt, hash, &result.context.source) {
            return Err("Trace receipt no longer matches the pinned transaction and block.".into());
        }
    }
    let options = json!({"tracer":"callTracer","timeout":"10s","tracerConfig":{"withLog":true}});
    let (method, params) = match transaction_hash {
        Some(hash) => ("debug_traceTransaction", json!([hash, options])),
        None => {
            let block = result.context.source["block"]["number"].as_str().ok_or("Missing pinned trace block.")?;
            parse_quantity_text(block)?;
            ("debug_traceCall", json!([result.context.request["params"][0], block, options]))
        }
    };
    let response = rpc(rpc_url, method, params).await?;
    if response.get("error").is_some() {
        return Err("The selected node does not support callTracer for this exact context. No latest fallback, external signature lookup or broadcast was attempted.".into());
    }
    let text = super::call_trace::render(&response["result"], &result.abi, &result.context.address)?;
    verify(rpc_url, &result.context.source).await?;
    if let Some(hash) = transaction_hash {
        let current_receipt = rpc(rpc_url, "eth_getTransactionReceipt", json!([hash]))
            .await
            .map_err(|_| "Trace receipt could not be verified against the pinned block.")?;
        if !receipt_matches_source(&current_receipt, hash, &result.context.source) {
            return Err("Trace receipt no longer matches the pinned transaction and block.".into());
        }
    }
    Ok(format!(
        "Read-only local {} trace at block {}. Not verified debugger replay.\n\n{}",
        if transaction_hash.is_some() {
            "transaction"
        } else {
            "call"
        },
        result.context.source["block"]["number"],
        text
    ))
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum TraceProcessError {
    Timeout,
    TooLarge,
    Exit,
    Io,
}

#[cfg(test)]
async fn read_trace_pipe<R: AsyncRead + Unpin>(
    mut pipe: R,
    stdout: bool,
    used: &std::sync::atomic::AtomicUsize,
    limit: usize,
) -> Result<Vec<u8>, TraceProcessError> {
    use std::sync::atomic::Ordering;
    let mut captured = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = pipe
            .read(&mut chunk)
            .await
            .map_err(|_| TraceProcessError::Io)?;
        if count == 0 {
            return Ok(captured);
        }
        if used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                total.checked_add(count).filter(|sum| *sum <= limit)
            })
            .is_err()
        {
            return Err(TraceProcessError::TooLarge);
        }
        if stdout {
            captured.extend_from_slice(&chunk[..count]);
        }
    }
}

#[cfg(test)]
async fn collect_trace(
    child: &mut tokio::process::Child,
    deadline: Duration,
    limit: usize,
) -> Result<Vec<u8>, TraceProcessError> {
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        terminate_child(child).await?;
        return Err(TraceProcessError::Io);
    };
    let used = std::sync::atomic::AtomicUsize::new(0);
    let (result, exited) = {
        let work = async {
            let stdout_read = read_trace_pipe(stdout, true, &used, limit);
            let stderr_read = read_trace_pipe(stderr, false, &used, limit);
            let (stdout, _) = tokio::try_join!(stdout_read, stderr_read)?;
            let status = child.wait().await.map_err(|_| TraceProcessError::Io)?;
            Ok::<_, TraceProcessError>((stdout, status))
        };
        match tokio::time::timeout(deadline, work).await {
            Ok(Ok((stdout, status))) if status.success() => (Ok(stdout), true),
            Ok(Ok((_stdout, _status))) => (Err(TraceProcessError::Exit), true),
            Ok(Err(error)) => (Err(error), false),
            Err(_) => (Err(TraceProcessError::Timeout), false),
        }
    };
    if !exited {
        terminate_child(child).await?;
    }
    result
}

#[cfg(test)]
async fn terminate_child(child: &mut tokio::process::Child) -> Result<(), TraceProcessError> {
    match child.try_wait().map_err(|_| TraceProcessError::Io)? {
        Some(_) => Ok(()),
        None => {
            if child.start_kill().is_err() {
                return match child.try_wait() {
                    Ok(Some(_)) => Ok(()),
                    _ => Err(TraceProcessError::Io),
                };
            }
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(_)) => Ok(()),
                _ => Err(TraceProcessError::Io),
            }
        }
    }
}

async fn identity(url: &str) -> Result<Value, String> {
    identity_at(url, "latest").await
}
async fn identity_at(url: &str, block: &str) -> Result<Value, String> {
    if block != "latest" {
        parse_quantity_text(block)?;
    }
    let chain = rpc(url, "eth_chainId", json!([])).await?;
    let genesis = rpc(url, "eth_getBlockByNumber", json!(["0x0", false])).await?;
    let header = rpc(url, "eth_getBlockByNumber", json!([block, false])).await?;
    identity_from_responses(block, &chain, &genesis, &header)
}

fn identity_from_responses(
    block: &str,
    chain: &Value,
    genesis: &Value,
    header: &Value,
) -> Result<Value, String> {
    if [chain, genesis, header]
        .iter()
        .any(|response| response.get("error").is_some_and(|error| !error.is_null()))
    {
        return Err("The node returned an error while resolving block identity.".into());
    }
    let chain_id =
        parse_quantity(&chain["result"]).map_err(|_| "The node returned an invalid chain ID.")?;
    let genesis_result = genesis
        .get("result")
        .and_then(Value::as_object)
        .ok_or("The genesis block is unavailable.")?;
    if parse_quantity(genesis_result.get("number").unwrap_or(&Value::Null)).ok()
        != Some(U256::zero())
    {
        return Err("The node returned an invalid genesis block number.".into());
    }
    let genesis_hash = genesis_result
        .get("hash")
        .and_then(Value::as_str)
        .filter(|hash| valid_hash(hash))
        .ok_or("The node returned an invalid genesis hash.")?;
    let header_result = header
        .get("result")
        .and_then(Value::as_object)
        .ok_or("The requested block is unavailable.")?;
    let number = parse_quantity(header_result.get("number").unwrap_or(&Value::Null))
        .map_err(|_| "The node returned an invalid block number.")?;
    if block != "latest" && number != parse_quantity_text(block)? {
        return Err("The node returned a different block than requested.".into());
    }
    let hash = header_result
        .get("hash")
        .and_then(Value::as_str)
        .filter(|hash| valid_hash(hash))
        .ok_or("The node returned an invalid block hash.")?;
    Ok(
        json!({"chainId":format!("{chain_id:#x}"),"genesisHash":genesis_hash.to_ascii_lowercase(),"block":{"number":format!("{number:#x}"),"hash":hash.to_ascii_lowercase()}}),
    )
}

async fn verify(url: &str, source: &Value) -> Result<(), String> {
    let block = source["block"]["number"]
        .as_str()
        .ok_or("Missing pinned source context.")?;
    parse_quantity_text(block).map_err(|_| "Invalid pinned source block.")?;
    if identity_at(url, block).await? != *source {
        return Err("The network or pinned block changed. Result/trace was not accepted.".into());
    }
    Ok(())
}

fn matches_expected_source(head: &Value, expected: &super::macro_runner::SourceIdentity) -> bool {
    head["chainId"] == expected.chain_id
        && head["genesisHash"] == expected.genesis_hash
        && head["block"]["number"] == format!("{:#x}", expected.block_number)
        && head["block"]["hash"] == expected.block_hash
}

fn valid_hash(text: &str) -> bool {
    text.strip_prefix("0x").is_some_and(|digits| {
        digits.len() == 64 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}
fn parse_quantity_text(text: &str) -> Result<U256, String> {
    let digits = text.strip_prefix("0x").ok_or("Invalid RPC quantity")?;
    if digits.is_empty()
        || digits.len() > 64
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Invalid RPC quantity".into());
    }
    U256::from_str_radix(digits, 16).map_err(|_| "Invalid RPC quantity".into())
}
/// Start of the error for a call that reverted in the gas estimate.
pub const REVERTED_BEFORE_SENDING: &str = "Reverted before sending";

/// Quantity from a pre-send RPC response. An `error` there means nothing was
/// signed; a gas estimate error is the call reverting, so name its reason.
fn preflight_quantity(response: &Value, method: &str, abi: &Abi) -> Result<U256, String> {
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        if method == "eth_estimateGas" {
            let reason = super::call_result::revert_reason(error, abi)
                .unwrap_or_else(|| "the node returned no revert data (a bare revert, a require without a message, or out of gas)".into());
            return Err(format!("{REVERTED_BEFORE_SENDING}: {reason}\nNothing was broadcast. Debug this call steps to the failing line."));
        }
        let code = error.get("code").and_then(Value::as_i64).unwrap_or_default();
        return Err(format!("{method} failed (code {code}). Nothing was broadcast."));
    }
    parse_quantity(&response["result"])
}

fn parse_quantity(value: &Value) -> Result<U256, String> {
    parse_quantity_text(value.as_str().ok_or("Missing RPC quantity")?)
}
fn validate_rpc_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid RPC endpoint.")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("RPC endpoint must be HTTP without userinfo, query, or fragment.".into());
    }
    Ok(())
}

async fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    validate_rpc_url(url)?;
    let allowed = [
        "eth_chainId",
        "eth_getBlockByNumber",
        "eth_call",
        "eth_getTransactionCount",
        "eth_gasPrice",
        "eth_estimateGas",
        "eth_sendRawTransaction",
        "eth_getTransactionReceipt",
        "debug_traceCall",
        "debug_traceTransaction",
    ];
    if !allowed.contains(&method) {
        return Err("Unsupported contract operation.".into());
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create local RPC client")?;
    let mut response = http
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|_| "Contract RPC request failed or timed out.".to_string())?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read contract RPC response")?
    {
        let limit = if method.starts_with("debug_trace") { MAX_TRACE_BYTES } else { MAX_BYTES };
        if bytes.len() + chunk.len() > limit {
            return Err(if method.starts_with("debug_trace") { "Trace response exceeds 1 MiB." } else { "Contract RPC response exceeds 8 MiB." }.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(format!("Contract RPC returned HTTP{status}."));
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Contract RPC returned invalid JSON.")?;
    validate_and_redact_rpc(value, method)
}

fn validate_and_redact_rpc(mut value: Value, method: &str) -> Result<Value, String> {
    let object = value
        .as_object_mut()
        .ok_or("Contract RPC response must be an object.")?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(1)
    {
        return Err("Contract RPC returned an invalid version or response ID.".into());
    }
    if object.contains_key("result") == object.contains_key("error") {
        return Err("Contract RPC must return exactly one of result or error.".into());
    }
    let removed_reserved = object.remove("chasmRedaction").is_some();
    if let Some(error) = object.get_mut("error") {
        let error = error
            .as_object_mut()
            .ok_or("Contract RPC returned an invalid error.")?;
        if error.get("code").and_then(Value::as_i64).is_none()
            || !error.get("message").is_some_and(Value::is_string)
        {
            return Err("Contract RPC returned an invalid error code or message.".into());
        }
        error.insert("message".into(), json!("[provider message redacted]"));
        let safe_data = if method == "eth_sendRawTransaction" {
            None
        } else {
            error.get("data").and_then(revert_data).map(str::to_owned)
        };
        error.retain(|key, _| key == "code" || key == "message");
        if let Some(data) = safe_data {
            error.insert("data".into(), json!(data));
        }
        object.insert("chasmRedaction".into(), json!({"applied":true,"reason":"provider message and untrusted error fields omitted; only ABI-shaped hex data retained"}));
        object
            .retain(|key, _| matches!(key.as_str(), "jsonrpc" | "id" | "error" | "chasmRedaction"));
    } else if method == "eth_sendRawTransaction" {
        let valid = object
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(valid_hash);
        let extra = object.len() > 3 || removed_reserved;
        if !valid {
            object.insert(
                "result".into(),
                json!("[invalid submission result redacted]"),
            );
        }
        object.retain(|key, _| matches!(key.as_str(), "jsonrpc" | "id" | "result"));
        if !valid || extra {
            object.insert("chasmRedaction".into(), json!({"applied":true,"reason":"untrusted submission result or extra fields omitted"}));
        }
    } else if object.len() > 3 || removed_reserved {
        object.retain(|key, _| matches!(key.as_str(), "jsonrpc" | "id" | "result"));
        object.insert(
            "chasmRedaction".into(),
            json!({"applied":true,"reason":"untrusted extra response fields omitted"}),
        );
    }
    Ok(value)
}

fn revert_data(value: &Value) -> Option<&str> {
    fn walk<'a>(value: &'a Value, depth: usize, remaining: &mut usize) -> Option<&'a str> {
        if depth > 8 || *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        if let Some(text) = value.as_str() {
            let digits = text.strip_prefix("0x")?;
            return (digits.len() >= 8
                && digits.len() <= 512 * 1024
                && digits.len() % 2 == 0
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(text);
        }
        let object = value.as_object()?;
        for key in ["data", "return", "result", "originalError", "error"] {
            if let Some(found) = object
                .get(key)
                .and_then(|child| walk(child, depth + 1, remaining))
            {
                return Some(found);
            }
        }
        for (key, child) in object {
            if key.starts_with("0x") && child.is_object() {
                if let Some(found) = walk(child, depth + 1, remaining) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(value, 0, &mut 256)
}

#[cfg(test)]
#[path = "contract_service_trace_tests.rs"]
mod trace_transport_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argument_split_preserves_json_string_whitespace() {
        assert_eq!(
            split_arguments("[\" x \",42]", 2).unwrap(),
            vec![" x ", "42"]
        );
        assert!(split_arguments("1 2", 1).is_err());
    }

    #[test]
    fn identity_requires_exact_quantities_and_real_hashes() {
        let hash = format!("0x{}", "12".repeat(32));
        let chain = json!({"result":"0x7a69"});
        let genesis = json!({"result":{"number":"0x0","hash":hash}});
        let header = json!({"result":{"number":"0x10","hash":hash}});
        let source = identity_from_responses("0x10", &chain, &genesis, &header).unwrap();
        assert_eq!(source["block"]["number"], "0x10");
        assert!(identity_from_responses("0x11", &chain, &genesis, &header).is_err());
        assert!(identity_from_responses("0x010", &chain, &genesis, &header).is_err());
        for bad in [
            json!({"result":"0x"}),
            json!({"result":"0x01"}),
            json!({"result":null}),
            json!({"error":{"code":-1}}),
        ] {
            assert!(identity_from_responses("0x10", &bad, &genesis, &header).is_err());
        }
        assert!(identity_from_responses(
            "0x10",
            &chain,
            &json!({"result":{"number":"0x1","hash":hash}}),
            &header
        )
        .is_err());
        assert!(identity_from_responses(
            "0x10",
            &chain,
            &genesis,
            &json!({"result":{"number":"0x10","hash":"0x1234"}})
        )
        .is_err());
        assert!(identity_from_responses(
            "0x10",
            &chain,
            &genesis,
            &json!({"result":{"number":"0x010","hash":hash}})
        )
        .is_err());
    }

    #[test]
    fn receipt_requires_mined_block_and_matching_transaction() {
        let tx = format!("0x{}", "34".repeat(32));
        let block = format!("0x{}", "56".repeat(32));
        let receipt =
            json!({"result":{"transactionHash":tx,"blockNumber":"0x10","blockHash":block}});
        assert_eq!(receipt_block(&receipt, &tx), Ok("0x10"));
        assert!(receipt_matches_source(
            &receipt,
            &tx,
            &json!({"block":{"number":"0x10","hash":block}})
        ));
        assert!(!receipt_matches_source(
            &receipt,
            &tx,
            &json!({"block":{"number":"0x11","hash":block}})
        ));
        assert!(!receipt_matches_source(
            &receipt,
            &tx,
            &json!({"block":{"number":"0x10","hash":tx}})
        ));
        for bad in [
            json!({"result":{"transactionHash":tx,"blockHash":block}}),
            json!({"result":{"transactionHash":tx,"blockNumber":"0x010","blockHash":block}}),
            json!({"result":{"transactionHash":tx,"blockNumber":"0x10","blockHash":null}}),
            json!({"result":{"transactionHash":block,"blockNumber":"0x10","blockHash":block}}),
            json!({"result":null}),
        ] {
            assert!(receipt_block(&bad, &tx).is_err());
        }
    }

    #[test]
    fn ambiguous_submission_retains_hash_without_signed_payload() {
        let abi: Abi = serde_json::from_value(json!([{"type":"function","name":"set","inputs":[],"outputs":[],"stateMutability":"nonpayable"}])).unwrap();
        let hash = format!("0x{}", "78".repeat(32));
        let request = TransactionPlan {
            context: ResultContext {
                contract: "Test".into(),
                address: format!("0x{}", "11".repeat(20)),
                signature: "set()".into(),
                request: json!({"method":"eth_sendRawTransaction","params":["[signed payload omitted]"],"transactionHash":hash}),
                workspace_epoch: 1,
                network_epoch: 2,
                source: Value::Null,
            },
            abi,
            rpc: "http://127.0.0.1:8545".into(),
            to: None,
            data: vec![],
            value: U256::zero(),
            exact_gas: None,
            expected_source: None,
        };
        let result = receipt_unavailable(request, None, "Submission outcome is unknown.");
        assert!(result.simple.contains(&hash));
        assert_eq!(result.context.request["transactionHash"], hash);
        assert_eq!(result.context.request["submissionState"], "unknown");
        assert_eq!(result.raw, Value::Null);
        assert!(!result.context.request.to_string().contains("private key"));
    }

    #[test]
    fn deployment_transaction_has_no_to_and_exact_value() {
        let value =
            "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(parse_wei(value).unwrap(), U256::MAX);
        assert!(parse_wei("1e3").is_err());
        assert!(parse_wei("-1").is_err());
        assert!(parse_wei(&format!("{value}0")).is_err());
        let plan = TransactionPlan {
            context: ResultContext {
                contract: String::new(),
                address: String::new(),
                signature: "constructor".into(),
                request: Value::Null,
                workspace_epoch: 0,
                network_epoch: 0,
                source: Value::Null,
            },
            abi: Abi::default(),
            rpc: String::new(),
            to: None,
            data: vec![0x60, 0x00],
            value: U256::MAX,
            exact_gas: Some(250_000),
            expected_source: None,
        };
        let sender = ethers::types::Address::from_low_u64_be(1);
        let unsigned = unsigned_transaction(&plan, sender);
        assert!(unsigned.to.is_none());
        assert_eq!(unsigned.data.unwrap().as_ref(), &[0x60, 0x00]);
        assert_eq!(unsigned.value, Some(U256::MAX));
        let public = public_transaction(&plan, sender);
        assert!(public.get("to").is_none());
        assert_eq!(public["data"], "0x6000");
    }

    #[test]
    fn pinned_sweep_head_must_match_at_final_submission() {
        let expected = super::super::macro_runner::SourceIdentity {
            chain_id: "0x7a69".into(),
            genesis_hash: format!("0x{}", "11".repeat(32)),
            block_number: 4,
            block_hash: format!("0x{}", "22".repeat(32)),
        };
        let mut head = json!({"chainId":expected.chain_id,"genesisHash":expected.genesis_hash,"block":{"number":"0x4","hash":expected.block_hash}});
        assert!(matches_expected_source(&head, &expected));
        head["block"]["number"] = json!("0x5");
        assert!(!matches_expected_source(&head, &expected));
        head["block"]["number"] = json!("0x4");
        head["genesisHash"] = json!(format!("0x{}", "33".repeat(32)));
        assert!(!matches_expected_source(&head, &expected));
    }

    #[test]
    fn creation_receipt_requires_success_address_but_revert_does_not() {
        let valid =
            json!({"result":{"status":"0x1","contractAddress":format!("0x{}", "34".repeat(20))}});
        assert_eq!(receipt_status(&valid), Some(true));
        assert!(valid_created_address(&valid));
        for address in [
            Value::Null,
            json!("0x1234"),
            json!(format!("0x{}", "00".repeat(20))),
            json!(format!("0x{}", "zz".repeat(20))),
        ] {
            assert!(!valid_created_address(
                &json!({"result":{"contractAddress":address}})
            ));
        }
        let reverted = json!({"result":{"status":"0x0","contractAddress":null}});
        assert_eq!(receipt_status(&reverted), Some(false));
        assert!(!valid_created_address(&reverted));
        let mut context = ResultContext {
            contract: String::new(),
            address: String::new(),
            signature: "constructor".into(),
            request: json!({"submissionState":"unknown"}),
            workspace_epoch: 0,
            network_epoch: 0,
            source: Value::Null,
        };
        mark_confirmed(
            &mut context,
            &reverted,
            json!({"block":{"number":"0x1"}}),
            true,
        );
        assert_eq!(context.request["submissionState"], "confirmed");
        assert!(context.address.is_empty());
        mark_confirmed(&mut context, &valid, Value::Null, true);
        assert_eq!(
            context.address,
            valid["result"]["contractAddress"].as_str().unwrap()
        );
        assert_eq!(receipt_status(&json!({"result":{"status":"0x2"}})), None);
        assert_eq!(receipt_status(&json!({"result":{"status":"0x01"}})), None);
    }

    #[test]
    fn presend_errors_name_the_revert_and_never_read_a_missing_quantity() {
        use ethers::abi::{encode, Token};
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend(encode(&[Token::String("Minimum Withdrawal is 1 Ether".into())]));
        let reverted = json!({"jsonrpc":"2.0","id":1,"error":{"code":3,"message":"[provider message redacted]","data":format!("0x{}", ethers::utils::hex::encode(&data))}});
        let error = preflight_quantity(&reverted, "eth_estimateGas", &Abi::default()).unwrap_err();
        assert!(error.starts_with(REVERTED_BEFORE_SENDING), "{error}");
        assert!(error.contains("Minimum Withdrawal is 1 Ether"), "{error}");
        assert!(!error.contains("Missing RPC quantity"));
        let bare = json!({"jsonrpc":"2.0","id":1,"error":{"code":3,"message":"[provider message redacted]"}});
        let error = preflight_quantity(&bare, "eth_estimateGas", &Abi::default()).unwrap_err();
        assert!(error.contains("no revert data"), "{error}");
        let nonce = preflight_quantity(&bare, "eth_getTransactionCount", &Abi::default()).unwrap_err();
        assert!(nonce.contains("eth_getTransactionCount failed (code 3)"), "{nonce}");
        let gas = json!({"jsonrpc":"2.0","id":1,"result":"0x5208"});
        assert_eq!(preflight_quantity(&gas, "eth_estimateGas", &Abi::default()).unwrap(), U256::from(21000u64));
    }

    #[test]
    fn rpc_envelopes_are_checked_and_untrusted_error_text_is_redacted() {
        let fixture = "synthetic-signed-payload-do-not-retain";
        let raw = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":fixture,"data":{"nested":fixture},"extra":fixture},"extra":fixture});
        let safe = validate_and_redact_rpc(raw, "eth_sendRawTransaction").unwrap();
        assert_eq!(safe["error"]["code"], -32000);
        assert_eq!(safe["error"]["message"], "[provider message redacted]");
        assert_eq!(safe["chasmRedaction"]["applied"], true);
        assert!(!safe.to_string().contains(fixture));
        let revert = "0x08c379a0";
        let call = validate_and_redact_rpc(json!({"jsonrpc":"2.0","id":1,"error":{"code":3,"message":fixture,"data":{"originalError":{"data":revert},"message":fixture}}}), "eth_call").unwrap();
        assert_eq!(call["error"]["data"], revert);
        assert_eq!(call["error"]["code"], 3);
        assert!(!call.to_string().contains(fixture));
        let invalid_submission = validate_and_redact_rpc(
            json!({"jsonrpc":"2.0","id":1,"result":fixture}),
            "eth_sendRawTransaction",
        )
        .unwrap();
        assert!(!invalid_submission.to_string().contains(fixture));
        assert_eq!(invalid_submission["chasmRedaction"]["applied"], true);
        for bad in [
            json!({"id":1,"result":"0x1"}),
            json!({"jsonrpc":"1.0","id":1,"result":"0x1"}),
            json!({"jsonrpc":"2.0","id":2,"result":"0x1"}),
            json!({"jsonrpc":"2.0","id":1,"result":"0x1","error":{"code":3,"message":"x"}}),
            json!({"jsonrpc":"2.0","id":1}),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":3}}),
            json!({"jsonrpc":"2.0","id":1,"error":null}),
        ] {
            assert!(validate_and_redact_rpc(bad, "eth_call").is_err());
        }
    }

    #[test]
    fn trace_child_fixture() {
        use std::io::Write;
        match std::env::var("CHASM_TRACE_TEST_FIXTURE").ok().as_deref() {
            Some("both") => {
                let bytes = [b'x'; 4096];
                let mut stdout = std::io::stdout().lock();
                let mut stderr = std::io::stderr().lock();
                for _ in 0..64 {
                    stdout.write_all(&bytes).unwrap();
                    stderr.write_all(&bytes).unwrap();
                }
            }
            Some("overflow") => {
                let bytes = [b'y'; 4096];
                let mut stdout = std::io::stdout().lock();
                let mut stderr = std::io::stderr().lock();
                for _ in 0..512 {
                    if stdout.write_all(&bytes).is_err() || stderr.write_all(&bytes).is_err() {
                        return;
                    }
                }
            }
            Some("timeout") => std::thread::sleep(Duration::from_secs(30)),
            Some("failure") => {
                std::io::stderr()
                    .write_all(b"synthetic provider credential")
                    .unwrap();
                panic!("fixture failure");
            }
            _ => {}
        }
    }

    fn fixture_child(mode: &str) -> tokio::process::Child {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        let module = module_path!().split_once("::").unwrap().1;
        let fixture = format!("{module}::trace_child_fixture");
        command
            .arg("--exact")
            .arg(fixture)
            .arg("--nocapture")
            .env("CHASM_TRACE_TEST_FIXTURE", mode)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        command.spawn().unwrap()
    }

    #[tokio::test]
    async fn pipe_drain_enforces_shared_budget_and_discards_stderr() {
        use tokio::io::AsyncWriteExt;
        let (mut out_writer, out_reader) = tokio::io::duplex(32);
        let (mut err_writer, err_reader) = tokio::io::duplex(32);
        let used = std::sync::atomic::AtomicUsize::new(0);
        let write = async {
            let (stdout, stderr) = tokio::join!(
                out_writer.write_all(b"12345678"),
                err_writer.write_all(b"secret!!")
            );
            stdout.unwrap();
            stderr.unwrap();
            drop(out_writer);
            drop(err_writer);
        };
        let read = async {
            tokio::try_join!(
                read_trace_pipe(out_reader, true, &used, 15),
                read_trace_pipe(err_reader, false, &used, 15)
            )
        };
        let (_, result) = tokio::join!(write, read);
        assert_eq!(result, Err(TraceProcessError::TooLarge));
    }

    #[tokio::test]
    async fn trace_drains_stdout_and_stderr_without_deadlock() {
        let mut child = fixture_child("both");
        let output = collect_trace(&mut child, Duration::from_secs(10), MAX_TRACE_BYTES)
            .await
            .unwrap();
        assert!(output.iter().filter(|byte| **byte == b'x').count() >= 64 * 4096);
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn trace_combined_output_overflow_kills_and_reaps() {
        let mut child = fixture_child("overflow");
        let result = collect_trace(&mut child, Duration::from_secs(10), MAX_TRACE_BYTES).await;
        assert_eq!(result, Err(TraceProcessError::TooLarge));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn trace_timeout_kills_and_reaps() {
        let mut child = fixture_child("timeout");
        let result = collect_trace(&mut child, Duration::from_millis(500), MAX_TRACE_BYTES).await;
        assert_eq!(result, Err(TraceProcessError::Timeout));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn trace_failure_discards_provider_stderr() {
        let mut child = fixture_child("failure");
        let result = collect_trace(&mut child, Duration::from_secs(10), MAX_TRACE_BYTES).await;
        assert_eq!(result, Err(TraceProcessError::Exit));
        assert!(child.try_wait().unwrap().is_some());
    }
}
