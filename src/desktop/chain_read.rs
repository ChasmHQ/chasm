//! Read-only chain inspection over JSON-RPC.
//!
//! Single entry point: [`query`] takes an endpoint URL and a typed
//! [`ReadRequest`], and returns a provenance-stamped JSON envelope:
//!
//! ```json
//! { "chain_id": "1337", "genesis_block_hash": "0x..", "block": { "number": "0x10", "hash": "0x.." }, "data": ... }
//! ```
//!
//! Every result carries the chain id, the genesis block hash, and the exact
//! block (number + hash) the data was read at. There is no silent fallback to
//! `latest`: a historical selector the node cannot serve is a [`ReadErrorCode::NotFound`]
//! error, and an explicit `latest` is pinned once (resolved to a concrete
//! number/hash) before any operation runs. After account/storage/simulation
//! reads the pinned header is re-fetched and compared; a changed hash is a
//! [`ReadErrorCode::Reorg`] error instead of silently mis-pinned data.
//! All parameters are validated before the first RPC is sent.
//!
//! Hard limits: 10s request timeout, redirects disabled, 8 MiB response cap,
//! and a fixed method allowlist (read methods only; `Simulate` is `eth_call`
//! and never signs or broadcasts, so repeat calls are safe). Endpoint URLs
//! with userinfo, query parameters, or fragments are rejected. Error messages
//! never include the endpoint URL, which may carry secrets.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Hard cap on any single RPC response body.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Total request timeout (connect through response body).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum blocks per `history` page. Block-page only; there is no
/// address-filtered full-history claim.
pub const MAX_HISTORY_BLOCKS: usize = 20;

/// The only JSON-RPC methods this module will ever send. Every outbound call
/// is funneled through `rpc_call`, which checks this list before touching the
/// network, so broadcast/signing methods are unreachable by construction.
const ALLOWED_METHODS: &[&str] = &[
    "eth_chainId",
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_call",
    // Read-only node info, for the fork block when block 0 is pruned.
    "anvil_nodeInfo",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadErrorCode {
    InvalidInput,
    InvalidUrl,
    MethodNotAllowed,
    Network,
    Timeout,
    TooLarge,
    BadResponse,
    HttpError,
    RpcError,
    CallReverted,
    NotFound,
    TxPending,
    /// A mined transaction whose receipt is missing; node state is incomplete.
    Incomplete,
    /// The pinned block header changed between resolve and read.
    Reorg,
}

impl ReadErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidUrl => "invalid_url",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::TooLarge => "too_large",
            Self::BadResponse => "bad_response",
            Self::HttpError => "http_error",
            Self::RpcError => "rpc_error",
            Self::CallReverted => "call_reverted",
            Self::NotFound => "not_found",
            Self::TxPending => "tx_pending",
            Self::Incomplete => "incomplete",
            Self::Reorg => "reorg",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadError {
    pub code: ReadErrorCode,
    pub message: String,
    /// JSON-RPC error code from the node, when the failure came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_code: Option<i64>,
}

impl ReadError {
    pub fn new(code: ReadErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            rpc_code: None,
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rpc_code {
            Some(c) => write!(
                f,
                "{}: {} (rpc error {})",
                self.code.as_str(),
                self.message,
                c
            ),
            None => write!(f, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for ReadError {}

/// Block selector: `"latest"`, a decimal number, a `0x` hex quantity, or a
/// `0x` block hash. A `0x` string of exactly 64 hex digits is a hash; shorter
/// ones are quantities. Historical selectors are used exactly as given.
/// Serializes back as a single string (`"latest"`, `"0x10"`, `"0x<hash>"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockRef {
    Latest,
    Number(u64),
    Hash(String),
}

impl BlockRef {
    pub fn parse(s: &str) -> Result<Self, ReadError> {
        let bad = || {
            ReadError::new(
            ReadErrorCode::InvalidInput,
            "block selector must be \"latest\", a decimal number, a 0x quantity, or a 0x 32-byte hash",
        )
        };
        if s == "latest" {
            return Ok(Self::Latest);
        }
        if let Some(h) = s.strip_prefix("0x") {
            if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad());
            }
            if h.len() == 64 {
                return Ok(Self::Hash(format!("0x{}", h.to_ascii_lowercase())));
            }
            let lower = h.to_ascii_lowercase();
            let significant = lower.trim_start_matches('0');
            let number = if significant.is_empty() {
                0
            } else {
                u64::from_str_radix(significant, 16).map_err(|_| {
                    ReadError::new(ReadErrorCode::InvalidInput, "block number too large")
                })?
            };
            return Ok(Self::Number(number));
        }
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            return s.parse::<u64>().map(Self::Number).map_err(|_| {
                ReadError::new(ReadErrorCode::InvalidInput, "block number too large")
            });
        }
        Err(bad())
    }
}

impl Serialize for BlockRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::Latest => "latest".to_string(),
            Self::Number(n) => format!("{n:#x}"),
            Self::Hash(h) => h.clone(),
        };
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for BlockRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match Value::deserialize(deserializer)? {
            Value::String(s) => Self::parse(&s).map_err(serde::de::Error::custom),
            Value::Number(n) => n
                .as_u64()
                .map(Self::Number)
                .ok_or_else(|| serde::de::Error::custom("block number too large")),
            _ => Err(serde::de::Error::custom(
                "block selector must be a string or number",
            )),
        }
    }
}

/// `eth_call` payload for [`ReadRequest::Simulate`]. Value/gas forms accept
/// decimal or `0x` hex and are normalized to canonical hex quantities before
/// the RPC. Read-only: nothing here signs or broadcasts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, alias = "input", skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas: Option<String>,
    #[serde(rename = "gasPrice", default, skip_serializing_if = "Option::is_none")]
    pub gas_price: Option<String>,
}

