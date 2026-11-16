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

fn word(value: U256) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    bytes
}

fn hex(bytes: &[u8]) -> String {
    ethers::utils::hex::encode(bytes)
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
