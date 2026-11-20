use reqwest::{blocking::Client, redirect::Policy};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::transaction::{AccessList, AccessListItem},
    database_interface::{DBErrorMarker, DatabaseRef},
    primitives::{Address, B256, Bytes, TxKind, U256},
    state::{AccountInfo, Bytecode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const RPC_TIMEOUT: Duration = Duration::from_secs(2);
const ANCESTRY_BUDGET: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkAnchor {
    pub chain_id: u64,
    pub number: u64,
    pub hash: String,
    pub genesis_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkBlock {
    pub number: u64,
    pub timestamp: u64,
    pub beneficiary: String,
    pub gas_limit: u64,
    pub base_fee: String,
    pub prevrandao: String,
    pub excess_blob_gas: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    pub rpc_url: Option<String>,
    pub anchor: ForkAnchor,
    pub block: ForkBlock,
    pub strict: bool,
    pub witness: Option<ForkWitness>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessAccount {
    pub balance: String,
    pub nonce: u64,
    pub code: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessHeader {
    pub number: u64,
    pub parent_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkWitness {
    pub version: u32,
    pub anchor: ForkAnchor,
    pub accounts: BTreeMap<String, WitnessAccount>,
    pub storage: BTreeMap<String, BTreeMap<String, String>>,
    pub block_hashes: BTreeMap<String, String>,
    pub headers: BTreeMap<String, WitnessHeader>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkError(pub String);
impl fmt::Display for ForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ForkError {}
impl DBErrorMarker for ForkError {}
fn error(message: &str) -> ForkError {
    ForkError(message.to_owned())
}

pub fn quantity(value: &str) -> Result<U256, ForkError> {
    let digits = value
        .strip_prefix("0x")
        .ok_or_else(|| error("expected hex quantity"))?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(error("invalid hex quantity"));
    }
    U256::from_str_radix(digits, 16).map_err(|_| error("hex quantity exceeds 256 bits"))
}
fn u64_quantity(value: &str) -> Result<u64, ForkError> {
    quantity(value)?
        .try_into()
        .map_err(|_| error("quantity exceeds 64 bits"))
}
fn u128_quantity(value: &str) -> Result<u128, ForkError> {
    quantity(value)?
        .try_into()
        .map_err(|_| error("quantity exceeds 128 bits"))
}
pub fn hex_bytes(value: &str) -> Result<Bytes, ForkError> {
    let digits = value
        .strip_prefix("0x")
        .ok_or_else(|| error("expected hex data"))?;
    if digits.len() % 2 != 0 || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(error("invalid hex data"));
    }
    let bytes = digits
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |c: u8| {
                if c <= b'9' {
                    c - b'0'
                } else {
                    c.to_ascii_lowercase() - b'a' + 10
                }
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect::<Vec<_>>();
    Ok(bytes.into())
}
fn hash(value: &str) -> Result<B256, ForkError> {
    let bytes = hex_bytes(value)?;
    if bytes.len() != 32 {
        return Err(error("expected 32-byte hash"));
    }
    Ok(B256::from_slice(&bytes))
}
fn address(value: &str) -> Result<Address, ForkError> {
    let bytes = hex_bytes(value)?;
    if bytes.len() != 20 {
        return Err(error("expected 20-byte address"));
    }
    Ok(Address::from_slice(&bytes))
}
fn account_info(account: &WitnessAccount) -> Result<AccountInfo, ForkError> {
    let code = Bytecode::new_raw(hex_bytes(&account.code)?);
    Ok(AccountInfo {
        balance: quantity(&account.balance)?,
        nonce: account.nonce,
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    })
}

impl ForkConfig {
    pub fn from_json(input: &str) -> Result<Self, ForkError> {
        let config: Self =
            serde_json::from_str(input).map_err(|_| error("invalid fork configuration"))?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<(), ForkError> {
        if !self.strict || self.anchor.chain_id != 1 {
            return Err(error("fork requires strict Ethereum chain_id 1"));
        }
        hash(&self.anchor.hash)?;
        if hash(&self.anchor.genesis_hash)?
            != hash("0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3")?
        {
            return Err(error("fork requires Ethereum mainnet genesis"));
        }
        if !(1_710_338_135..1_746_612_311).contains(&self.block.timestamp) {
            return Err(error("fork supports only the Ethereum Cancun interval"));
        }
        if self.block.number < self.anchor.number || self.block.number - self.anchor.number > 1 {
            return Err(error("block must be anchor or its immediate child"));
        }
        self.block_env()?;
        if let Some(witness) = &self.witness {
            witness.validate(&self.anchor)?;
        }
        if let Some(url) = &self.rpc_url {
            let url = reqwest::Url::parse(url).map_err(|_| error("invalid fork RPC URL"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || url.fragment().is_some()
            {
                return Err(error("fork RPC requires an HTTP(S) URL without fragment"));
            }
        }
        Ok(())
    }
    pub fn block_env(&self) -> Result<BlockEnv, ForkError> {
        let mut block = BlockEnv {
            number: U256::from(self.block.number),
            timestamp: U256::from(self.block.timestamp),
            beneficiary: address(&self.block.beneficiary)?,
            gas_limit: self.block.gas_limit,
            basefee: u64_quantity(&self.block.base_fee)?,
            prevrandao: Some(hash(&self.block.prevrandao)?),
            difficulty: U256::ZERO,
            ..Default::default()
        };
        block.set_blob_excess_gas_and_price(self.block.excess_blob_gas, 3_338_477);
        Ok(block)
    }
}

impl ForkWitness {
    fn validate(&self, anchor: &ForkAnchor) -> Result<(), ForkError> {
        if self.version != 1 || &self.anchor != anchor {
            return Err(error("witness version or anchor mismatch"));
        }
        for (addr, account) in &self.accounts {
            if format!("{:#x}", address(addr)?) != *addr {
                return Err(error("noncanonical witness address"));
            }
            account_info(account)?;
        }
        for (addr, slots) in &self.storage {
            if format!("{:#x}", address(addr)?) != *addr {
                return Err(error("noncanonical witness address"));
            }
            for (key, value) in slots {
                quantity(key)?;
                quantity(value)?;
            }
        }
        for (number, value) in &self.block_hashes {
            let number = number
                .parse::<u64>()
                .map_err(|_| error("invalid witness block number"))?;
            if number > anchor.number || anchor.number - number > 256 {
                return Err(error("witness block hash outside anchored window"));
            }
            hash(value)?;
        }
        for (key, header) in &self.headers {
            hash(key)?;
            hash(&header.parent_hash)?;
            if header.number == 0
                || header.number > anchor.number
                || anchor.number - header.number >= 256
            {
                return Err(error("witness header outside anchored window"));
            }
            let current = if header.number == anchor.number {
                Some(&anchor.hash)
            } else {
                self.block_hashes.get(&header.number.to_string())
            };
            if current != Some(key)
                || self.block_hashes.get(&(header.number - 1).to_string())
                    != Some(&header.parent_hash)
            {
                return Err(error("orphan or inconsistent witness header"));
            }
        }
        if self
            .block_hashes
            .get(&anchor.number.to_string())
            .is_some_and(|value| value != &anchor.hash)
        {
            return Err(error("witness anchor hash mismatch"));
        }
        // Every historical hash must have an unbroken path back to the pinned anchor.
        for (number, value) in &self.block_hashes {
            let target = number
                .parse::<u64>()
                .map_err(|_| error("invalid witness block number"))?;
            let mut cursor = anchor.hash.clone();
            for expected in (target + 1..=anchor.number).rev() {
                let header = self
                    .headers
                    .get(&cursor)
                    .ok_or_else(|| error("missing witness ancestry"))?;
                if header.number != expected {
                    return Err(error("witness ancestry number mismatch"));
                }
                cursor = header.parent_hash.clone();
            }
            if &cursor != value {
                return Err(error("witness ancestry hash mismatch"));
            }
        }
        Ok(())
    }
}

struct Remote {
    endpoint: Option<String>,
    client: Option<Client>,
    witness: Mutex<ForkWitness>,
    next_id: AtomicU64,
}

#[derive(Clone, Default)]
pub struct RemoteDb {
    remote: Option<Arc<Remote>>,
    deadline: Option<Instant>,
}
impl fmt::Debug for RemoteDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.remote.is_some() {
            "RemoteDb(pinned)"
        } else {
            "RemoteDb(local)"
        })
    }
}
impl RemoteDb {
    pub fn new(config: &ForkConfig) -> Result<Self, ForkError> {
        config.validate()?;
        let mut witness = config.witness.clone().unwrap_or_else(|| ForkWitness {
            version: 1,
            anchor: config.anchor.clone(),
            accounts: BTreeMap::new(),
            storage: BTreeMap::new(),
            block_hashes: BTreeMap::new(),
            headers: BTreeMap::new(),
        });
        witness
            .block_hashes
            .insert(config.anchor.number.to_string(), config.anchor.hash.clone());
        let client = config
            .rpc_url
            .as_ref()
            .map(|_| {
                Client::builder()
                    .redirect(Policy::none())
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(1))
                    .timeout(RPC_TIMEOUT)
                    .build()
                    .map_err(|_| error("could not initialize fork RPC client"))
            })
            .transpose()?;
        Ok(Self {
            remote: Some(Arc::new(Remote {
                endpoint: config.rpc_url.clone(),
                client,
                witness: Mutex::new(witness),
                next_id: AtomicU64::new(1),
            })),
            deadline: None,
        })
    }
    pub(crate) fn limit_reads_until(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }
    pub(crate) fn clear_read_limit(&mut self) {
        self.deadline = None;
    }
    fn check_budget(&self) -> Result<(), ForkError> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(error("unsupported_evaluation: remote read budget exceeded"));
        }
        Ok(())
    }
    pub fn export(&self) -> Result<String, ForkError> {
        let remote = self
            .remote
            .as_ref()
            .ok_or_else(|| error("not a fork session"))?;
        serde_json::to_string(
            &*remote
                .witness
                .lock()
                .map_err(|_| error("fork witness lock unavailable"))?,
        )
        .map_err(|_| error("could not serialize fork witness"))
    }
    pub fn is_fork(&self) -> bool {
        self.remote.is_some()
    }
}
impl Remote {
    fn rpc(
        &self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
    ) -> Result<Value, ForkError> {
        let timeout = deadline
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(RPC_TIMEOUT)
            })
            .unwrap_or(RPC_TIMEOUT);
        if timeout.is_zero() {
            return Err(error("unsupported_evaluation: remote read budget exceeded"));
        }
        if !matches!(
            method,
            "eth_getBalance"
                | "eth_getTransactionCount"
                | "eth_getCode"
                | "eth_getStorageAt"
                | "eth_getBlockByHash"
        ) {
            return Err(error("RPC method is not read-only allowlisted"));
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| error("missing offline witness data"))?;
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or_else(|| error("missing offline witness data"))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = client
            .post(endpoint)
            .timeout(timeout)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .map_err(|_| error("fork RPC transport failed"))?;
        if !response.status().is_success() {
            return Err(error("fork RPC HTTP status failure"));
        }
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|_| error("fork RPC response read failed"))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(error("fork RPC response exceeds size limit"));
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| error("fork RPC returned invalid JSON"))?;
        let object = value
            .as_object()
            .ok_or_else(|| error("fork RPC response must be an object"))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.get("id").and_then(Value::as_u64) != Some(id)
        {
            return Err(error("fork RPC response identity mismatch"));
        }
        if object.contains_key("error") {
            return Err(error("fork RPC provider error"));
        }
        object
            .get("result")
            .filter(|v| !v.is_null())
            .cloned()
            .ok_or_else(|| error("fork RPC missing result"))
    }
    fn selector(witness: &ForkWitness) -> Value {
        json!({"blockHash":witness.anchor.hash,"requireCanonical":true})
    }
    fn string_rpc(
        &self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
    ) -> Result<String, ForkError> {
        self.rpc(method, params, deadline)?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| error("fork RPC result must be a string"))
    }
}
impl DatabaseRef for RemoteDb {
    type Error = ForkError;
    fn basic_ref(&self, addr: Address) -> Result<Option<AccountInfo>, ForkError> {
        self.check_budget()?;
        let Some(remote) = &self.remote else {
            return Ok(None);
        };
        let key = format!("{addr:#x}");
        let mut witness = remote
            .witness
            .lock()
            .map_err(|_| error("fork witness lock unavailable"))?;
        if let Some(account) = witness.accounts.get(&key) {
            return account_info(account).map(Some);
        }
        let selector = Remote::selector(&witness);
        let balance = remote.string_rpc("eth_getBalance", json!([key, selector]), self.deadline)?;
        quantity(&balance)?;
        let nonce = u64_quantity(&remote.string_rpc(
            "eth_getTransactionCount",
            json!([key, selector]),
            self.deadline,
        )?)?;
        let code = remote.string_rpc("eth_getCode", json!([key, selector]), self.deadline)?;
        let account = WitnessAccount {
            balance,
            nonce,
            code,
        };
        let info = account_info(&account)?;
        witness.accounts.insert(key, account);
        Ok(Some(info))
    }
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, ForkError> {
        if code_hash == revm::primitives::KECCAK_EMPTY || code_hash == B256::ZERO {
            return Ok(Bytecode::default());
        }
        if let Some(remote) = &self.remote {
            let witness = remote
                .witness
                .lock()
                .map_err(|_| error("fork witness lock unavailable"))?;
            for account in witness.accounts.values() {
                let info = account_info(account)?;
                if info.code_hash == code_hash {
                    return Ok(info.code.expect("account_info includes code"));
                }
            }
        }
        Err(error("missing witness code hash"))
    }
    fn storage_ref(&self, addr: Address, index: U256) -> Result<U256, ForkError> {
        self.check_budget()?;
        let Some(remote) = &self.remote else {
            return Ok(U256::ZERO);
        };
        let key = format!("{addr:#x}");
        let slot = format!("0x{index:x}");
        let mut witness = remote
            .witness
            .lock()
            .map_err(|_| error("fork witness lock unavailable"))?;
        if let Some(value) = witness.storage.get(&key).and_then(|slots| slots.get(&slot)) {
            return quantity(value);
        }
        let result = remote.string_rpc(
            "eth_getStorageAt",
            json!([key, slot, Remote::selector(&witness)]),
            self.deadline,
        )?;
        let bytes = hex_bytes(&result)?;
        if bytes.len() != 32 {
            return Err(error("fork RPC storage result must contain 32 bytes"));
        }
        let value = U256::from_be_slice(&bytes);
        witness
            .storage
            .entry(key)
            .or_default()
            .insert(slot, format!("0x{value:x}"));
        Ok(value)
    }
    fn block_hash_ref(&self, number: u64) -> Result<B256, ForkError> {
        self.check_budget()?;
        let Some(remote) = &self.remote else {
            return Ok(B256::ZERO);
        };
        let mut witness = remote
            .witness
            .lock()
            .map_err(|_| error("fork witness lock unavailable"))?;
        if number > witness.anchor.number || witness.anchor.number - number > 256 {
            return Err(error("block hash outside anchored window"));
        }
        if let Some(value) = witness.block_hashes.get(&number.to_string()) {
            return hash(value);
        }
        let started = Instant::now();
        let mut cursor = witness.anchor.hash.clone();
        let anchor_number = witness.anchor.number;
        for expected in (number + 1..=anchor_number).rev() {
            if started.elapsed() >= ANCESTRY_BUDGET {
                return Err(error("fork ancestry read budget exceeded"));
            }
            let header = if let Some(header) = witness.headers.get(&cursor) {
                header.clone()
            } else {
                let value =
                    remote.rpc("eth_getBlockByHash", json!([cursor, false]), self.deadline)?;
                let object = value
                    .as_object()
                    .ok_or_else(|| error("fork RPC block result must be an object"))?;
                let text = |name| {
                    object
                        .get(name)
                        .and_then(Value::as_str)
                        .ok_or_else(|| error("fork RPC block header field missing"))
                };
                if hash(text("hash")?)? != hash(&cursor)? {
                    return Err(error("fork RPC block hash mismatch"));
                }
                let header = WitnessHeader {
                    number: u64_quantity(text("number")?)?,
                    parent_hash: format!("{:#x}", hash(text("parentHash")?)?),
                };
                if header.number != expected {
                    return Err(error("fork RPC ancestry number mismatch"));
                }
                witness.headers.insert(cursor.clone(), header.clone());
                header
            };
            if header.number != expected {
                return Err(error("fork ancestry number mismatch"));
            }
            cursor = header.parent_hash;
            witness
                .block_hashes
                .insert((expected - 1).to_string(), cursor.clone());
        }
        hash(&cursor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionJson {
    caller: String,
    to: Option<String>,
    data: String,
    value: String,
    gas_limit: u64,
    nonce: u64,
    chain_id: u64,
    tx_type: u8,
    gas_price: String,
    max_priority_fee_per_gas: Option<String>,
    access_list: Vec<AccessJson>,
    blob_hashes: Vec<String>,
    max_fee_per_blob_gas: Option<String>,
    commit: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessJson {
    address: String,
    storage_keys: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speculative_read_deadline_is_clone_local_and_clearable() {
        let base = RemoteDb::default();
        let mut speculative = base.clone();
        speculative.limit_reads_until(Instant::now() - Duration::from_millis(1));
        assert!(
            speculative
                .basic_ref(Address::ZERO)
                .unwrap_err()
                .to_string()
                .contains("remote read budget")
        );
        assert!(speculative.storage_ref(Address::ZERO, U256::ZERO).is_err());
        assert!(speculative.block_hash_ref(0).is_err());
        assert!(base.basic_ref(Address::ZERO).unwrap().is_none());
        speculative.clear_read_limit();
        assert!(speculative.basic_ref(Address::ZERO).unwrap().is_none());
    }
}

pub fn transaction_from_json(input: &str) -> Result<(TxEnv, bool), ForkError> {
    let request: TransactionJson =
        serde_json::from_str(input).map_err(|_| error("invalid transaction envelope"))?;
    if request.chain_id != 1 || request.tx_type > 3 {
        return Err(error(
            "only Ethereum transaction types 0/1/2/3 are supported",
        ));
    }
    if request.tx_type == 0 && !request.access_list.is_empty() {
        return Err(error("legacy transaction cannot carry access list"));
    }
    if request.tx_type < 2 && request.max_priority_fee_per_gas.is_some() {
        return Err(error(
            "legacy/access-list transaction cannot carry priority fee",
        ));
    }
    if request.tx_type >= 2 && request.max_priority_fee_per_gas.is_none() {
        return Err(error("dynamic fee transaction requires priority fee"));
    }
    if request.tx_type != 3
        && (!request.blob_hashes.is_empty() || request.max_fee_per_blob_gas.is_some())
    {
        return Err(error("non-blob transaction cannot carry blob fields"));
    }
    if request.tx_type == 3
        && (request.to.is_none()
            || request.blob_hashes.is_empty()
            || request.max_fee_per_blob_gas.is_none())
    {
        return Err(error(
            "blob transaction requires destination, hashes and blob fee",
        ));
    }
    let access_list = request
        .access_list
        .into_iter()
        .map(|item| {
            Ok(AccessListItem {
                address: address(&item.address)?,
                storage_keys: item
                    .storage_keys
                    .iter()
                    .map(|key| hash(key))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, ForkError>>()?;
    let tx = TxEnv {
        tx_type: request.tx_type,
        caller: address(&request.caller)?,
        kind: request
            .to
            .as_deref()
            .map(address)
            .transpose()?
            .map_or(TxKind::Create, TxKind::Call),
        data: hex_bytes(&request.data)?,
        value: quantity(&request.value)?,
        gas_limit: request.gas_limit,
        nonce: request.nonce,
        chain_id: Some(request.chain_id),
        gas_price: u128_quantity(&request.gas_price)?,
        gas_priority_fee: request
            .max_priority_fee_per_gas
            .as_deref()
            .map(u128_quantity)
            .transpose()?,
        access_list: AccessList(access_list),
        blob_hashes: request
            .blob_hashes
            .iter()
            .map(|h| hash(h))
            .collect::<Result<_, _>>()?,
        max_fee_per_blob_gas: request
            .max_fee_per_blob_gas
            .as_deref()
            .map(u128_quantity)
            .transpose()?
            .unwrap_or(0),
        ..Default::default()
    };
    Ok((tx, request.commit))
}