/// Typed read request. Serialized as an internally tagged enum:
/// `{"op": "block", "selector": "latest", "full_tx": false}`. Unknown fields
/// inside a variant, and unknown `op` values, are rejected on deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadRequest {
    /// Full block, with transaction objects when `full_tx` is set.
    Block {
        selector: BlockRef,
        #[serde(default)]
        full_tx: bool,
    },
    /// Full transaction plus its receipt (logs and calldata included) as
    /// separate objects. Pending classification happens only when the
    /// transaction's `blockNumber` is null ([`ReadErrorCode::TxPending`]);
    /// a mined transaction without a receipt is [`ReadErrorCode::Incomplete`];
    /// receipt RPC errors surface as [`ReadErrorCode::RpcError`], never as
    /// pending.
    Transaction { hash: String },
    /// Balance, nonce, and deployed code at a pinned block.
    Account { address: String, block: BlockRef },
    /// Contract storage slot at a pinned block. Slot accepts decimal or `0x`
    /// hex up to 256 bits and is sent as a canonical hex quantity.
    Storage {
        address: String,
        slot: String,
        block: BlockRef,
    },
    /// `eth_call` simulation at a pinned block.
    Simulate { call: CallRequest, block: BlockRef },
    /// Consecutive blocks starting at `from_block`, at most
    /// [`MAX_HISTORY_BLOCKS`]. This is a block page only; address-filtered
    /// full history is out of scope.
    History {
        from_block: BlockRef,
        count: u8,
        #[serde(default)]
        full_tx: bool,
    },
}

enum Plan {
    Block {
        selector: BlockRef,
        full_tx: bool,
    },
    Transaction {
        hash: String,
    },
    Account {
        address: String,
        block: BlockRef,
    },
    Storage {
        address: String,
        slot: String,
        block: BlockRef,
    },
    Simulate {
        call: Value,
        block: BlockRef,
    },
    History {
        from_block: BlockRef,
        count: usize,
        full_tx: bool,
    },
}

impl ReadRequest {
    fn validate(self) -> Result<Plan, ReadError> {
        Ok(match self {
            Self::Block { selector, full_tx } => {
                validate_ref(&selector)?;
                Plan::Block { selector, full_tx }
            }
            Self::Transaction { hash } => {
                require_hash(&hash, "transaction hash")?;
                Plan::Transaction { hash }
            }
            Self::Account { address, block } => {
                require_address(&address)?;
                validate_ref(&block)?;
                Plan::Account { address, block }
            }
            Self::Storage {
                address,
                slot,
                block,
            } => {
                require_address(&address)?;
                validate_ref(&block)?;
                let slot = to_hex_quantity(&slot, "storage slot")?;
                Plan::Storage {
                    address,
                    slot,
                    block,
                }
            }
            Self::Simulate { call, block } => {
                let call = validate_call(call)?;
                validate_ref(&block)?;
                Plan::Simulate { call, block }
            }
            Self::History {
                from_block,
                count,
                full_tx,
            } => {
                validate_ref(&from_block)?;
                if count == 0 || count as usize > MAX_HISTORY_BLOCKS {
                    return Err(ReadError::new(
                        ReadErrorCode::InvalidInput,
                        format!("history count must be 1..={MAX_HISTORY_BLOCKS}"),
                    ));
                }
                Plan::History {
                    from_block,
                    count: count as usize,
                    full_tx,
                }
            }
        })
    }
}

