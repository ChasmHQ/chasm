//! Named storage view for a deployed contract: every state variable from the
//! compiler's storage layout, read at one pinned block and decoded by type.
//! Mappings need a key, so they are listed with their slot and a hint; the
//! raw slot reader stays available below the list for anything else.
use ethers::types::{H256, U256};
use ethers::utils::keccak256;
use serde_json::Value;

/// Dynamic arrays show at most this many elements.
const ARRAY_PREVIEW: u64 = 16;
/// Upper bound on slot reads for one refresh.
const MAX_READS: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub label: String,
    pub type_label: String,
    pub slot: String,
    pub offset: u64,
    pub value: String,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    label: String,
    slot: U256,
    offset: u64,
    type_id: String,
}

#[derive(Clone, Debug)]
pub struct Layout {
    entries: Vec<Entry>,
    types: serde_json::Map<String, Value>,
}

impl Layout {
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let storage = value["storage"].as_array().ok_or("The contract has no storage layout.")?;
        let entries = storage
            .iter()
            .map(|item| {
                Ok(Entry {
                    label: item["label"].as_str().unwrap_or("?").to_string(),
                    slot: U256::from_dec_str(item["slot"].as_str().unwrap_or("0")).map_err(|_| "Invalid slot in storage layout.")?,
                    offset: item["offset"].as_u64().unwrap_or(0),
                    type_id: item["type"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self { entries, types: value["types"].as_object().cloned().unwrap_or_default() })
    }

    fn type_info(&self, id: &str) -> &Value {
        self.types.get(id).unwrap_or(&Value::Null)
    }

    fn bytes(&self, id: &str) -> u64 {
        self.type_info(id)["numberOfBytes"].as_str().and_then(|text| text.parse().ok()).unwrap_or(32)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Slots to fetch in a first pass: each variable's base slot.
    pub fn base_slots(&self) -> Vec<U256> {
        let mut slots: Vec<U256> = self.entries.iter().map(|entry| entry.slot).collect();
        slots.dedup();
        slots.truncate(MAX_READS);
        slots
    }

    /// Element slots of dynamic arrays, known once their lengths are read.
    pub fn array_slots(&self, read: &dyn Fn(U256) -> Option<H256>) -> Vec<U256> {
        let mut slots = Vec::new();
        for entry in &self.entries {
            let info = self.type_info(&entry.type_id);
            if info["encoding"] != "dynamic_array" {
                continue;
            }
            let Some(length) = read(entry.slot) else { continue };
            let length = U256::from_big_endian(length.as_bytes()).min(U256::from(ARRAY_PREVIEW)).as_u64();
            let base = U256::from_big_endian(&keccak256(word(entry.slot)));
            let base_type = info["base"].as_str().unwrap_or("");
            let size = self.bytes(base_type).max(1);
            let per_slot = if size >= 32 { 1 } else { 32 / size };
            let slots_used = if size >= 32 { length * size.div_ceil(32) } else { length.div_ceil(per_slot) };
            for index in 0..slots_used {
                if slots.len() >= MAX_READS {
                    return slots;
                }
                slots.push(base + index);
            }
        }
        slots
    }

    pub fn decode(&self, read: &dyn Fn(U256) -> Option<H256>) -> Vec<Variable> {
        self.entries
            .iter()
            .map(|entry| {
                let info = self.type_info(&entry.type_id);
                let type_label = info["label"].as_str().unwrap_or(&entry.type_id).to_string();
                let encoding = info["encoding"].as_str().unwrap_or("inplace");
                let (value, note) = match encoding {
                    "mapping" => (
                        String::new(),
                        "Mapping: enter a key below to read one entry.".into(),
                    ),
                    "dynamic_array" => self.dynamic_array(entry, info, read),
                    "bytes" => match read(entry.slot) {
                        Some(word) => short_bytes(word),
                        None => (String::new(), "Not read.".into()),
                    },
                    _ => match read(entry.slot) {
                        Some(word) => {
                            let size = self.bytes(&entry.type_id);
                            if size > 32 || info["members"].is_array() {
                                (format!("0x{}", hex(word.as_bytes())), format!("{size}-byte value starting at this slot; first slot shown."))
                            } else {
                                (scalar(&type_label, word, entry.offset, size), String::new())
                            }
                        }
                        None => (String::new(), "Not read.".into()),
                    },
                };
                Variable {
                    label: entry.label.clone(),
                    type_label,
                    slot: entry.slot.to_string(),
                    offset: entry.offset,
                    value,
                    note,
                }
            })
            .collect()
    }

    fn dynamic_array(&self, entry: &Entry, info: &Value, read: &dyn Fn(U256) -> Option<H256>) -> (String, String) {
        let Some(length) = read(entry.slot) else { return (String::new(), "Not read.".into()) };
        let length = U256::from_big_endian(length.as_bytes());
        let base_type = info["base"].as_str().unwrap_or("");
        let base_label = self.type_info(base_type)["label"].as_str().unwrap_or(base_type).to_string();
        let size = self.bytes(base_type).max(1);
        let base = U256::from_big_endian(&keccak256(word(entry.slot)));
        let shown = length.min(U256::from(ARRAY_PREVIEW)).as_u64();
        let mut items = Vec::new();
        for index in 0..shown {
            let (slot, offset) = if size >= 32 {
                (base + index * size.div_ceil(32), 0)
            } else {
                let per_slot = 32 / size;
                (base + index / per_slot, (index % per_slot) * size)
            };
            items.push(match read(slot) {
                Some(word) if size <= 32 => scalar(&base_label, word, offset, size),
                Some(word) => format!("0x{}", hex(word.as_bytes())),
                None => "?".into(),
            });
        }
        let more = if length > U256::from(shown) { format!(", … {} more", length - shown) } else { String::new() };
        (format!("[{}{more}]", items.join(", ")), format!("length {length}"))
    }

    /// Slot holding `mapping[key]` for a mapping variable, for the key reader.
    pub fn mapping_slot(&self, label: &str, key: &str) -> Result<(U256, String), String> {
        let entry = self.entries.iter().find(|entry| entry.label == label).ok_or("Unknown variable.")?;
        let info = self.type_info(&entry.type_id);
        if info["encoding"] != "mapping" {
            return Err("This variable is not a mapping.".into());
        }
        let key_type = self.type_info(info["key"].as_str().unwrap_or(""))["label"].as_str().unwrap_or("").to_string();
        let encoded = encode_key(&key_type, key)?;
        let mut data = encoded;
        data.extend_from_slice(&word(entry.slot));
        let value_type = self.type_info(info["value"].as_str().unwrap_or(""))["label"].as_str().unwrap_or("").to_string();
        Ok((U256::from_big_endian(&keccak256(data)), value_type))
    }
}

/// One named piece of a storage slot: the access path (`balanceOf[0xf39F…]`),
/// its type label, and where it sits in the word.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotField {
    pub path: String,
    pub type_label: String,
    pub offset: u64,
    pub size: u64,
}

impl Layout {
    /// Name a slot seen in a trace. `preimages` maps keccak results to the
    /// bytes hashed in the same execution, which recovers mapping keys and
    /// dynamic array bases. Packed variables sharing a slot are all returned.
    pub fn name_slot(&self, slot: U256, preimages: &std::collections::HashMap<U256, Vec<u8>>) -> Option<Vec<SlotField>> {
        let direct: Vec<SlotField> = self
            .entries
            .iter()
            .filter(|entry| entry.slot == slot)
            .map(|entry| SlotField { path: entry.label.clone(), type_label: self.label_of(&entry.type_id), offset: entry.offset, size: self.bytes(&entry.type_id) })
            .collect();
        if !direct.is_empty() {
            return Some(direct);
        }
        let (path, type_id) = self.locate(slot, preimages, 0)?;
        Some(vec![SlotField { path, type_label: self.label_of(&type_id), offset: 0, size: self.bytes(&type_id) }])
    }

    fn label_of(&self, type_id: &str) -> String {
        self.type_info(type_id)["label"].as_str().unwrap_or(type_id).to_string()
    }

    /// Access path and type id of the value starting at `slot`.
    fn locate(&self, slot: U256, preimages: &std::collections::HashMap<U256, Vec<u8>>, depth: usize) -> Option<(String, String)> {
        if depth > 6 {
            return None;
        }
        if let Some(entry) = self.entries.iter().find(|entry| entry.slot == slot && entry.offset == 0) {
            return Some((entry.label.clone(), entry.type_id.clone()));
        }
        // mapping: keccak(key . base)
        if let Some(bytes) = preimages.get(&slot).filter(|bytes| bytes.len() == 64) {
            let base = U256::from_big_endian(&bytes[32..]);
            if let Some((path, type_id)) = self.locate(base, preimages, depth + 1) {
                let info = self.type_info(&type_id);
                if info["encoding"] == "mapping" {
                    let key_type = self.label_of(info["key"].as_str().unwrap_or(""));
                    let value_type = info["value"].as_str().unwrap_or("").to_string();
                    return Some((format!("{path}[{}]", key_text(&key_type, &bytes[..32])), value_type));
                }
            }
        }
        // dynamic array element: keccak(base) + index
        for (hash, bytes) in preimages.iter().filter(|(hash, bytes)| bytes.len() == 32 && **hash <= slot && slot - **hash < U256::from(1u64 << 20)) {
            let base = U256::from_big_endian(bytes);
            let Some((path, type_id)) = self.locate(base, preimages, depth + 1) else { continue };
            let info = self.type_info(&type_id);
            if info["encoding"] != "dynamic_array" {
                continue;
            }
            let element = info["base"].as_str().unwrap_or("").to_string();
            let size = self.bytes(&element).max(1);
            let offset = (slot - *hash).as_u64();
            let path = if size >= 32 {
                format!("{path}[{}]", offset / size.div_ceil(32))
            } else {
                let per_slot = 32 / size;
                format!("{path}[{}..{}]", offset * per_slot, offset * per_slot + per_slot - 1)
            };
            return Some((path, element));
        }
        // struct member: the member's slot is the struct's base plus its index
        for back in 1..32u64 {
            let Some(base) = slot.checked_sub(U256::from(back)) else { break };
            if !preimages.contains_key(&base) && !self.entries.iter().any(|entry| entry.slot == base) {
                continue;
            }
            let Some((path, type_id)) = self.locate(base, preimages, depth + 1) else { continue };
            let members = self.type_info(&type_id)["members"].as_array().cloned().unwrap_or_default();
            if let Some(member) = members.iter().find(|member| member["slot"].as_str() == Some(&back.to_string()) && member["offset"].as_u64().unwrap_or(0) == 0) {
                return Some((format!("{path}.{}", member["label"].as_str().unwrap_or("?")), member["type"].as_str().unwrap_or("").to_string()));
            }
            break;
        }
        None
    }
}

impl Layout {
    /// Value of a storage path as written in Solidity: `owner`,
    /// `balanceOf[0xf39F…]`, `allowance[a][b]`, `list[2]`, `position.amount`.
    /// Structs list their members; mappings and arrays say what to add.
    pub fn show(&self, path: &str, read: &dyn Fn(U256) -> Option<H256>) -> Result<String, String> {
        let (slot, offset, type_id) = self.resolve(path)?;
        let info = self.type_info(&type_id);
        let label = self.label_of(&type_id);
        let size = self.bytes(&type_id);
        let unread = || format!("{path} ({label}) at slot {slot:#x}: not read in this trace and no state before it.");
        Ok(match info["encoding"].as_str().unwrap_or("inplace") {
            "mapping" => format!("{path} is a {label}; add a key, e.g. {path}[0x…]."),
            "dynamic_array" => match read(slot) {
                Some(length) => format!("{path} ({label}) length {}; add an index, e.g. {path}[0].", U256::from_big_endian(length.as_bytes())),
                None => unread(),
            },
            "bytes" => match read(slot) {
                Some(word) => {
                    let (value, note) = short_bytes(word);
                    format!("{path} ({label}) = {value}  ({note})")
                }
                None => unread(),
            },
            _ if info["members"].is_array() => {
                let mut lines = vec![format!("{path} ({label}) at slot {slot:#x}")];
                for member in info["members"].as_array().into_iter().flatten() {
                    let name = member["label"].as_str().unwrap_or("?");
                    match self.show(&format!("{path}.{name}"), read) {
                        Ok(text) => lines.push(format!("  {}", text.trim_start_matches(path).trim_start_matches('.'))),
                        Err(error) => lines.push(format!("  {name}: {error}")),
                    }
                }
                lines.join("\n")
            }
            _ if size > 32 || info["base"].is_string() => format!("{path} ({label}) spans {} slots from {slot:#x}; add an index.", size.div_ceil(32)),
            _ => match read(slot) {
                Some(word) => format!("{path} ({label}) = {}  (slot {slot:#x}{})", scalar(&label, word, offset, size), if offset > 0 { format!(", offset {offset}") } else { String::new() }),
                None => unread(),
            },
        })
    }

    /// Numeric value of a storage path (an unsigned field of one slot), for
    /// breakpoint conditions; also returns the slot, offset and size.
    pub fn value(&self, path: &str, read: &dyn Fn(U256) -> Option<H256>) -> Result<(U256, U256, u64, u64), String> {
        let (slot, offset, type_id) = self.resolve(path)?;
        let size = self.bytes(&type_id);
        if size > 32 || self.type_info(&type_id)["encoding"] != "inplace" || self.type_info(&type_id)["members"].is_array() {
            return Err(format!("{path} is not a single value; compare one member or element."));
        }
        let word = read(slot).ok_or_else(|| format!("{path} is not known at this step."))?;
        Ok((field(word, offset, size), slot, offset, size))
    }

    /// Slot, byte offset and type id of a storage path.
    pub fn resolve(&self, path: &str) -> Result<(U256, u64, String), String> {
        let path = path.trim();
        let end = path.find(['[', '.']).unwrap_or(path.len());
        let name = &path[..end];
        let entry = self.entries.iter().find(|entry| entry.label == name).ok_or_else(|| format!("No state variable named {name}."))?;
        let (mut slot, mut offset, mut type_id) = (entry.slot, entry.offset, entry.type_id.clone());
        let mut rest = &path[end..];
        while !rest.is_empty() {
            let info = self.type_info(&type_id).clone();
            if let Some(after) = rest.strip_prefix('[') {
                let close = after.find(']').ok_or("Missing ].")?;
                let key = &after[..close];
                rest = &after[close + 1..];
                match info["encoding"].as_str().unwrap_or("inplace") {
                    "mapping" => {
                        let key_type = self.label_of(info["key"].as_str().unwrap_or(""));
                        let mut data = encode_key(&key_type, key)?;
                        data.extend_from_slice(&word(slot));
                        slot = U256::from_big_endian(&keccak256(data));
                        offset = 0;
                        type_id = info["value"].as_str().unwrap_or("").to_string();
                    }
                    encoding @ ("dynamic_array" | "inplace") if info["base"].is_string() => {
                        let index: u64 = key.trim().parse().map_err(|_| format!("Index {key} is not a whole number."))?;
                        let base = if encoding == "dynamic_array" { U256::from_big_endian(&keccak256(word(slot))) } else { slot };
                        let element = info["base"].as_str().unwrap_or("").to_string();
                        let size = self.bytes(&element).max(1);
                        if size >= 32 {
                            slot = base + U256::from(index) * U256::from(size.div_ceil(32));
                            offset = 0;
                        } else {
                            let per_slot = 32 / size;
                            slot = base + U256::from(index / per_slot);
                            offset = (index % per_slot) * size;
                        }
                        type_id = element;
                    }
                    _ => return Err(format!("{} cannot be indexed.", self.label_of(&type_id))),
                }
            } else if let Some(after) = rest.strip_prefix('.') {
                let end = after.find(['[', '.']).unwrap_or(after.len());
                let member_name = &after[..end];
                rest = &after[end..];
                let member = info["members"]
                    .as_array()
                    .and_then(|members| members.iter().find(|member| member["label"] == member_name))
                    .ok_or_else(|| format!("{} has no member {member_name}.", self.label_of(&type_id)))?;
                slot += U256::from_dec_str(member["slot"].as_str().unwrap_or("0")).unwrap_or_default();
                offset = member["offset"].as_u64().unwrap_or(0);
                type_id = member["type"].as_str().unwrap_or("").to_string();
            } else {
                return Err(format!("Unexpected `{rest}` in the path."));
            }
        }
        Ok((slot, offset, type_id))
    }

    /// Mapping-entry preimages for keys that were not hashed in the trace:
    /// each candidate key under every top-level mapping, and one level of
    /// nested mappings (allowances). Merged with the trace's own preimages,
    /// `name_slot` then names those entries too.
    pub fn guessed_preimages(&self, keys: &[[u8; 32]]) -> std::collections::HashMap<U256, Vec<u8>> {
        let mut out = std::collections::HashMap::new();
        for entry in &self.entries {
            let info = self.type_info(&entry.type_id);
            if info["encoding"] != "mapping" {
                continue;
            }
            let nested = self.type_info(info["value"].as_str().unwrap_or(""))["encoding"] == "mapping";
            for key in keys {
                let mut data = key.to_vec();
                data.extend_from_slice(&word(entry.slot));
                let inner = U256::from_big_endian(&keccak256(&data));
                out.insert(inner, data);
                if nested {
                    for second in keys {
                        let mut data = second.to_vec();
                        data.extend_from_slice(&word(inner));
                        out.insert(U256::from_big_endian(&keccak256(&data)), data);
                    }
                }
            }
        }
        out
    }
}

/// A mapping key as written in source: addresses checksummed, integers decimal.
fn key_text(key_type: &str, word: &[u8]) -> String {
    let value = U256::from_big_endian(word);
    if key_type == "address" || key_type.starts_with("contract ") {
        return ethers::utils::to_checksum(&ethers::types::Address::from_slice(&word[12..]), None);
    }
    if key_type.starts_with("uint") || key_type.starts_with("enum ") {
        return value.to_string();
    }
    if key_type == "bool" {
        return (!value.is_zero()).to_string();
    }
    format!("0x{}", hex(word))
}

fn word(value: U256) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    bytes
}

fn hex(bytes: &[u8]) -> String {
    ethers::utils::hex::encode(bytes)
}

/// Unsigned value of `size` bytes, `offset` bytes from the right of the word.
pub fn field(word: H256, offset: u64, size: u64) -> U256 {
    let bytes = word.as_bytes();
    let end = 32usize.saturating_sub(offset as usize);
    let start = end.saturating_sub(size.min(32) as usize);
    U256::from_big_endian(&bytes[start..end])
}

/// Value of a packed field: `size` bytes, `offset` bytes from the right of the word.
pub fn scalar(type_label: &str, word: H256, offset: u64, size: u64) -> String {
    let bytes = word.as_bytes();
    let end = 32usize.saturating_sub(offset as usize);
    let start = end.saturating_sub(size.min(32) as usize);
    let field = &bytes[start..end];
    let unsigned = U256::from_big_endian(field);
    if type_label == "bool" {
        return (!unsigned.is_zero()).to_string();
    }
    if type_label == "address" || type_label.starts_with("contract ") || type_label == "address payable" {
        return ethers::utils::to_checksum(&ethers::types::Address::from_slice(&field[field.len().saturating_sub(20)..]), None);
    }
    if let Some(bits) = type_label.strip_prefix("int").and_then(|bits| bits.parse::<u32>().ok()) {
        let negative = field.first().is_some_and(|byte| byte & 0x80 != 0);
        if negative {
            let modulus = if bits >= 256 { U256::MAX } else { (U256::one() << bits) - 1 };
            return format!("-{}", (modulus - unsigned) + 1);
        }
        return unsigned.to_string();
    }
    if type_label.starts_with("uint") || type_label.starts_with("enum ") {
        return unsigned.to_string();
    }
    format!("0x{}", hex(field))
}

/// Short `bytes`/`string` values live in the slot; long ones only report their length.
fn short_bytes(word: H256) -> (String, String) {
    let bytes = word.as_bytes();
    if bytes[31] & 1 == 0 {
        let length = (bytes[31] / 2) as usize;
        let data = &bytes[..length.min(31)];
        match std::str::from_utf8(data) {
            Ok(text) if text.chars().all(|c| !c.is_control()) => (format!("{text:?}"), format!("{length} bytes")),
            _ => (format!("0x{}", hex(data)), format!("{length} bytes")),
        }
    } else {
        let length = (U256::from_big_endian(bytes) - 1) / 2;
        (String::new(), format!("{length} bytes, stored out of line; read it with a getter."))
    }
}

fn encode_key(key_type: &str, key: &str) -> Result<Vec<u8>, String> {
    let key = key.trim();
    let padded = |value: U256| word(value).to_vec();
    if key_type == "address" || key_type.starts_with("contract ") {
        let address: ethers::types::Address = key.parse().map_err(|_| "Enter a 20-byte 0x address.")?;
        let mut out = vec![0u8; 12];
        out.extend_from_slice(address.as_bytes());
        return Ok(out);
    }
    if key_type == "bool" {
        return Ok(padded(U256::from(matches!(key, "true" | "1") as u8)));
    }
    if key_type.starts_with("uint") || key_type.starts_with("int") {
        let value = if let Some(hex) = key.strip_prefix("0x") {
            U256::from_str_radix(hex, 16).map_err(|_| "Enter a decimal or 0x integer.")?
        } else {
            U256::from_dec_str(key).map_err(|_| "Enter a decimal or 0x integer.")?
        };
        return Ok(padded(value));
    }
    if let Some(size) = key_type.strip_prefix("bytes").and_then(|size| size.parse::<usize>().ok()) {
        let data = ethers::utils::hex::decode(key.trim_start_matches("0x")).map_err(|_| "Enter 0x hex bytes.")?;
        if data.len() != size {
            return Err(format!("Enter exactly {size} bytes."));
        }
        let mut out = data;
        out.resize(32, 0);
        return Ok(out);
    }
    if key_type == "string" {
        return Ok(key.as_bytes().to_vec());
    }
    if key_type == "bytes" {
        return ethers::utils::hex::decode(key.trim_start_matches("0x")).map_err(|_| "Enter 0x hex bytes.".into());
    }
    Err(format!("Keys of type {key_type} are not supported yet."))
}

#[cfg(test)]
mod tests {
    #[test]
    fn trace_slots_are_named_from_keccak_preimages() {
        let layout = Layout::from_json(&serde_json::json!({
            "storage": [
                {"label":"balanceOf","slot":"0","offset":0,"type":"t_mapping"},
                {"label":"owner","slot":"1","offset":0,"type":"t_address"},
                {"label":"paused","slot":"1","offset":20,"type":"t_bool"},
                {"label":"luckyNumbers","slot":"2","offset":0,"type":"t_array"}
            ],
            "types": {
                "t_mapping":{"encoding":"mapping","label":"mapping(address => uint256)","key":"t_address","value":"t_uint256","numberOfBytes":"32"},
                "t_address":{"encoding":"inplace","label":"address","numberOfBytes":"20"},
                "t_bool":{"encoding":"inplace","label":"bool","numberOfBytes":"1"},
                "t_uint256":{"encoding":"inplace","label":"uint256","numberOfBytes":"32"},
                "t_array":{"encoding":"dynamic_array","label":"uint256[]","base":"t_uint256","numberOfBytes":"32"}
            }
        })).unwrap();
        let mut preimages = std::collections::HashMap::new();
        let mut key = vec![0u8; 12];
        key.extend_from_slice(&ethers::utils::hex::decode("f39fd6e51aad88f6f4ce6ab8827279cfffb92266").unwrap());
        key.extend_from_slice(&word(U256::zero()));
        let slot = U256::from_big_endian(&keccak256(&key));
        preimages.insert(slot, key);
        let named = layout.name_slot(slot, &preimages).unwrap();
        assert_eq!(named[0].path, "balanceOf[0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266]");
        assert_eq!(named[0].type_label, "uint256");
        let packed = layout.name_slot(U256::one(), &preimages).unwrap();
        assert_eq!(packed.iter().map(|field| field.path.as_str()).collect::<Vec<_>>(), vec!["owner", "paused"]);
        let base = U256::from_big_endian(&keccak256(word(U256::from(2))));
        preimages.insert(base, word(U256::from(2)).to_vec());
        assert_eq!(layout.name_slot(base + 2, &preimages).unwrap()[0].path, "luckyNumbers[2]");
        assert!(layout.name_slot(U256::from(99), &preimages).is_none());
    }

    use super::*;
    use serde_json::json;

    fn bank() -> Layout {
        Layout::from_json(&json!({
            "storage": [
                {"astId":1,"contract":"Bank.sol:Bank","label":"balanceOf","offset":0,"slot":"0","type":"t_mapping(t_address,t_uint256)"},
                {"astId":2,"contract":"Bank.sol:Bank","label":"owner","offset":0,"slot":"1","type":"t_address"},
                {"astId":3,"contract":"Bank.sol:Bank","label":"luckyNumbers","offset":0,"slot":"2","type":"t_array(t_uint256)dyn_storage"},
                {"astId":4,"contract":"Bank.sol:Bank","label":"paused","offset":20,"slot":"1","type":"t_bool"}
            ],
            "types": {
                "t_address":{"encoding":"inplace","label":"address","numberOfBytes":"20"},
                "t_bool":{"encoding":"inplace","label":"bool","numberOfBytes":"1"},
                "t_uint256":{"encoding":"inplace","label":"uint256","numberOfBytes":"32"},
                "t_array(t_uint256)dyn_storage":{"base":"t_uint256","encoding":"dynamic_array","label":"uint256[]","numberOfBytes":"32"},
                "t_mapping(t_address,t_uint256)":{"encoding":"mapping","key":"t_address","label":"mapping(address => uint256)","numberOfBytes":"32","value":"t_uint256"}
            }
        })).unwrap()
    }

    #[test]
    fn decodes_packed_scalars_arrays_and_lists_mappings() {
        let layout = bank();
        let owner = "f39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
        let mut slot1 = [0u8; 32];
        slot1[11] = 1; // paused, byte 20 from the right
        slot1[12..].copy_from_slice(&ethers::utils::hex::decode(owner).unwrap());
        let base = U256::from_big_endian(&keccak256(word(U256::from(2))));
        let read = move |slot: U256| -> Option<H256> {
            if slot == U256::from(1) { return Some(H256::from(slot1)); }
            if slot == U256::from(2) { return Some(H256::from(word(U256::from(3)))); }
            if slot >= base && slot < base + 3 { return Some(H256::from(word(U256::from([7, 13, 42][(slot - base).as_usize()])))); }
            Some(H256::zero())
        };
        assert_eq!(layout.array_slots(&read).len(), 3);
        let vars = layout.decode(&read);
        assert_eq!(vars[0].label, "balanceOf");
        assert!(vars[0].note.contains("key"));
        assert_eq!(vars[1].value, "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        assert_eq!((vars[2].value.as_str(), vars[2].note.as_str()), ("[7, 13, 42]", "length 3"));
        assert_eq!(vars[3].value, "true");
    }

    #[test]
    fn mapping_slot_matches_solidity_keccak_rule() {
        let (slot, value_type) = bank().mapping_slot("balanceOf", "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap();
        // cast index address 0xf39F...2266 0
        assert_eq!(format!("{slot:#x}"), "0x723077b8a1b173adc35e5f0e7e3662fd1208212cb629f9c128551ea7168da722");
        assert_eq!(value_type, "uint256");
        assert!(bank().mapping_slot("owner", "1").is_err());
    }

    fn vault() -> Layout {
        Layout::from_json(&json!({
            "storage": [
                {"label":"owner","slot":"0","offset":0,"type":"t_address"},
                {"label":"paused","slot":"0","offset":20,"type":"t_bool"},
                {"label":"allowance","slot":"1","offset":0,"type":"t_mapping(t_address,t_mapping(t_address,t_uint256))"},
                {"label":"total","slot":"2","offset":0,"type":"t_struct(Total)"},
                {"label":"small","slot":"4","offset":0,"type":"t_array(t_uint64)dyn_storage"}
            ],
            "types": {
                "t_address":{"encoding":"inplace","label":"address","numberOfBytes":"20"},
                "t_bool":{"encoding":"inplace","label":"bool","numberOfBytes":"1"},
                "t_uint64":{"encoding":"inplace","label":"uint64","numberOfBytes":"8"},
                "t_uint128":{"encoding":"inplace","label":"uint128","numberOfBytes":"16"},
                "t_uint256":{"encoding":"inplace","label":"uint256","numberOfBytes":"32"},
                "t_mapping(t_address,t_uint256)":{"encoding":"mapping","key":"t_address","label":"mapping(address => uint256)","numberOfBytes":"32","value":"t_uint256"},
                "t_mapping(t_address,t_mapping(t_address,t_uint256))":{"encoding":"mapping","key":"t_address","label":"mapping(address => mapping(address => uint256))","numberOfBytes":"32","value":"t_mapping(t_address,t_uint256)"},
                "t_struct(Total)":{"encoding":"inplace","label":"struct Vault.Total","numberOfBytes":"64","members":[
                    {"label":"amount","slot":"0","offset":0,"type":"t_uint128"},
                    {"label":"shares","slot":"0","offset":16,"type":"t_uint128"},
                    {"label":"updated","slot":"1","offset":0,"type":"t_uint256"}
                ]},
                "t_array(t_uint64)dyn_storage":{"base":"t_uint64","encoding":"dynamic_array","label":"uint64[]","numberOfBytes":"32"}
            }
        })).unwrap()
    }

    #[test]
    fn paths_resolve_to_slots_like_solidity() {
        let layout = vault();
        let a = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
        let b = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        // cast index address b (cast index address a 1)
        let inner = U256::from_big_endian(&keccak256([encode_key("address", a).unwrap(), word(U256::one()).to_vec()].concat()));
        let outer = U256::from_big_endian(&keccak256([encode_key("address", b).unwrap(), word(inner).to_vec()].concat()));
        assert_eq!(layout.resolve(&format!("allowance[{a}][{b}]")).unwrap(), (outer, 0, "t_uint256".into()));
        assert_eq!(layout.resolve("total.shares").unwrap(), (U256::from(2), 16, "t_uint128".into()));
        assert_eq!(layout.resolve("total.updated").unwrap().0, U256::from(3));
        let base = U256::from_big_endian(&keccak256(word(U256::from(4))));
        assert_eq!(layout.resolve("small[5]").unwrap(), (base + 1, 8, "t_uint64".into()));
        assert!(layout.resolve("nothing").unwrap_err().contains("No state variable"));
        assert!(layout.resolve("total.nope").unwrap_err().contains("no member nope"));
        assert!(layout.resolve("owner[1]").unwrap_err().contains("cannot be indexed"));

        let mut slot0 = [0u8; 32];
        slot0[11] = 1;
        slot0[12..].copy_from_slice(&ethers::utils::hex::decode(&a[2..]).unwrap());
        let mut slot2 = [0u8; 32];
        slot2[15] = 3; // shares = 3 (upper 16 bytes)
        slot2[31] = 9; // amount = 9
        let read = move |slot: U256| -> Option<H256> {
            if slot == outer {
                return Some(H256::from(word(U256::from(500))));
            }
            match slot.low_u64() {
                _ if slot > U256::from(u64::MAX) => None,
                0 => Some(H256::from(slot0)),
                2 => Some(H256::from(slot2)),
                3 => Some(H256::from(word(U256::from(77)))),
                _ => None,
            }
        };
        assert_eq!(layout.show("paused", &read).unwrap(), "paused (bool) = true  (slot 0x0, offset 20)");
        assert!(layout.show(&format!("allowance[{a}][{b}]"), &read).unwrap().contains("= 500"));
        let total = layout.show("total", &read).unwrap();
        assert!(total.contains("amount (uint128) = 9") && total.contains("shares (uint128) = 3") && total.contains("updated (uint256) = 77"), "{total}");
        assert!(layout.show("allowance", &read).unwrap().contains("add a key"));
        assert!(layout.show("small", &read).unwrap().contains("not read"));
    }

    #[test]
    fn candidate_keys_name_entries_the_trace_never_hashed() {
        let layout = vault();
        let mut a = [0u8; 32];
        a[12..].copy_from_slice(&ethers::utils::hex::decode("f39fd6e51aad88f6f4ce6ab8827279cfffb92266").unwrap());
        let mut b = [0u8; 32];
        b[12..].copy_from_slice(&ethers::utils::hex::decode("70997970c51812dc3a010c7d01b50e0d17dc79c8").unwrap());
        let (slot, ..) = layout.resolve("allowance[0x70997970C51812dc3A010C7d01b50e0d17dc79C8][0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266]").unwrap();
        assert!(layout.name_slot(slot, &Default::default()).is_none());
        let guessed = layout.guessed_preimages(&[a, b]);
        assert_eq!(layout.name_slot(slot, &guessed).unwrap()[0].path, "allowance[0x70997970C51812dc3A010C7d01b50e0d17dc79C8][0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266]");
    }

    #[test]
    fn signed_and_short_string_values() {
        let mut minus_one = [0xffu8; 32];
        assert_eq!(scalar("int256", H256::from(minus_one), 0, 32), "-1");
        minus_one = [0u8; 32];
        minus_one[31] = 0xfe;
        assert_eq!(scalar("int8", H256::from(minus_one), 0, 1), "-2");
        let mut text = [0u8; 32];
        text[..2].copy_from_slice(b"hi");
        text[31] = 4;
        assert_eq!(short_bytes(H256::from(text)).0, "\"hi\"");
    }
}