pub async fn query(rpc_url: &str, request: ReadRequest) -> Result<Value, ReadError> {
    let plan = request.validate()?;
    validate_url(rpc_url)?;
    let http = build_client()?;
    let identity = load_identity(&http, rpc_url).await?;

    let (block, data) = match plan {
        Plan::Block { selector, full_tx } => {
            let resolved = resolve_block(&http, rpc_url, &selector).await?;
            let pinned = resolved.number.clone();
            let raw = match &selector {
                BlockRef::Hash(h) => {
                    rpc_call(&http, rpc_url, "eth_getBlockByHash", json!([h, full_tx])).await?
                }
                _ => {
                    rpc_call(
                        &http,
                        rpc_url,
                        "eth_getBlockByNumber",
                        json!([pinned, full_tx]),
                    )
                    .await?
                }
            };
            let fetched = raw.as_object().ok_or_else(|| {
                ReadError::new(
                    ReadErrorCode::NotFound,
                    format!("block not found: {selector:?}"),
                )
            })?;
            let (number, hash) = block_number_hash(fetched)?;
            if number != resolved.number || hash != resolved.hash {
                return Err(ReadError::new(
                    ReadErrorCode::Reorg,
                    "node returned inconsistent block identity (reorg or bad pin)",
                ));
            }
            (
                ResolvedBlock { number, hash },
                Value::Object(fetched.clone()),
            )
        }
        Plan::Transaction { hash } => {
            let tx = rpc_call(
                &http,
                rpc_url,
                "eth_getTransactionByHash",
                json!([hash.as_str()]),
            )
            .await?;
            if tx.is_null() {
                return Err(ReadError::new(
                    ReadErrorCode::NotFound,
                    "transaction not found",
                ));
            }
            let tx_obj = tx.as_object().ok_or_else(|| {
                ReadError::new(
                    ReadErrorCode::BadResponse,
                    "transaction response is not an object",
                )
            })?;
            if hash_field(
                tx_obj.get("hash").unwrap_or(&Value::Null),
                "transaction hash",
            )? != hash
            {
                return Err(ReadError::new(
                    ReadErrorCode::BadResponse,
                    "Provider returned a different transaction.",
                ));
            }
            let tx_block = match tx_obj.get("blockNumber") {
                Some(Value::Null) => {
                    return Err(ReadError::new(
                        ReadErrorCode::TxPending,
                        "transaction is pending (no block number yet)",
                    ));
                }
                Some(value) => quantity_field(value, "transaction block number")?,
                None => {
                    return Err(ReadError::new(
                        ReadErrorCode::BadResponse,
                        "Transaction response is missing blockNumber.",
                    ))
                }
            };
            let receipt = rpc_call(
                &http,
                rpc_url,
                "eth_getTransactionReceipt",
                json!([hash.as_str()]),
            )
            .await?;
            if receipt.is_null() {
                return Err(ReadError::new(
                    ReadErrorCode::Incomplete,
                    format!(
                        "mined transaction at {tx_block} has no receipt (node state incomplete)"
                    ),
                ));
            }
            let block = receipt_as_block(&receipt)?;
            if block.number != tx_block {
                return Err(ReadError::new(
                    ReadErrorCode::BadResponse,
                    "Transaction and receipt block numbers disagree.",
                ));
            }
            for object in [&tx, &receipt] {
                if !hash_field(
                    object
                        .get("transactionHash")
                        .or_else(|| object.get("hash"))
                        .unwrap_or(&Value::Null),
                    "transaction identity",
                )?
                .eq_ignore_ascii_case(&hash)
                {
                    return Err(ReadError::new(
                        ReadErrorCode::BadResponse,
                        "Provider returned a different transaction.",
                    ));
                }
                if !hash_field(
                    object.get("blockHash").unwrap_or(&Value::Null),
                    "transaction block hash",
                )?
                .eq_ignore_ascii_case(&block.hash)
                {
                    return Err(ReadError::new(
                        ReadErrorCode::Reorg,
                        "Transaction and receipt block hashes disagree.",
                    ));
                }
            }
            (block, json!({ "transaction": tx, "receipt": receipt }))
        }
        Plan::Account {
            address,
            block: selector,
        } => {
            let resolved = resolve_block(&http, rpc_url, &selector).await?;
            let pinned = resolved.number.clone();
            let balance = rpc_call(
                &http,
                rpc_url,
                "eth_getBalance",
                json!([address.as_str(), pinned.as_str()]),
            )
            .await?;
            let nonce = rpc_call(
                &http,
                rpc_url,
                "eth_getTransactionCount",
                json!([address.as_str(), pinned.as_str()]),
            )
            .await?;
            let code = rpc_call(
                &http,
                rpc_url,
                "eth_getCode",
                json!([address.as_str(), pinned.as_str()]),
            )
            .await?;
            quantity_field(&balance, "balance")?;
            quantity_field(&nonce, "nonce")?;
            hex_data_field(&code, "code")?;
            verify_pin(&http, rpc_url, &resolved).await?;
            (
                resolved,
                json!({ "address": address, "balance": balance, "nonce": nonce, "code": code }),
            )
        }
        Plan::Storage {
            address,
            slot,
            block: selector,
        } => {
            let resolved = resolve_block(&http, rpc_url, &selector).await?;
            let pinned = resolved.number.clone();
            let value = rpc_call(
                &http,
                rpc_url,
                "eth_getStorageAt",
                json!([address.as_str(), slot.as_str(), pinned.as_str()]),
            )
            .await?;
            let value = storage_word_field(&value)?;
            verify_pin(&http, rpc_url, &resolved).await?;
            (
                resolved,
                json!({ "address": address, "slot": slot, "value": value }),
            )
        }
        Plan::Simulate {
            call,
            block: selector,
        } => {
            let resolved = resolve_block(&http, rpc_url, &selector).await?;
            let pinned = resolved.number.clone();
            let result =
                rpc_call(&http, rpc_url, "eth_call", json!([call, pinned.as_str()])).await?;
            let result = hex_data_field(&result, "eth_call result")?;
            verify_pin(&http, rpc_url, &resolved).await?;
            (resolved, json!({ "return_data": result }))
        }
        Plan::History {
            from_block,
            count,
            full_tx,
        } => {
            let start = resolve_block(&http, rpc_url, &from_block).await?;
            let start_number = quantity_to_u64(&start.number)?;
            let mut blocks = Vec::with_capacity(count);
            let mut response_bytes = 0usize;
            let mut previous_hash = None;
            for offset in 0..count {
                let number = start_number.checked_add(offset as u64).ok_or_else(|| {
                    ReadError::new(
                        ReadErrorCode::BadResponse,
                        "history page exceeds block number range",
                    )
                })?;
                let raw = rpc_call(
                    &http,
                    rpc_url,
                    "eth_getBlockByNumber",
                    json!([format!("{number:#x}"), full_tx]),
                )
                .await?;
                let fetched = raw.as_object().ok_or_else(|| {
                    ReadError::new(
                        ReadErrorCode::NotFound,
                        format!("block not found at {number:#x}"),
                    )
                })?;
                let (page_number, page_hash) = block_number_hash(fetched)?;
                if offset == 0 && (page_number != start.number || page_hash != start.hash) {
                    return Err(ReadError::new(
                        ReadErrorCode::Reorg,
                        "history page header changed after resolve (reorg or bad pin)",
                    ));
                }
                if quantity_to_u64(&page_number)? != number {
                    return Err(ReadError::new(
                        ReadErrorCode::BadResponse,
                        "History returned a different block number.",
                    ));
                }
                let parent = hash_field(
                    fetched.get("parentHash").unwrap_or(&Value::Null),
                    "history parent hash",
                )?;
                if let Some(previous) = &previous_hash {
                    if &parent != previous {
                        return Err(ReadError::new(
                            ReadErrorCode::Reorg,
                            "History page parent hashes are inconsistent.",
                        ));
                    }
                }
                previous_hash = Some(page_hash);
                response_bytes += serde_json::to_vec(fetched)
                    .map_err(|_| {
                        ReadError::new(ReadErrorCode::BadResponse, "Invalid block response.")
                    })?
                    .len();
                if response_bytes > MAX_RESPONSE_BYTES {
                    return Err(ReadError::new(
                        ReadErrorCode::TooLarge,
                        "History page exceeds the response limit. Reduce the count.",
                    ));
                }
                blocks.push(Value::Object(fetched.clone()));
            }
            let data = json!({
                "from_block": { "number": start.number, "hash": start.hash },
                "blocks": blocks,
            });
            (start, data)
        }
    };

    let final_identity = load_identity(&http, rpc_url).await?;
    if final_identity.chain_id != identity.chain_id
        || final_identity.genesis_hash != identity.genesis_hash
    {
        return Err(ReadError::new(
            ReadErrorCode::Reorg,
            "Network identity changed during the read; no evidence returned.",
        ));
    }
    Ok(json!({
        "chain_id": identity.chain_id,
        "genesis_block_hash": identity.genesis_hash,
        "block": { "number": block.number, "hash": block.hash },
        "data": data,
    }))
}

// Re-fetch the pinned header after a state read; a changed hash means the
// number pin can no longer be trusted (reorg), so the result is discarded.
async fn verify_pin(
    http: &reqwest::Client,
    url: &str,
    pinned: &ResolvedBlock,
) -> Result<(), ReadError> {
    let raw = rpc_call(
        http,
        url,
        "eth_getBlockByNumber",
        json!([pinned.number.as_str(), false]),
    )
    .await?;
    let fetched = raw.as_object().ok_or_else(|| {
        ReadError::new(
            ReadErrorCode::NotFound,
            format!("pinned block {} no longer available", pinned.number),
        )
    })?;
    let (number, hash) = block_number_hash(fetched)?;
    if number != pinned.number {
        return Err(ReadError::new(
            ReadErrorCode::BadResponse,
            "node returned a different block number during verify",
        ));
    }
    if hash != pinned.hash {
        return Err(ReadError::new(
            ReadErrorCode::Reorg,
            "block hash changed during read (reorg or bad pin)",
        ));
    }
    Ok(())
}

struct ChainIdentity {
    chain_id: String,
    genesis_hash: String,
}

async fn load_identity(http: &reqwest::Client, url: &str) -> Result<ChainIdentity, ReadError> {
    let raw = rpc_call(http, url, "eth_chainId", json!([])).await?;
    let chain_id = quantity_to_decimal(&quantity_field(&raw, "chain id")?)?;
    let mut raw = rpc_call(http, url, "eth_getBlockByNumber", json!(["0x0", false])).await?;
    let mut anchored = false;
    if raw.is_null() {
        // Pruned block 0: a fork node's fork block pins the identity instead.
        if let Ok(info) = rpc_call(http, url, "anvil_nodeInfo", json!([])).await {
            if let Some(block) = info["forkConfig"]["forkBlockNumber"].as_u64() {
                raw = rpc_call(http, url, "eth_getBlockByNumber", json!([format!("{block:#x}"), false])).await?;
                anchored = true;
            }
        }
    }
    let genesis = raw.as_object().ok_or_else(|| {
        ReadError::new(ReadErrorCode::BadResponse, "node serves neither block 0 nor a fork block")
    })?;
    if !anchored && genesis.get("number") != Some(&json!("0x0")) {
        return Err(ReadError::new(
            ReadErrorCode::BadResponse,
            "genesis block number is not 0x0",
        ));
    }
    let (_, hash) = block_number_hash(genesis)?;
    Ok(ChainIdentity {
        chain_id,
        genesis_hash: hash,
    })
}

struct ResolvedBlock {
    number: String,
    hash: String,
}

async fn resolve_block(
    http: &reqwest::Client,
    url: &str,
    selector: &BlockRef,
) -> Result<ResolvedBlock, ReadError> {
    let raw = match selector {
        BlockRef::Latest => {
            rpc_call(http, url, "eth_getBlockByNumber", json!(["latest", false])).await?
        }
        BlockRef::Number(n) => {
            rpc_call(
                http,
                url,
                "eth_getBlockByNumber",
                json!([format!("{n:#x}"), false]),
            )
            .await?
        }
        BlockRef::Hash(h) => rpc_call(http, url, "eth_getBlockByHash", json!([h, false])).await?,
    };
    let block = raw.as_object().ok_or_else(|| {
        ReadError::new(
            ReadErrorCode::NotFound,
            format!("block not found: {selector:?}"),
        )
    })?;
    let (number, hash) = block_number_hash(block)?;
    let matches = match selector {
        BlockRef::Latest => true,
        BlockRef::Number(expected) => quantity_to_u64(&number)? == *expected,
        BlockRef::Hash(expected) => hash.eq_ignore_ascii_case(expected),
    };
    if !matches {
        return Err(ReadError::new(
            ReadErrorCode::BadResponse,
            "Provider returned a different block than requested.",
        ));
    }
    Ok(ResolvedBlock { number, hash })
}

fn receipt_as_block(receipt: &Value) -> Result<ResolvedBlock, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::BadResponse,
            "receipt missing block number/hash",
        )
    };
    let number = quantity_field(
        receipt.get("blockNumber").ok_or_else(bad)?,
        "receipt block number",
    )?;
    let hash = hash_field(
        receipt.get("blockHash").ok_or_else(bad)?,
        "receipt block hash",
    )?;
    Ok(ResolvedBlock { number, hash })
}

fn block_number_hash(
    block: &serde_json::Map<String, Value>,
) -> Result<(String, String), ReadError> {
    let number = quantity_field(block.get("number").unwrap_or(&Value::Null), "block number")?;
    let hash = hash_field(block.get("hash").unwrap_or(&Value::Null), "block hash")?;
    Ok((number, hash))
}

fn build_client() -> Result<reqwest::Client, ReadError> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ReadError::new(ReadErrorCode::Network, "failed to build HTTP client"))
}

async fn rpc_call(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value, ReadError> {
    if !ALLOWED_METHODS.contains(&method) {
        return Err(ReadError::new(
            ReadErrorCode::MethodNotAllowed,
            format!("method not in read-only allowlist: {method}"),
        ));
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(transport_error)?;
    let status = resp.status();
    let bytes = read_bounded(resp).await?;
    let parsed: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ReadError::new(ReadErrorCode::BadResponse, "RPC response is not valid JSON")
    })?;
    if let Some(err) = parsed.get("error").filter(|e| !e.is_null()) {
        let rpc_code = err.get("code").and_then(Value::as_i64);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("RPC error");
        return Err(ReadError {
            code: if method == "eth_call" && rpc_code == Some(3) {
                ReadErrorCode::CallReverted
            } else {
                ReadErrorCode::RpcError
            },
            message: if message.to_ascii_lowercase().contains("revert") {
                "Read-only simulation reverted. Provider details are not exposed.".into()
            } else {
                "The provider rejected this historical read. Data may be pruned or the method unsupported.".into()
            },
            rpc_code,
        });
    }
    if !status.is_success() {
        return Err(ReadError::new(
            ReadErrorCode::HttpError,
            format!("RPC endpoint returned HTTP {}", status.as_u16()),
        ));
    }
    match parsed.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(ReadError::new(
            ReadErrorCode::BadResponse,
            "RPC response missing result",
        )),
    }
}

async fn read_bounded(mut resp: reqwest::Response) -> Result<Vec<u8>, ReadError> {
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_RESPONSE_BYTES {
            return Err(ReadError::new(
                ReadErrorCode::TooLarge,
                format!("response exceeds {MAX_RESPONSE_BYTES} byte cap"),
            ));
        }
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
        if out.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ReadError::new(
                ReadErrorCode::TooLarge,
                format!("response exceeds {MAX_RESPONSE_BYTES} byte cap"),
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

// reqwest error text embeds the URL, which may carry an API key; keep fixed messages.
fn transport_error(e: reqwest::Error) -> ReadError {
    if e.is_timeout() {
        ReadError::new(
            ReadErrorCode::Timeout,
            format!("request timed out after {}s", REQUEST_TIMEOUT.as_secs()),
        )
    } else {
        ReadError::new(
            ReadErrorCode::Network,
            "request failed (connection or transport error)",
        )
    }
}

fn validate_url(url: &str) -> Result<(), ReadError> {
    let rest = match url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    {
        Some(r) => r,
        None => {
            return Err(ReadError::new(
                ReadErrorCode::InvalidUrl,
                "RPC endpoint must be http:// or https://",
            ))
        }
    };
    if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('#') {
        return Err(ReadError::new(
            ReadErrorCode::InvalidUrl,
            "RPC endpoint has no host",
        ));
    }
    let authority = rest
        .split(|c| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or("");
    if authority.contains('@') {
        return Err(ReadError::new(
            ReadErrorCode::InvalidUrl,
            "RPC endpoint must not contain userinfo",
        ));
    }
    if url.contains('?') {
        return Err(ReadError::new(
            ReadErrorCode::InvalidUrl,
            "RPC endpoint must not contain query parameters",
        ));
    }
    if url.contains('#') {
        return Err(ReadError::new(
            ReadErrorCode::InvalidUrl,
            "RPC endpoint must not contain a fragment",
        ));
    }
    Ok(())
}

fn validate_ref(selector: &BlockRef) -> Result<(), ReadError> {
    match selector {
        BlockRef::Latest | BlockRef::Number(_) => Ok(()),
        BlockRef::Hash(h) => require_hash(h, "block hash"),
    }
}

fn validate_call(call: CallRequest) -> Result<Value, ReadError> {
    let mut obj = serde_json::Map::new();
    if let Some(a) = call.from {
        require_address(&a)?;
        obj.insert("from".to_string(), Value::String(a));
    }
    if let Some(a) = call.to {
        require_address(&a)?;
        obj.insert("to".to_string(), Value::String(a));
    }
    if let Some(d) = call.data {
        require_hex_data(&d, "calldata")?;
        obj.insert("data".to_string(), Value::String(d));
    }
    if let Some(v) = call.value {
        obj.insert(
            "value".to_string(),
            Value::String(to_hex_quantity(&v, "call value")?),
        );
    }
    let gas = to_hex_quantity(call.gas.as_deref().unwrap_or("5000000"), "call gas")?;
    if ethers::types::U256::from_str_radix(&gas[2..], 16)
        .map_err(|_| ReadError::new(ReadErrorCode::InvalidInput, "Invalid gas."))?
        > ethers::types::U256::from(30_000_000u64)
    {
        return Err(ReadError::new(
            ReadErrorCode::InvalidInput,
            "Simulation gas is limited to 30000000.",
        ));
    }
    obj.insert("gas".to_string(), Value::String(gas));
    if let Some(g) = call.gas_price {
        obj.insert(
            "gasPrice".to_string(),
            Value::String(to_hex_quantity(&g, "call gas price")?),
        );
    }
    Ok(Value::Object(obj))
}

fn require_address(s: &str) -> Result<(), ReadError> {
    let ok = matches!(s.strip_prefix("0x"), Some(h) if h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()));
    if ok {
        Ok(())
    } else {
        Err(ReadError::new(
            ReadErrorCode::InvalidInput,
            "address must be 0x-prefixed 20-byte hex",
        ))
    }
}

fn require_hash(s: &str, what: &str) -> Result<(), ReadError> {
    if is_32byte_hex(s) {
        Ok(())
    } else {
        Err(ReadError::new(
            ReadErrorCode::InvalidInput,
            format!("{what} must be 0x-prefixed 32-byte hex"),
        ))
    }
}

fn is_32byte_hex(s: &str) -> bool {
    matches!(s.strip_prefix("0x"), Some(h) if h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

fn require_hex_data(s: &str, what: &str) -> Result<(), ReadError> {
    let ok = match s.strip_prefix("0x") {
        Some(h) => h.chars().all(|c| c.is_ascii_hexdigit()) && h.len() % 2 == 0,
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ReadError::new(
            ReadErrorCode::InvalidInput,
            format!("{what} must be 0x-prefixed even-length hex"),
        ))
    }
}

/// Accepts decimal or `0x` hex input up to 256 bits and returns the canonical
/// minimal hex quantity actually sent to the node.
fn to_hex_quantity(s: &str, what: &str) -> Result<String, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::InvalidInput,
            format!("{what} must be an exact uint256 decimal or 0x hex quantity"),
        )
    };
    let number = if let Some(hex) = s.strip_prefix("0x") {
        if hex.is_empty() || hex.len() > 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(bad());
        }
        ethers::types::U256::from_str_radix(hex, 16).map_err(|_| bad())?
    } else {
        if s.is_empty() || s.len() > 78 || !s.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(bad());
        }
        ethers::types::U256::from_dec_str(s).map_err(|_| bad())?
    };
    Ok(format!("{number:#x}"))
}

fn is_canonical_quantity(s: &str) -> bool {
    let Some(h) = s.strip_prefix("0x") else {
        return false;
    };
    if h.is_empty() || h.len() > 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let significant = h.trim_start_matches('0');
    if significant.is_empty() {
        return h.len() == 1;
    }
    significant.len() == h.len()
}

fn quantity_field(v: &Value, what: &str) -> Result<String, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::BadResponse,
            format!("node returned a malformed {what}"),
        )
    };
    let s = v.as_str().ok_or_else(bad)?;
    if !is_canonical_quantity(s) {
        return Err(bad());
    }
    Ok(s.to_string())
}

fn hash_field(v: &Value, what: &str) -> Result<String, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::BadResponse,
            format!("node returned a malformed {what}"),
        )
    };
    let s = v.as_str().ok_or_else(bad)?;
    if !is_32byte_hex(s) {
        return Err(bad());
    }
    Ok(s.to_string())
}

fn storage_word_field(v: &Value) -> Result<String, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::BadResponse,
            "node returned a malformed storage value",
        )
    };
    let s = v.as_str().ok_or_else(bad)?;
    if !is_32byte_hex(s) {
        return Err(bad());
    }
    Ok(s.to_string())
}

fn hex_data_field(v: &Value, what: &str) -> Result<String, ReadError> {
    let bad = || {
        ReadError::new(
            ReadErrorCode::BadResponse,
            format!("node returned a malformed {what}"),
        )
    };
    let s = v.as_str().ok_or_else(bad)?;
    match s.strip_prefix("0x") {
        Some(h) if h.chars().all(|c| c.is_ascii_hexdigit()) && h.len() % 2 == 0 => {
            Ok(s.to_string())
        }
        _ => Err(bad()),
    }
}

fn quantity_to_decimal(s: &str) -> Result<String, ReadError> {
    let digits = &s[2..];
    let n = ethers::types::U256::from_str_radix(digits, 16)
        .map_err(|_| ReadError::new(ReadErrorCode::BadResponse, "value out of range"))?;
    Ok(n.to_string())
}

fn quantity_to_u64(s: &str) -> Result<u64, ReadError> {
    u64::from_str_radix(&s[2..], 16)
        .map_err(|_| ReadError::new(ReadErrorCode::BadResponse, "block number out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};

    type MockHandler = dyn Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + Sync;

    struct MockState {
        calls: Mutex<Vec<(String, Value)>>,
        handler: Box<MockHandler>,
    }

    async fn spawn_mock(handler: Box<MockHandler>) -> (String, Arc<MockState>) {
        let state = Arc::new(MockState {
            calls: Mutex::new(Vec::new()),
            handler,
        });
        let app = Router::new()
            .route("/", post(mock_rpc))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), state)
    }

    async fn mock_rpc(State(state): State<Arc<MockState>>, Json(body): Json<Value>) -> Json<Value> {
        let method = body
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = body.get("params").cloned().unwrap_or(Value::Null);
        state
            .calls
            .lock()
            .unwrap()
            .push((method.clone(), params.clone()));
        let id = body.get("id").cloned().unwrap_or(json!(1));
        let reply = match (state.handler)(&method, &params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        Json(reply)
    }

    fn first(p: &Value) -> &str {
        p.get(0).and_then(Value::as_str).unwrap_or("")
    }

    fn hex_word(pair: &str) -> String {
        format!("0x{}", pair.repeat(32))
    }

    fn genesis_hash() -> String {
        hex_word("aa")
    }

    fn b16_hash() -> String {
        hex_word("bb")
    }

    fn b17_hash() -> String {
        hex_word("cc")
    }

    fn tx_hash() -> String {
        hex_word("dd")
    }

    fn addr() -> String {
        format!("0x{}", "ab".repeat(20))
    }

    const CHAIN_ID: &str = "0x539"; // 1337

    fn genesis_block() -> Value {
        json!({ "number": "0x0", "hash": genesis_hash(), "parentHash": hex_word("00"), "transactions": [] })
    }

    fn block16(full: bool) -> Value {
        let transactions = if full {
            json!([{ "hash": tx_hash(), "input": "0xdeadbeef", "blockNumber": "0x10" }])
        } else {
            json!([tx_hash()])
        };
        json!({ "number": "0x10", "hash": b16_hash(), "parentHash": genesis_hash(), "transactions": transactions })
    }

    fn block17(full: bool) -> Value {
        let transactions = if full {
            json!([{ "hash": tx_hash() }])
        } else {
            json!([tx_hash()])
        };
        json!({ "number": "0x11", "hash": b17_hash(), "parentHash": genesis_hash(), "transactions": transactions })
    }

    fn with_identity(
        rest: impl Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + Sync + 'static,
    ) -> Box<MockHandler> {
        Box::new(move |m: &str, p: &Value| match m {
            "eth_chainId" => Ok(json!(CHAIN_ID)),
            "eth_getBlockByNumber" if p == &json!(["0x0", false]) => Ok(genesis_block()),
            _ => rest(m, p),
        })
    }

    fn calls_of(mock: &MockState) -> Vec<(String, Value)> {
        mock.calls.lock().unwrap().clone()
    }

    #[test]
    fn tagged_requests_round_trip_and_reject_unknown_fields() {
        assert_eq!(
            serde_json::to_value(BlockRef::Number(16)).unwrap(),
            json!("0x10")
        );
        assert_eq!(
            serde_json::to_value(BlockRef::Latest).unwrap(),
            json!("latest")
        );
        assert_eq!(
            serde_json::from_value::<BlockRef>(json!(16)).unwrap(),
            BlockRef::Number(16)
        );
        assert_eq!(
            serde_json::from_value::<BlockRef>(json!("0x10")).unwrap(),
            BlockRef::Number(16)
        );

        let request = ReadRequest::Block {
            selector: BlockRef::Latest,
            full_tx: false,
        };
        let body = serde_json::to_value(request).unwrap();
        assert_eq!(
            body,
            json!({ "op": "block", "selector": "latest", "full_tx": false })
        );
        assert!(serde_json::from_value::<ReadRequest>(body).is_ok());

        let err = serde_json::from_value::<ReadRequest>(
            json!({ "op": "block", "selector": "latest", "full_tx": false, "bogus": true }),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "unexpected error: {err}"
        );

        let err = serde_json::from_value::<ReadRequest>(json!({ "op": "nonsense" })).unwrap_err();
        assert!(
            err.to_string().contains("unknown variant"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn block_by_fixed_number_sends_hex_quantity_and_pins_identity() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            let full = p.get(1).and_then(Value::as_bool).unwrap_or(false);
            match (m, first(p)) {
                ("eth_getBlockByNumber", "0x10") => Ok(block16(full)),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(
            &url,
            ReadRequest::Block {
                selector: BlockRef::parse("16").unwrap(),
                full_tx: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(out["chain_id"], json!("1337"));
        assert_eq!(out["genesis_block_hash"], json!(genesis_hash()));
        assert_eq!(out["block"]["number"], json!("0x10"));
        assert_eq!(out["block"]["hash"], json!(b16_hash()));
        assert_eq!(out["data"]["transactions"][0]["input"], json!("0xdeadbeef"));

        let calls = calls_of(&mock);
        assert_eq!(calls[0], ("eth_chainId".to_string(), json!([])));
        assert_eq!(
            calls[1],
            ("eth_getBlockByNumber".to_string(), json!(["0x0", false]))
        );
        assert_eq!(
            calls[2],
            ("eth_getBlockByNumber".to_string(), json!(["0x10", false]))
        );
        assert_eq!(
            calls[3],
            ("eth_getBlockByNumber".to_string(), json!(["0x10", true]))
        );
    }

    #[tokio::test]
    async fn explicit_latest_is_resolved_before_use() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            let full = p.get(1).and_then(Value::as_bool).unwrap_or(false);
            match (m, first(p)) {
                ("eth_getBlockByNumber", "latest") => Ok(block17(false)),
                ("eth_getBlockByNumber", "0x11") => Ok(block17(full)),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(
            &url,
            ReadRequest::Block {
                selector: BlockRef::Latest,
                full_tx: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(out["block"]["number"], json!("0x11"));
        assert_eq!(out["block"]["hash"], json!(b17_hash()));

        let calls = calls_of(&mock);
        assert_eq!(
            calls
                .iter()
                .filter(|(m, p)| m == "eth_getBlockByNumber" && first(p) == "latest")
                .count(),
            1
        );
        assert_eq!(
            calls[2],
            ("eth_getBlockByNumber".to_string(), json!(["latest", false]))
        );
        assert_eq!(
            calls[3],
            ("eth_getBlockByNumber".to_string(), json!(["0x11", false]))
        );
    }

    #[tokio::test]
    async fn receipt_rpc_error_is_surfaced_not_masked_as_pending() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            match (m, first(p)) {
                ("eth_getTransactionByHash", h) if h == tx_hash() => {
                    Ok(json!({ "hash": tx_hash(), "input": "0xdeadbeef", "blockNumber": "0x10", "blockHash":b16_hash() }))
                }
                ("eth_getTransactionReceipt", _) => {
                    Err((-32010, "receipt unavailable".to_string()))
                }
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let err = query(&url, ReadRequest::Transaction { hash: tx_hash() })
            .await
            .unwrap_err();

        assert_eq!(err.code, ReadErrorCode::RpcError);
        assert_ne!(err.code, ReadErrorCode::TxPending);
        assert_eq!(err.rpc_code, Some(-32010));
        assert!(err.message.contains("provider rejected"));
        assert!(!err.message.contains("receipt unavailable"));
        assert!(calls_of(&mock)
            .iter()
            .any(|(m, _)| m == "eth_getTransactionReceipt"));
    }

    #[tokio::test]
    async fn call_revert_requires_typed_rpc_code_not_provider_text() {
        let (url, _mock) = spawn_mock(with_identity(|method: &str, params: &Value| match method {
            "eth_getBlockByNumber" if params == &json!(["0x10", false]) => Ok(block16(false)),
            "eth_call" => Err((3, "execution reverted: private data".into())),
            _ => Err((-32601, "unexpected method".into())),
        }))
        .await;
        let call = CallRequest {
            to: Some(addr()),
            data: Some("0x1234".into()),
            ..Default::default()
        };
        let error = query(
            &url,
            ReadRequest::Simulate {
                call,
                block: BlockRef::Number(16),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ReadErrorCode::CallReverted);
        assert!(!error.message.contains("private data"));
    }

    #[tokio::test]
    async fn transaction_returns_tx_and_receipt_with_logs() {
        let (url, _mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            match (m, first(p)) {
                ("eth_getTransactionByHash", h) if h == tx_hash() => {
                    Ok(json!({ "hash": tx_hash(), "input": "0xdeadbeef", "blockNumber": "0x10", "blockHash":b16_hash() }))
                }
                ("eth_getTransactionReceipt", _) => Ok(json!({
                    "transactionHash":tx_hash(),
                    "status": "0x1",
                    "blockNumber": "0x10",
                    "blockHash": b16_hash(),
                    "logs": [{ "address": addr(), "topics": ["0x01"] }],
                })),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(&url, ReadRequest::Transaction { hash: tx_hash() })
            .await
            .unwrap();

        assert_eq!(out["data"]["transaction"]["input"], json!("0xdeadbeef"));
        assert_eq!(out["data"]["receipt"]["logs"].as_array().unwrap().len(), 1);
        assert_eq!(out["block"]["hash"], json!(b16_hash()));
    }

    #[tokio::test]
    async fn pending_tx_is_reported_only_from_null_block_number() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            match (m, first(p)) {
                ("eth_getTransactionByHash", h) if h == tx_hash() => {
                    Ok(json!({ "hash": tx_hash(), "input": "0x", "blockNumber":null }))
                }
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let err = query(&url, ReadRequest::Transaction { hash: tx_hash() })
            .await
            .unwrap_err();

        assert_eq!(err.code, ReadErrorCode::TxPending);
        assert!(!calls_of(&mock)
            .iter()
            .any(|(m, _)| m == "eth_getTransactionReceipt"));
    }

    #[tokio::test]
    async fn mined_tx_without_receipt_is_incomplete_not_pending() {
        let (url, _mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            match (m, first(p)) {
                ("eth_getTransactionByHash", h) if h == tx_hash() => {
                    Ok(json!({ "hash": tx_hash(), "blockNumber": "0x10" }))
                }
                ("eth_getTransactionReceipt", _) => Ok(json!(null)),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let err = query(&url, ReadRequest::Transaction { hash: tx_hash() })
            .await
            .unwrap_err();

        assert_eq!(err.code, ReadErrorCode::Incomplete);
        assert_ne!(err.code, ReadErrorCode::TxPending);
    }

    #[tokio::test]
    async fn send_methods_are_rejected_before_any_request() {
        let (url, mock) = spawn_mock(Box::new(|_m: &str, _p: &Value| Ok(json!(null)))).await;
        let http = build_client().unwrap();

        for method in [
            "eth_sendRawTransaction",
            "eth_sendTransaction",
            "eth_signTransaction",
            "eth_sign",
            "personal_sign",
        ] {
            let err = rpc_call(&http, &url, method, json!([{}]))
                .await
                .unwrap_err();
            assert_eq!(
                err.code,
                ReadErrorCode::MethodNotAllowed,
                "method: {method}"
            );
        }
        assert!(calls_of(&mock).is_empty());
        for m in ALLOWED_METHODS {
            assert!(!m.contains("send") && !m.contains("sign") && !m.contains("personal"));
        }
    }

    #[tokio::test]
    async fn invalid_params_fail_before_any_rpc() {
        let (url, mock) =
            spawn_mock(Box::new(|_m: &str, _p: &Value| panic!("no RPC expected"))).await;

        let cases = [
            ReadRequest::History {
                from_block: BlockRef::Number(1),
                count: 21,
                full_tx: false,
            },
            ReadRequest::History {
                from_block: BlockRef::Number(1),
                count: 0,
                full_tx: false,
            },
            ReadRequest::Account {
                address: "0x1234".to_string(),
                block: BlockRef::Latest,
            },
            ReadRequest::Transaction {
                hash: "not-a-hash".to_string(),
            },
            ReadRequest::Storage {
                address: addr(),
                slot: "zz".to_string(),
                block: BlockRef::Number(1),
            },
            ReadRequest::Block {
                selector: BlockRef::Hash("0x22".to_string()),
                full_tx: false,
            },
        ];
        for request in cases {
            let err = query(&url, request).await.unwrap_err();
            assert_eq!(err.code, ReadErrorCode::InvalidInput);
        }
        for url in [
            "ftp://node",
            "http://user:pass@node",
            "http://node/?apikey=x",
        ] {
            let err = query(
                url,
                ReadRequest::Block {
                    selector: BlockRef::Latest,
                    full_tx: false,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, ReadErrorCode::InvalidUrl);
        }
        assert!(calls_of(&mock).is_empty());
    }

    #[tokio::test]
    async fn storage_and_simulate_send_canonical_hex_params() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            let n = first(p);
            match (m, n) {
                ("eth_getBlockByNumber", "0x10") => Ok(block16(false)),
                ("eth_getStorageAt", _) => Ok(json!(format!("0x{}", "42".repeat(32)))),
                ("eth_call", _) => Ok(json!("0x6080")),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(
            &url,
            ReadRequest::Storage {
                address: addr(),
                slot: "42".to_string(),
                block: BlockRef::Number(16),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            out["data"]["value"],
            json!(format!("0x{}", "42".repeat(32)))
        );
        assert_eq!(out["block"]["number"], json!("0x10"));

        let call = CallRequest {
            to: Some(addr()),
            data: Some("0xdeadbeef".to_string()),
            value: Some("1000000000000000".to_string()),
            gas: Some("21000".to_string()),
            ..Default::default()
        };
        let out = query(
            &url,
            ReadRequest::Simulate {
                call,
                block: BlockRef::Number(16),
            },
        )
        .await
        .unwrap();
        assert_eq!(out["data"]["return_data"], json!("0x6080"));

        let calls = calls_of(&mock);
        let storage = calls.iter().find(|(m, _)| m == "eth_getStorageAt").unwrap();
        assert_eq!(storage.1, json!([addr(), "0x2a", "0x10"]));
        let simulated = calls.iter().find(|(m, _)| m == "eth_call").unwrap();
        assert_eq!(simulated.1[0]["to"], json!(addr()));
        assert_eq!(simulated.1[0]["data"], json!("0xdeadbeef"));
        assert_eq!(simulated.1[0]["gas"], json!("0x5208"));
        assert_eq!(simulated.1[0]["value"], json!("0x38d7ea4c68000"));
        assert_eq!(simulated.1[1], json!("0x10"));
    }

    #[tokio::test]
    async fn account_reads_pinned_fields() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, p: &Value| {
            match (m, first(p)) {
                ("eth_getBlockByNumber", "0x10") => Ok(block16(false)),
                ("eth_getBalance", a) if *a == addr() => Ok(json!("0xde0b6b3a7640000")),
                ("eth_getTransactionCount", a) if *a == addr() => Ok(json!("0x2a")),
                ("eth_getCode", a) if *a == addr() => Ok(json!("0x6080")),
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(
            &url,
            ReadRequest::Account {
                address: addr(),
                block: BlockRef::Number(16),
            },
        )
        .await
        .unwrap();

        assert_eq!(out["data"]["balance"], json!("0xde0b6b3a7640000"));
        assert_eq!(out["data"]["nonce"], json!("0x2a"));
        assert_eq!(out["data"]["code"], json!("0x6080"));
        assert_eq!(out["block"]["number"], json!("0x10"));

        let calls = calls_of(&mock);
        for method in ["eth_getBalance", "eth_getTransactionCount", "eth_getCode"] {
            let call = calls.iter().find(|(m, _)| m == method).unwrap();
            assert_eq!(call.1, json!([addr(), "0x10"]));
        }
    }

    #[tokio::test]
    async fn history_page_is_bounded_and_pinned() {
        let (url, mock) = spawn_mock(with_identity(Box::new(|m: &str, _p: &Value| {
            match (m, first(_p)) {
                ("eth_getBlockByNumber", "0xe") => {
                    Ok(json!({ "number": "0xe", "hash": hex_word("ee"), "parentHash":genesis_hash(), "transactions": [] }))
                }
                ("eth_getBlockByNumber", "0xf") => {
                    Ok(json!({ "number": "0xf", "hash": hex_word("ff"), "parentHash":hex_word("ee"), "transactions": [] }))
                }
                _ => Err((-32601, "unexpected method".to_string())),
            }
        })))
        .await;

        let out = query(
            &url,
            ReadRequest::History {
                from_block: BlockRef::Number(14),
                count: 2,
                full_tx: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(out["block"]["number"], json!("0xe"));
        assert_eq!(out["data"]["blocks"].as_array().unwrap().len(), 2);

        let calls = calls_of(&mock);
        let page: Vec<_> = calls
            .iter()
            .filter(|(m, _)| m == "eth_getBlockByNumber")
            .collect();
        assert_eq!(page[1].1, json!(["0xe", false]));
        assert_eq!(page[2].1, json!(["0xe", true]));
        assert_eq!(page[3].1, json!(["0xf", true]));
    }

    #[tokio::test]
    async fn changed_network_identity_after_read_is_rejected() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let seen = Arc::new(AtomicUsize::new(0));
        let count = seen.clone();
        let (url, _) = spawn_mock(Box::new(move |method: &str, params: &Value| {
            match (method, first(params)) {
                ("eth_chainId", _) => Ok(json!(if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    "0x1"
                } else {
                    "0x2"
                })),
                ("eth_getBlockByNumber", "0x0") => Ok(genesis_block()),
                ("eth_getBlockByNumber", "0x10") => Ok(block16(false)),
                _ => Err((-32601, "unexpected".into())),
            }
        }))
        .await;
        let error = query(
            &url,
            ReadRequest::Block {
                selector: BlockRef::Number(16),
                full_tx: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ReadErrorCode::Reorg);
    }

    #[tokio::test]
    async fn missing_transaction_block_number_is_malformed_not_pending() {
        let (url, _) = spawn_mock(with_identity(|method: &str, _: &Value| match method {
            "eth_getTransactionByHash" => Ok(json!({"hash":tx_hash()})),
            _ => Err((-32601, "unexpected".into())),
        }))
        .await;
        let error = query(&url, ReadRequest::Transaction { hash: tx_hash() })
            .await
            .unwrap_err();
        assert_eq!(error.code, ReadErrorCode::BadResponse);
    }

    #[tokio::test]
    async fn pin_change_during_state_read_is_a_reorg_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let (url, mock) = spawn_mock(Box::new(move |m: &str, p: &Value| match (m, first(p)) {
            ("eth_chainId", _) => Ok(json!(CHAIN_ID)),
            ("eth_getBlockByNumber", "0x0") => Ok(genesis_block()),
            ("eth_getBlockByNumber", "0x10") => {
                let hit = counter_clone.fetch_add(1, Ordering::SeqCst);
                let hash = if hit == 0 { b16_hash() } else { b17_hash() };
                Ok(json!({ "number": "0x10", "hash": hash, "transactions": [] }))
            }
            ("eth_getBalance", _) => Ok(json!("0x1")),
            ("eth_getTransactionCount", _) => Ok(json!("0x1")),
            ("eth_getCode", _) => Ok(json!("0x")),
            _ => Err((-32601, "unexpected method".to_string())),
        }))
        .await;

        let err = query(
            &url,
            ReadRequest::Account {
                address: addr(),
                block: BlockRef::Number(16),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, ReadErrorCode::Reorg);
        // resolve saw the original hash, the post-read verify saw the swapped one
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert!(calls_of(&mock).iter().any(|(m, _)| m == "eth_getBalance"));
    }
}
