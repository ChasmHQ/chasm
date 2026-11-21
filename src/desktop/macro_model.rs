//! Local macro definitions and validation. This module does not execute steps.
//!
//! Credential screening is best effort for free text. Callers must never put
//! wallet keys, passwords, endpoint credentials, or other secrets in these models.

use ethers::abi::param_type::{Reader, Writer};
use ethers::abi::ParamType;
use ethers::types::U256;
use rand::{rngs::OsRng, RngCore};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

pub const MACRO_SCHEMA_VERSION: u32 = 1;
pub const MAX_MACRO_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_STRING_BYTES: usize = 64 * 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_NAME_BYTES: usize = 200;
pub const MAX_DESCRIPTION_BYTES: usize = 4000;
pub const MAX_STEPS: usize = 128;
pub const MAX_PROBES: usize = 32;
pub const MAX_SWEEP_ATTEMPTS: u64 = 512;
pub const MAX_SWEEP_SPAN: u64 = 512;
pub const MAX_DELAY_MS: u64 = 30_000;
pub const MAX_VALUE_DEPTH: usize = 32;
pub const MAX_VALUE_ELEMENTS: usize = 16_384;
pub const MAX_PUBLIC_VALUE_BYTES: usize = 1024 * 1024;
const MAX_ERRORS: usize = 256;
const MAX_SIGNATURE_BYTES: usize = 4096;
const MAX_SIGNATURE_DEPTH: usize = 32;

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                let mut bytes = [0u8; 16];
                OsRng.fill_bytes(&mut bytes);
                Self(format!(
                    concat!($prefix, "-{}"),
                    ethers::utils::hex::encode(bytes)
                ))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

id_type!(MacroId, "macro");
id_type!(StepId, "step");
id_type!(RunId, "run");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacroDefinition {
    pub schema_version: u32,
    pub id: MacroId,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
    #[serde(default)]
    pub snapshot_before_run: bool,
    #[serde(default)]
    pub revert_on_failure: bool,
    #[serde(default)]
    pub rollback_probes: Vec<StorageProbe>,
    /// Send every leading transaction in one block; a failure reverts the run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bundle: bool,
    /// The network (`profile:chain:block`) the macro was first validated on;
    /// set automatically. Validation warns — it does not block — when the
    /// macro runs on another network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    pub steps: Vec<MacroStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_notes: Vec<LegacyNote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyNote {
    pub name: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageProbe {
    pub address: String,
    pub slot: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacroStep {
    pub id: StepId,
    pub enabled: bool,
    pub kind: StepKind,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signer {
    AnvilAccount0,
    AnvilAccount1,
    AnvilAccount2,
    UnlockedWalletRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbiCallMode {
    Read,
    Send,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertComparator {
    Eq,
    NotEq,
    Zero,
    Nonzero,
    True,
    False,
    HashEq,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SweepPredicate {
    Equals { value: String },
    Zero {},
    Nonzero {},
    Revert {},
    Success {},
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepKind {
    DeployArtifact {
        artifact_id: String,
        constructor_signature: String,
        args: Vec<Value>,
        signer: Signer,
        value: String,
        gas_limit: String,
    },
    AbiCall {
        artifact_id: String,
        signature: String,
        mode: AbiCallMode,
        target: String,
        args: Vec<Value>,
        signer: Signer,
        value: String,
        gas_limit: String,
    },
    RawCall {
        target: String,
        data: String,
        signer: Signer,
        value: String,
        gas_limit: String,
    },
    RawSend {
        target: String,
        data: String,
        signer: Signer,
        value: String,
        gas_limit: String,
    },
    StorageRead {
        target: String,
        slot: String,
    },
    Snapshot {
        #[serde(default)]
        probes: Vec<StorageProbe>,
    },
    Revert {
        snapshot: String,
    },
    GasSweep {
        target: String,
        data: String,
        signer: Signer,
        value: String,
        start: String,
        end: String,
        increment: String,
        #[serde(default)]
        max_attempts: u16,
        #[serde(default)]
        predicate: Option<SweepPredicate>,
    },
    ProbeGasSweep {
        artifact_id: String,
        probe: String,
        router: String,
        data: String,
        token: String,
        treasury: String,
        signer: Signer,
        value: String,
        start: String,
        end: String,
        increment: String,
        max_attempts: u16,
        outer_gas_margin: String,
        final_gas_margin: String,
    },
    Assert {
        left: String,
        comparator: AssertComparator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        right: Option<Value>,
    },
    Delay {
        millis: u64,
    },
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Success,
    Failed,
    Unknown,
    Skipped,
    RolledBack,
    Cancelled,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepRun {
    pub step_id: StepId,
    pub status: RunStatus,
    pub request: Option<Value>,
    pub result: Option<Value>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacroRun {
    pub run_id: RunId,
    pub macro_id: MacroId,
    pub status: RunStatus,
    pub steps: Vec<StepRun>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationError {
    pub path: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightSummary {
    pub total_steps: usize,
    pub enabled_steps: usize,
    pub disabled_steps: usize,
    pub broadcast_count: usize,
    pub read_count: usize,
    pub max_read_requests: u64,
    pub max_sweep_attempts: u64,
    pub total_sweep_attempts: u64,
    pub signers: Vec<Signer>,
    pub targets: Vec<String>,
    pub artifact_ids: Vec<String>,
    pub snapshot_count: usize,
    pub revert_count: usize,
    pub snapshot_before_run: bool,
    pub revert_on_failure: bool,
    pub rollback_probe_count: usize,
    pub revert_policy: String,
    pub local_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    Step { id: StepId, field: String },
    Account { index: u8 },
    Variable { id: String },
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn parse_quantity(text: &str) -> Result<U256, &'static str> {
    if text.is_empty() || text.len() > 80 {
        return Err("Expected an unsigned uint256 decimal or 0x hexadecimal string.");
    }
    if let Some(hex) = text.strip_prefix("0x") {
        if hex.is_empty() || hex.len() > 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Expected an unsigned uint256 decimal or 0x hexadecimal string.");
        }
        let significant = hex.trim_start_matches('0');
        if significant.is_empty() {
            Ok(U256::zero())
        } else {
            U256::from_str_radix(significant, 16)
                .map_err(|_| "Expected an unsigned uint256 decimal or 0x hexadecimal string.")
        }
    } else {
        if !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Expected an unsigned uint256 decimal or 0x hexadecimal string.");
        }
        let significant = text.trim_start_matches('0');
        if significant.is_empty() {
            Ok(U256::zero())
        } else {
            U256::from_dec_str(significant)
                .map_err(|_| "Expected an unsigned uint256 decimal or 0x hexadecimal string.")
        }
    }
}

pub fn parse_gas(text: &str) -> Result<u64, &'static str> {
    let value = parse_quantity(text)?;
    if value > U256::from(u64::MAX) {
        return Err("Gas exceeds u64.");
    }
    Ok(value.as_u64())
}

pub fn valid_calldata(text: &str) -> bool {
    text.len() <= MAX_STRING_BYTES
        && text.strip_prefix("0x").is_some_and(|digits| {
            digits.len() % 2 == 0 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

pub fn valid_address(text: &str) -> bool {
    text.strip_prefix("0x").is_some_and(|digits| {
        digits.len() == 40 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub fn parse_reference(text: &str) -> Result<Reference, &'static str> {
    let inner = text
        .strip_prefix("${")
        .and_then(|text| text.strip_suffix('}'))
        .ok_or("A reference must be one complete ${...} token.")?;
    let parts: Vec<_> = inner.split('.').collect();
    match parts.as_slice() {
        ["step", id, field]
            if valid_id(id) && (PUBLIC_OUTPUT_FIELDS.contains(field) || *field == "snapshotId") =>
        {
            Ok(Reference::Step {
                id: StepId((*id).to_string()),
                field: (*field).to_string(),
            })
        }
        ["account", index @ ("0" | "1" | "2"), "address"] => Ok(Reference::Account {
            index: index.as_bytes()[0] - b'0',
        }),
        ["var", id] if valid_id(id) => Ok(Reference::Variable {
            id: (*id).to_string(),
        }),
        _ => Err("Reference must name an allowlisted step field, account address, or variable."),
    }
}

const PUBLIC_OUTPUT_FIELDS: &[&str] = &[
    "contractAddress",
    "txHash",
    "blockNumber",
    "gasUsed",
    "returnData",
    "selectedGas",
    "storageValue",
    "passed",
];

pub fn output_fields(kind: &StepKind) -> &'static [&'static str] {
    match kind {
        StepKind::DeployArtifact { .. } => &["contractAddress", "txHash", "blockNumber", "gasUsed"],
        StepKind::AbiCall {
            mode: AbiCallMode::Read,
            ..
        } => &["returnData"],
        StepKind::AbiCall {
            mode: AbiCallMode::Send,
            ..
        } => &["txHash", "blockNumber", "gasUsed"],
        StepKind::RawCall { .. } => &["returnData"],
        StepKind::RawSend { .. } => &["txHash", "blockNumber", "gasUsed"],
        StepKind::StorageRead { .. } => &["storageValue"],
        StepKind::Snapshot { .. } => &[],
        StepKind::Revert { .. } => &["passed"],
        StepKind::GasSweep { .. } => &["selectedGas", "returnData"],
        StepKind::ProbeGasSweep { .. } => &["selectedGas"],
        StepKind::Assert { .. } => &["passed"],
        StepKind::Delay { .. } => &[],
    }
}

fn error(path: &str, code: &str, message: &str) -> ValidationError {
    ValidationError {
        path: path.to_string(),
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn push(errors: &mut Vec<ValidationError>, path: &str, code: &str, message: &str) {
    if errors.len() < MAX_ERRORS {
        errors.push(error(path, code, message));
    }
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "key"
            | "privatekey"
            | "password"
            | "passphrase"
            | "mnemonic"
            | "seed"
            | "seedphrase"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "apikey"
            | "accesskey"
            | "secret"
            | "authorization"
            | "credentials"
    )
}

fn sensitive_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    [
        "privatekey",
        "password",
        "passphrase",
        "mnemonic",
        "seedphrase",
        "secret",
        "apikey",
        "accesstoken",
        "refreshtoken",
        "authorization",
        "credentials",
    ]
    .iter()
    .any(|word| compact.contains(word))
        || lower.contains("://")
        || lower.contains("bearer ")
        || assigns_credential(&lower)
}

/// `token`/`key` are ordinary ERC-20 and ABI words ("Approve token",
/// `${step.token.contractAddress}`, `token()`). They only look like a leaked
/// credential when a value is assigned to them, as in `token=…` or `key: …`.
fn assigns_credential(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    ["token", "key"].iter().any(|name| {
        lower.match_indices(name).any(|(start, _)| {
            let end = start + name.len();
            let bounded = (start == 0 || !word(bytes[start - 1]))
                && bytes.get(end).is_none_or(|byte| !word(*byte));
            bounded
                && lower[end..]
                    .trim_start()
                    .starts_with(['=', ':'])
        })
    })
}

#[derive(Default)]
struct WalkBudget {
    elements: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScreenScope {
    Public,
    Macro,
    Steps,
    Step,
    Kind,
    Variables,
}

fn screen_value(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut WalkBudget,
    errors: &mut Vec<ValidationError>,
    scope: ScreenScope,
) {
    if errors.len() >= MAX_ERRORS || budget.elements > MAX_VALUE_ELEMENTS {
        return;
    }
    if depth > MAX_VALUE_DEPTH {
        push(
            errors,
            path,
            "depth_limit",
            "JSON nesting exceeds 32 levels.",
        );
        return;
    }
    budget.elements += 1;
    if budget.elements > MAX_VALUE_ELEMENTS {
        push(
            errors,
            path,
            "element_limit",
            "JSON exceeds the element limit.",
        );
        return;
    }
    match value {
        Value::String(text) => {
            if text.len() > MAX_STRING_BYTES {
                push(errors, path, "string_limit", "A string exceeds 64 KiB.");
            }
            if sensitive_text(text) {
                push(
                    errors,
                    path,
                    "secret_or_url",
                    "Secret-looking material or a URL is not allowed.",
                );
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let item_scope = if scope == ScreenScope::Steps {
                    ScreenScope::Step
                } else {
                    ScreenScope::Public
                };
                screen_value(item, &format!("{path}[{index}]"), depth + 1, budget, errors, item_scope);
                if budget.elements > MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        Value::Object(map) => {
            // A probe's `token` field and a top-level variable named `token`
            // hold an ERC-20 address, not a credential.
            let token_slot = scope == ScreenScope::Variables
                || (scope == ScreenScope::Kind
                    && map.get("type").and_then(Value::as_str) == Some("probe_gas_sweep"));
            for (index, (key, item)) in map.iter().enumerate() {
                if key.len() > MAX_STRING_BYTES {
                    push(errors, path, "string_limit", "A JSON key exceeds 64 KiB.");
                }
                let public_token_address = token_slot
                    && key == "token"
                    && item.as_str().is_some_and(|text| {
                        valid_address(text) || parse_reference(text).is_ok()
                    });
                if !public_token_address && (is_sensitive_key(key) || sensitive_text(key)) {
                    push(
                        errors,
                        path,
                        "secret_key",
                        "A secret-looking JSON key is not allowed.",
                    );
                }
                let item_scope = match (scope, key.as_str()) {
                    (ScreenScope::Macro, "steps") => ScreenScope::Steps,
                    (ScreenScope::Macro, "variables") => ScreenScope::Variables,
                    (ScreenScope::Step, "kind") => ScreenScope::Kind,
                    _ => ScreenScope::Public,
                };
                screen_value(item, &format!("{path}[{index}]"), depth + 1, budget, errors, item_scope);
                if budget.elements > MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn screen_reference_syntax(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut WalkBudget,
    errors: &mut Vec<ValidationError>,
) {
    if depth > MAX_VALUE_DEPTH
        || budget.elements >= MAX_VALUE_ELEMENTS
        || errors.len() >= MAX_ERRORS
    {
        return;
    }
    budget.elements += 1;
    match value {
        Value::String(text) if text.contains("${") || text.contains('}') => {
            if parse_reference(text).is_err() {
                push(
                    errors,
                    path,
                    "reference",
                    "References must be complete allowlisted tokens.",
                );
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                screen_reference_syntax(
                    item,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    budget,
                    errors,
                );
                if budget.elements >= MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        Value::Object(map) => {
            for (index, (key, item)) in map.iter().enumerate() {
                if key.contains("${") || key.contains('}') {
                    push(
                        errors,
                        path,
                        "reference",
                        "Object keys cannot contain references.",
                    );
                }
                screen_reference_syntax(
                    item,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    budget,
                    errors,
                );
                if budget.elements >= MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn screen_snapshot_handles(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut WalkBudget,
    errors: &mut Vec<ValidationError>,
) {
    if depth > MAX_VALUE_DEPTH
        || budget.elements >= MAX_VALUE_ELEMENTS
        || errors.len() >= MAX_ERRORS
    {
        return;
    }
    budget.elements += 1;
    match value {
        Value::String(text)
            if {
                let lower = text.to_ascii_lowercase();
                lower.contains("evm_snapshot")
                    || lower.contains("evm_revert")
                    || lower.contains("snapshotid")
                    || lower.contains("snapshot_id")
            } =>
        {
            push(
                errors,
                path,
                "snapshot_handle",
                "Raw snapshot and revert RPC requests must not be retained in public run data.",
            );
        }
        Value::Object(map) => {
            for (index, (key, child)) in map.iter().enumerate() {
                if matches!(
                    normalized_key(key).as_str(),
                    "snapshot"
                        | "snapshotid"
                        | "snapshotidentifier"
                        | "checkpoint"
                        | "checkpointid"
                ) {
                    push(
                        errors,
                        path,
                        "snapshot_handle",
                        "Node snapshot handles must not be retained in public run data.",
                    );
                }
                screen_snapshot_handles(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    budget,
                    errors,
                );
                if budget.elements >= MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                screen_snapshot_handles(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    budget,
                    errors,
                );
                if budget.elements >= MAX_VALUE_ELEMENTS {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn json_size<T: Serialize>(value: &T, limit: usize) -> bool {
    struct Counter {
        count: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.count = self.count.saturating_add(bytes.len());
            if self.count > self.limit {
                return Err(std::io::Error::other("size limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { count: 0, limit }, value).is_ok()
}

fn validate_scoped_value(value: &Value, scope: ScreenScope) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    screen_value(value, "$", 0, &mut WalkBudget::default(), &mut errors, scope);
    if !json_size(value, MAX_PUBLIC_VALUE_BYTES) {
        push(
            &mut errors,
            "$",
            "size_limit",
            "Public JSON exceeds 1 MiB or cannot be encoded.",
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
pub fn validate_public_value(value: &Value) -> Result<(), Vec<ValidationError>> {
    validate_scoped_value(value, ScreenScope::Public)
}

struct UniqueJsonSeed<'a> {
    depth: usize,
    elements: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for UniqueJsonSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        if self.depth > MAX_VALUE_DEPTH || *self.elements >= MAX_VALUE_ELEMENTS {
            return Err(de::Error::custom("JSON limit exceeded"));
        }
        *self.elements += 1;
        deserializer.deserialize_any(UniqueJsonVisitor {
            depth: self.depth,
            elements: self.elements,
        })
    }
}

struct UniqueJsonVisitor<'a> {
    depth: usize,
    elements: &'a mut usize,
}

impl<'de> Visitor<'de> for UniqueJsonVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<(), E> {
        if value.len() > MAX_STRING_BYTES {
            return Err(E::custom("string limit exceeded"));
        }
        Ok(())
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<(), E> {
        self.visit_str(&value)
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        UniqueJsonSeed {
            depth: self.depth + 1,
            elements: self.elements,
        }
        .deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq
            .next_element_seed(UniqueJsonSeed {
                depth: self.depth + 1,
                elements: self.elements,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > MAX_STRING_BYTES || !keys.insert(key) {
                return Err(de::Error::custom("invalid or duplicate object key"));
            }
            map.next_value_seed(UniqueJsonSeed {
                depth: self.depth + 1,
                elements: self.elements,
            })?;
        }
        Ok(())
    }
}

fn parse_bounded_json(bytes: &[u8], scope: ScreenScope) -> Result<Value, Vec<ValidationError>> {
    if bytes.len() > MAX_MACRO_JSON_BYTES {
        return Err(vec![error("$", "size_limit", "JSON exceeds 1 MiB.")]);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let mut elements = 0;
    UniqueJsonSeed {
        depth: 0,
        elements: &mut elements,
    }
    .deserialize(&mut deserializer)
    .and_then(|()| deserializer.end())
    .map_err(|_| {
        vec![error(
            "$",
            "invalid_json",
            "JSON is invalid, contains duplicate keys, or exceeds a structural limit.",
        )]
    })?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        vec![error(
            "$",
            "invalid_json",
            "JSON is invalid or too deeply nested.",
        )]
    })?;
    validate_scoped_value(&value, scope)?;
    Ok(value)
}

pub fn decode_macro(bytes: &[u8]) -> Result<MacroDefinition, Vec<ValidationError>> {
    parse_bounded_json(bytes, ScreenScope::Macro)?;
    let definition: MacroDefinition = serde_json::from_slice(bytes).map_err(|_| {
        vec![error(
            "$",
            "invalid_schema",
            "Macro JSON does not match the typed schema.",
        )]
    })?;
    definition.validate_storage()?;
    Ok(definition)
}

#[cfg(test)]
pub fn decode_run(bytes: &[u8]) -> Result<MacroRun, Vec<ValidationError>> {
    parse_bounded_json(bytes, ScreenScope::Public)?;
    let run: MacroRun = serde_json::from_slice(bytes).map_err(|_| {
        vec![error(
            "$",
            "invalid_schema",
            "Run JSON does not match the typed schema.",
        )]
    })?;
    run.validate()?;
    Ok(run)
}

pub fn migrate_legacy(bytes: &[u8]) -> Result<Vec<MacroDefinition>, Vec<ValidationError>> {
    parse_bounded_json(bytes, ScreenScope::Public)?;
    let notes: Vec<LegacyNote> = serde_json::from_slice(bytes).map_err(|_| {
        vec![error(
            "$",
            "invalid_schema",
            "Legacy notes must be objects containing only name and body.",
        )]
    })?;
    migrate_legacy_notes(notes)
}

pub fn migrate_legacy_notes(
    notes: Vec<LegacyNote>,
) -> Result<Vec<MacroDefinition>, Vec<ValidationError>> {
    if notes.len() > MAX_STEPS {
        return Err(vec![error(
            "$",
            "note_limit",
            "At most 128 legacy notes can be migrated at once.",
        )]);
    }
    if !json_size(&notes, MAX_MACRO_JSON_BYTES) {
        return Err(vec![error(
            "$",
            "size_limit",
            "Legacy JSON exceeds 1 MiB or cannot be encoded.",
        )]);
    }
    let mut migrated = Vec::with_capacity(notes.len());
    for note in notes {
        let definition = MacroDefinition {
            schema_version: MACRO_SCHEMA_VERSION,
            id: MacroId::new(),
            name: note.name.clone(),
            description: String::new(),
            variables: BTreeMap::new(),
            snapshot_before_run: false,
            revert_on_failure: false,
            rollback_probes: Vec::new(),
            bundle: false,
            network: None,
            steps: Vec::new(),
            legacy_notes: vec![note],
        };
        definition.validate_storage()?;
        migrated.push(definition);
    }
    Ok(migrated)
}

#[derive(Clone, Copy)]
enum Expected {
    Address,
    Calldata,
    Quantity,
    Gas,
    Any,
    Snapshot,
}

struct Validator<'a> {
    definition: &'a MacroDefinition,
    previous: HashMap<&'a str, &'a StepKind>,
    active_snapshots: HashSet<&'a str>,
    verified_reverts: HashSet<&'a str>,
    errors: Vec<ValidationError>,
    summary: PreflightSummary,
}

impl MacroDefinition {
    pub fn validate_storage(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        if !json_size(self, MAX_MACRO_JSON_BYTES) {
            push(
                &mut errors,
                "$",
                "size_limit",
                "Macro JSON exceeds 1 MiB or cannot be encoded.",
            );
        }
        let value = match serde_json::to_value(self) {
            Ok(value) => value,
            Err(_) => return Err(vec![error("$", "encoding", "Macro cannot be encoded.")]),
        };
        screen_value(&value, "$", 0, &mut WalkBudget::default(), &mut errors, ScreenScope::Macro);
        if let Some(object) = value.as_object() {
            for key in ["variables", "steps", "rollbackProbes"] {
                if let Some(child) = object.get(key) {
                    screen_reference_syntax(
                        child,
                        &format!("$.{key}"),
                        0,
                        &mut WalkBudget::default(),
                        &mut errors,
                    );
                }
            }
        }
        if self.schema_version != MACRO_SCHEMA_VERSION {
            push(
                &mut errors,
                "$.schemaVersion",
                "version",
                "Unsupported macro schema version.",
            );
        }
        if !valid_id(self.id.as_str()) {
            push(
                &mut errors,
                "$.id",
                "id",
                "Macro ID must contain 1 to 128 ASCII letters, digits, underscores, or hyphens.",
            );
        }
        if self.name.trim().is_empty() || self.name.len() > MAX_NAME_BYTES {
            push(
                &mut errors,
                "$.name",
                "name",
                "Name must be nonempty and at most 200 bytes.",
            );
        }
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            push(
                &mut errors,
                "$.description",
                "description",
                "Description exceeds 4000 bytes.",
            );
        }
        if self.steps.len() > MAX_STEPS {
            push(
                &mut errors,
                "$.steps",
                "step_limit",
                "A macro can contain at most 128 steps.",
            );
        }
        if self.rollback_probes.len() > MAX_PROBES {
            push(
                &mut errors,
                "$.rollbackProbes",
                "probe_limit",
                "At most 32 rollback probes are allowed.",
            );
        }
        for (index, step) in self.steps.iter().enumerate() {
            if let StepKind::Snapshot { probes } = &step.kind {
                if probes.len() > MAX_PROBES {
                    push(
                        &mut errors,
                        &format!("$.steps[{index}].kind.probes"),
                        "probe_limit",
                        "At most 32 snapshot probes are allowed.",
                    );
                }
            }
        }
        if self.legacy_notes.len() > MAX_STEPS {
            push(
                &mut errors,
                "$.legacyNotes",
                "note_limit",
                "A macro can contain at most 128 legacy notes.",
            );
        }
        for (index, note) in self.legacy_notes.iter().enumerate() {
            if note.name.trim().is_empty() || note.name.len() > MAX_NAME_BYTES {
                push(
                    &mut errors,
                    &format!("$.legacyNotes[{index}].name"),
                    "name",
                    "Legacy note name must be nonempty and at most 200 bytes.",
                );
            }
        }
        let mut ids = HashSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            let path = format!("$.steps[{index}].id");
            if !valid_id(step.id.as_str()) {
                push(
                    &mut errors,
                    &path,
                    "id",
                    "Step ID must contain 1 to 128 ASCII letters, digits, underscores, or hyphens.",
                );
            }
            if !ids.insert(step.id.as_str()) {
                push(
                    &mut errors,
                    &path,
                    "duplicate_id",
                    "Step IDs must be unique.",
                );
            }
        }
        for id in self.variables.keys() {
            if !valid_id(id) {
                push(&mut errors, "$.variables", "id", "Variable IDs must contain 1 to 128 ASCII letters, digits, underscores, or hyphens.");
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn validate(&self) -> Result<PreflightSummary, Vec<ValidationError>> {
        self.validate_storage()?;
        let mut validator = Validator {
            definition: self,
            previous: HashMap::new(),
            active_snapshots: HashSet::new(),
            verified_reverts: HashSet::new(),
            errors: Vec::new(),
            summary: PreflightSummary {
                total_steps: self.steps.len(),
                enabled_steps: 0,
                disabled_steps: 0,
                broadcast_count: 0,
                read_count: 0,
                max_read_requests: 0,
                max_sweep_attempts: 0,
                total_sweep_attempts: 0,
                signers: Vec::new(),
                targets: Vec::new(),
                artifact_ids: Vec::new(),
                snapshot_count: 0,
                revert_count: 0,
                snapshot_before_run: self.snapshot_before_run,
                revert_on_failure: self.revert_on_failure,
                rollback_probe_count: self.rollback_probes.len(),
                revert_policy: if self.revert_on_failure {
                    "A private pre-run snapshot is reverted on failure; successful read-back of every configured probe is required for verified rollback. The original failure remains a failure.".into()
                } else {
                    "No automatic rollback. Explicit reverts consume the referenced snapshot and later snapshots; read-back probes are required for verified rollback.".into()
                },
                local_only: true,
            },
        };
        if !self.legacy_notes.is_empty() {
            push(
                &mut validator.errors,
                "$.legacyNotes",
                "legacy_notes",
                "Legacy notes are inert; convert or remove them before running.",
            );
        }
        if self.revert_on_failure && !self.snapshot_before_run {
            validator.issue(
                "$.revertOnFailure",
                "rollback_policy",
                "Rollback on failure requires a pre-run snapshot.",
            );
        }
        if self.revert_on_failure && self.rollback_probes.is_empty() {
            validator.issue(
                "$.rollbackProbes",
                "rollback_probes",
                "Rollback on failure requires at least one storage verification probe.",
            );
        }
        if !self.snapshot_before_run && !self.rollback_probes.is_empty() {
            validator.issue(
                "$.rollbackProbes",
                "rollback_policy",
                "Rollback probes require a pre-run snapshot.",
            );
        }
        let mut budget = MAX_VALUE_ELEMENTS;
        for (index, value) in self.variables.values().enumerate() {
            validator.literal(
                value,
                &format!("$.variables[{index}]"),
                false,
                0,
                &mut budget,
            );
            if budget == 0 {
                break;
            }
        }
        validator.probes(&self.rollback_probes, "$.rollbackProbes");
        if self.snapshot_before_run {
            let reads =
                self.rollback_probes.len() as u64 * if self.revert_on_failure { 2 } else { 1 };
            validator.summary.max_read_requests =
                validator.summary.max_read_requests.saturating_add(reads);
        }
        for (index, step) in self.steps.iter().enumerate().take(MAX_STEPS + 1) {
            if step.enabled {
                validator.summary.enabled_steps += 1;
                validator.step(index, step);
                validator.previous.insert(step.id.as_str(), &step.kind);
            } else {
                validator.summary.disabled_steps += 1;
            }
        }
        if validator.errors.is_empty() {
            Ok(validator.summary)
        } else {
            Err(validator.errors)
        }
    }
}

impl<'a> Validator<'a> {
    fn issue(&mut self, path: &str, code: &str, message: &str) {
        push(&mut self.errors, path, code, message);
    }

    fn reference(&mut self, text: &str, path: &str, expected: Expected) -> Option<Reference> {
        let reference = match parse_reference(text) {
            Ok(reference) => reference,
            Err(message) => {
                self.issue(path, "reference", message);
                return None;
            }
        };
        match &reference {
            Reference::Step { id, field } => {
                let Some(kind) = self.previous.get(id.as_str()) else {
                    self.issue(
                        path,
                        "reference_order",
                        "Reference must point to an earlier enabled step.",
                    );
                    return None;
                };
                if matches!(expected, Expected::Snapshot) {
                    if field != "snapshotId"
                        || !matches!(kind, StepKind::Snapshot { .. })
                        || !self.active_snapshots.contains(id.as_str())
                    {
                        self.issue(
                            path,
                            "snapshot",
                            "Revert must reference an earlier unconsumed snapshot.",
                        );
                        return None;
                    }
                } else {
                    if !output_fields(kind).contains(&field.as_str()) {
                        self.issue(
                            path,
                            "reference_field",
                            "The referenced step does not expose this field.",
                        );
                        return None;
                    }
                    if matches!(kind, StepKind::Revert { .. })
                        && field == "passed"
                        && !self.verified_reverts.contains(id.as_str())
                    {
                        self.issue(
                            path,
                            "unverified_revert",
                            "A revert without probes does not expose a verified result.",
                        );
                        return None;
                    }
                    let compatible = match expected {
                        Expected::Address => field == "contractAddress",
                        Expected::Calldata => field == "returnData",
                        Expected::Quantity | Expected::Gas => matches!(
                            field.as_str(),
                            "blockNumber" | "gasUsed" | "selectedGas" | "storageValue"
                        ),
                        Expected::Any => true,
                        Expected::Snapshot => unreachable!(),
                    };
                    if !compatible {
                        self.issue(
                            path,
                            "reference_type",
                            "The referenced output is not compatible with this field.",
                        );
                        return None;
                    }
                }
            }
            Reference::Account { .. } if matches!(expected, Expected::Snapshot) => {
                self.issue(
                    path,
                    "snapshot",
                    "Revert must reference an earlier unconsumed snapshot.",
                );
                return None;
            }
            Reference::Variable { id } => {
                if matches!(expected, Expected::Snapshot) {
                    self.issue(
                        path,
                        "snapshot",
                        "Revert must reference an earlier unconsumed snapshot.",
                    );
                    return None;
                }
                let Some(value) = self.definition.variables.get(id) else {
                    self.issue(
                        path,
                        "reference_variable",
                        "Referenced variable does not exist.",
                    );
                    return None;
                };
                self.resolved_type(value, path, expected);
            }
            Reference::Account { .. } if !matches!(expected, Expected::Address | Expected::Any) => {
                self.issue(
                    path,
                    "reference_type",
                    "An account address cannot be used in this field.",
                );
                return None;
            }
            _ => {}
        }
        Some(reference)
    }

    fn resolved_type(&mut self, value: &Value, path: &str, expected: Expected) {
        if matches!(expected, Expected::Any) {
            return;
        }
        let Some(text) = value.as_str() else {
            self.issue(
                path,
                "value_type",
                "The referenced variable must be a string for this field.",
            );
            return;
        };
        self.literal_field(text, path, expected);
    }

    fn literal_field(&mut self, text: &str, path: &str, expected: Expected) {
        let valid = match expected {
            Expected::Address => valid_address(text),
            Expected::Calldata => valid_calldata(text),
            Expected::Quantity => parse_quantity(text).is_ok(),
            Expected::Gas => parse_gas(text).is_ok_and(|value| value > 0),
            Expected::Any => true,
            Expected::Snapshot => false,
        };
        if !valid {
            let message = match expected {
                Expected::Address => "Expected an exact 0x-prefixed 20-byte address.",
                Expected::Calldata => "Expected even-length 0x-prefixed hexadecimal bytes.",
                Expected::Quantity => "Expected an unsigned uint256 decimal or 0x hexadecimal string.",
                Expected::Gas => "Expected a positive gas limit as an exact u64 decimal or 0x hexadecimal string.",
                Expected::Any => "Invalid value.",
                Expected::Snapshot => "Expected a snapshot reference.",
            };
            self.issue(path, "value", message);
        }
    }

    fn field(&mut self, text: &str, path: &str, expected: Expected) {
        if text.contains("${") || text.contains('}') {
            self.reference(text, path, expected);
        } else {
            self.literal_field(text, path, expected);
        }
    }

    fn literal(
        &mut self,
        value: &Value,
        path: &str,
        allow_references: bool,
        depth: usize,
        remaining: &mut usize,
    ) {
        if depth > MAX_VALUE_DEPTH || *remaining == 0 {
            self.issue(
                path,
                "value_limit",
                "Value exceeds the nesting or element limit.",
            );
            return;
        }
        *remaining -= 1;
        match value {
            Value::String(text) if text.contains("${") || text.contains('}') => {
                if allow_references {
                    self.reference(text, path, Expected::Any);
                } else {
                    self.issue(path, "reference", "Variables cannot contain references.");
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.literal(
                        item,
                        &format!("{path}[{index}]"),
                        allow_references,
                        depth + 1,
                        remaining,
                    );
                    if *remaining == 0 {
                        break;
                    }
                }
            }
            Value::Object(items) => {
                for (index, (key, item)) in items.iter().enumerate() {
                    if key.contains("${") || key.contains('}') {
                        self.issue(path, "reference", "Object keys cannot contain references.");
                    }
                    self.literal(
                        item,
                        &format!("{path}[{index}]"),
                        allow_references,
                        depth + 1,
                        remaining,
                    );
                    if *remaining == 0 {
                        break;
                    }
                }
            }
            Value::Number(_) => self.issue(path, "number", "Use strings for exact numeric values."),
            _ => {}
        }
    }

    fn arguments(&mut self, args: &[Value], path: &str) {
        let mut remaining = MAX_VALUE_ELEMENTS;
        for (index, value) in args.iter().enumerate() {
            self.literal(value, &format!("{path}[{index}]"), true, 0, &mut remaining);
            if remaining == 0 {
                break;
            }
        }
    }

    fn probes(&mut self, probes: &[StorageProbe], path: &str) {
        for (index, probe) in probes.iter().enumerate() {
            self.field(
                &probe.address,
                &format!("{path}[{index}].address"),
                Expected::Address,
            );
            self.field(
                &probe.slot,
                &format!("{path}[{index}].slot"),
                Expected::Quantity,
            );
            self.target(&probe.address);
        }
    }

    fn signer(&mut self, signer: &Signer) {
        if !self.summary.signers.contains(signer) {
            self.summary.signers.push(signer.clone());
        }
    }
    fn target(&mut self, target: &str) {
        if !self.summary.targets.iter().any(|item| item == target) {
            self.summary.targets.push(target.to_string());
        }
    }
    fn artifact(&mut self, id: &str, path: &str) {
        if !valid_id(id) {
            self.issue(path, "artifact_id", "Artifact identity must contain 1 to 128 ASCII letters, digits, underscores, or hyphens.");
        }
        if !self.summary.artifact_ids.iter().any(|item| item == id) {
            self.summary.artifact_ids.push(id.to_string());
        }
    }
    fn signature(&mut self, value: &str, path: &str, constructor: bool) {
        if !valid_signature(value, constructor) {
            self.issue(
                path,
                "signature",
                "Expected a canonical ABI signature with explicit types.",
            );
        }
    }
    fn common_call(&mut self, target: &str, signer: &Signer, value: &str, path: &str) {
        self.field(target, &format!("{path}.target"), Expected::Address);
        self.field(value, &format!("{path}.value"), Expected::Quantity);
        self.signer(signer);
        self.target(target);
    }
    fn read(&mut self, requests: u64) {
        self.summary.read_count += 1;
        self.summary.max_read_requests = self.summary.max_read_requests.saturating_add(requests);
    }

    fn step(&mut self, index: usize, step: &'a MacroStep) {
        let path = format!("$.steps[{index}].kind");
        match &step.kind {
            StepKind::DeployArtifact {
                artifact_id,
                constructor_signature,
                args,
                signer,
                value,
                gas_limit,
            } => {
                self.artifact(artifact_id, &format!("{path}.artifact_id"));
                self.signature(
                    constructor_signature,
                    &format!("{path}.constructor_signature"),
                    true,
                );
                self.arguments(args, &format!("{path}.args"));
                self.field(value, &format!("{path}.value"), Expected::Quantity);
                self.field(gas_limit, &format!("{path}.gas_limit"), Expected::Gas);
                self.signer(signer);
                self.summary.broadcast_count += 1;
            }
            StepKind::AbiCall {
                artifact_id,
                signature,
                mode,
                target,
                args,
                signer,
                value,
                gas_limit,
            } => {
                self.artifact(artifact_id, &format!("{path}.artifact_id"));
                self.signature(signature, &format!("{path}.signature"), false);
                self.common_call(target, signer, value, &path);
                self.field(gas_limit, &format!("{path}.gas_limit"), Expected::Gas);
                self.arguments(args, &format!("{path}.args"));
                match mode {
                    AbiCallMode::Read => self.read(1),
                    AbiCallMode::Send => self.summary.broadcast_count += 1,
                }
            }
            StepKind::RawCall {
                target,
                data,
                signer,
                value,
                gas_limit,
            }
            | StepKind::RawSend {
                target,
                data,
                signer,
                value,
                gas_limit,
            } => {
                self.common_call(target, signer, value, &path);
                self.field(data, &format!("{path}.data"), Expected::Calldata);
                self.field(gas_limit, &format!("{path}.gas_limit"), Expected::Gas);
                if matches!(&step.kind, StepKind::RawCall { .. }) {
                    self.read(1);
                } else {
                    self.summary.broadcast_count += 1;
                }
            }
            StepKind::StorageRead { target, slot } => {
                self.field(target, &format!("{path}.target"), Expected::Address);
                self.field(slot, &format!("{path}.slot"), Expected::Quantity);
                self.target(target);
                self.read(1);
            }
            StepKind::Snapshot { probes } => {
                self.probes(probes, &format!("{path}.probes"));
                self.summary.snapshot_count += 1;
                self.active_snapshots.insert(step.id.as_str());
                self.summary.max_read_requests = self
                    .summary
                    .max_read_requests
                    .saturating_add(probes.len() as u64);
            }
            StepKind::Revert { snapshot } => {
                self.summary.revert_count += 1;
                if let Some(Reference::Step { id, .. }) =
                    self.reference(snapshot, &format!("{path}.snapshot"), Expected::Snapshot)
                {
                    if let Some(StepKind::Snapshot { probes }) = self.previous.get(id.as_str()) {
                        if !probes.is_empty() {
                            self.verified_reverts.insert(step.id.as_str());
                            self.summary.max_read_requests = self
                                .summary
                                .max_read_requests
                                .saturating_add(probes.len() as u64);
                        }
                    }
                    let mut found = false;
                    for previous in &self.definition.steps {
                        if previous.id == id {
                            found = true;
                        }
                        if found {
                            self.active_snapshots.remove(previous.id.as_str());
                        }
                        if previous.id == step.id {
                            break;
                        }
                    }
                }
            }
            StepKind::GasSweep {
                target,
                data,
                signer,
                value,
                start,
                end,
                increment,
                max_attempts,
                predicate,
            } => {
                self.common_call(target, signer, value, &path);
                self.field(data, &format!("{path}.data"), Expected::Calldata);
                if *max_attempts == 0 || u64::from(*max_attempts) > MAX_SWEEP_ATTEMPTS {
                    self.issue(
                        &format!("{path}.max_attempts"),
                        "sweep_limit",
                        "Choose a maximum attempt count between 1 and 512 before running.",
                    );
                }
                match predicate {
                    Some(SweepPredicate::Equals { value }) if !valid_calldata(value) => {
                        self.issue(&format!("{path}.predicate.value"), "predicate", "Sweep equality requires literal even-length 0x-prefixed hexadecimal bytes.");
                    }
                    None => self.issue(
                        &format!("{path}.predicate"),
                        "predicate",
                        "Choose a sweep predicate before running.",
                    ),
                    _ => {}
                }
                let parsed = (parse_gas(start), parse_gas(end), parse_gas(increment));
                match parsed {
                    (Ok(start), Ok(end), Ok(increment)) if start > 0 && increment > 0 && end >= start => {
                        let span = end - start;
                        let attempts = span.checked_div(increment).and_then(|count| count.checked_add(1));
                        let Some(attempts) = attempts else {
                            self.issue(&path, "sweep_limit", "Gas sweep attempts exceed the supported limit.");
                            return;
                        };
                        if span > MAX_SWEEP_SPAN || attempts > MAX_SWEEP_ATTEMPTS {
                            self.issue(&path, "sweep_limit", "Gas sweep span and attempts must each be at most 512.");
                        }
                        if attempts > u64::from(*max_attempts) {
                            self.issue(&format!("{path}.max_attempts"), "sweep_attempts", "Maximum attempts must cover every value in the configured gas range.");
                        }
                        self.summary.max_sweep_attempts = self.summary.max_sweep_attempts.max(attempts);
                        self.summary.total_sweep_attempts = self.summary.total_sweep_attempts.saturating_add(attempts);
                        self.read(attempts);
                    }
                    _ => self.issue(&path, "sweep", "Sweep requires positive literal u64 start and increment, with end at least start."),
                }
            }
            StepKind::ProbeGasSweep {
                artifact_id,
                probe,
                router,
                data,
                token,
                treasury,
                signer,
                value,
                start,
                end,
                increment,
                max_attempts,
                outer_gas_margin,
                final_gas_margin,
            } => {
                self.artifact(artifact_id, &format!("{path}.artifact_id"));
                self.field(probe, &format!("{path}.probe"), Expected::Address);
                self.field(router, &format!("{path}.router"), Expected::Address);
                self.field(data, &format!("{path}.data"), Expected::Calldata);
                self.field(token, &format!("{path}.token"), Expected::Address);
                self.field(treasury, &format!("{path}.treasury"), Expected::Address);
                self.field(value, &format!("{path}.value"), Expected::Quantity);
                if value != "0" {
                    self.issue(&format!("{path}.value"), "probe_value", "GasProbe calibration requires zero value.");
                }
                self.signer(signer);
                self.target(probe);
                self.target(router);
                if *max_attempts == 0 || u64::from(*max_attempts) > MAX_SWEEP_ATTEMPTS {
                    self.issue(&format!("{path}.max_attempts"), "sweep_limit", "Choose 1 to 512 attempts.");
                }
                let parsed = (parse_gas(start), parse_gas(end), parse_gas(increment), parse_gas(outer_gas_margin), parse_gas(final_gas_margin));
                match parsed {
                    (Ok(start), Ok(end), Ok(increment), Ok(outer), Ok(final_margin))
                        if start > 0 && end >= start && increment > 0 && outer >= 100_000 && outer <= 4_000_000 && final_margin <= 65_536 && end.checked_add(outer).is_some() && end.checked_add(final_margin).is_some() => {
                            let attempts = (end - start) / increment + 1;
                            if attempts > u64::from(*max_attempts) || attempts > MAX_SWEEP_ATTEMPTS {
                                self.issue(&path, "sweep_limit", "The gas range exceeds the configured attempt limit.");
                            }
                            self.summary.max_sweep_attempts = self.summary.max_sweep_attempts.max(attempts);
                            self.summary.total_sweep_attempts = self.summary.total_sweep_attempts.saturating_add(attempts);
                            self.read(attempts);
                        }
                    _ => self.issue(&path, "sweep", "Use bounded exact gas values and margins for this probe."),
                }
            }
            StepKind::Assert {
                left,
                comparator,
                right,
            } => {
                self.reference(left, &format!("{path}.left"), Expected::Any);
                let binary = matches!(
                    comparator,
                    AssertComparator::Eq | AssertComparator::NotEq | AssertComparator::HashEq
                );
                if binary != right.is_some() {
                    self.issue(
                        &format!("{path}.right"),
                        "assert_arity",
                        "eq, not_eq and hash_eq require a right operand; unary comparators do not.",
                    );
                }
                if let Some(value) = right {
                    let mut remaining = MAX_VALUE_ELEMENTS;
                    self.literal(value, &format!("{path}.right"), true, 0, &mut remaining);
                }
            }
            StepKind::Delay { millis } if *millis > MAX_DELAY_MS => self.issue(
                &format!("{path}.millis"),
                "delay_limit",
                "Delay cannot exceed 30000 milliseconds.",
            ),
            StepKind::Delay { .. } => {}
        }
    }
}

fn valid_signature(text: &str, constructor: bool) -> bool {
    if text.len() > MAX_SIGNATURE_BYTES
        || !text.is_ascii()
        || text.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return false;
    }
    let Some(open) = text.find('(') else {
        return false;
    };
    if !text.ends_with(')') {
        return false;
    }
    let name = &text[..open];
    if constructor {
        if name != "constructor" {
            return false;
        }
    } else if name.is_empty()
        || !name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || byte == b'_'
                || byte == b'$'
                || (index > 0 && byte.is_ascii_digit())
        })
    {
        return false;
    }
    let args = &text[open + 1..text.len() - 1];
    if args.is_empty() {
        return true;
    }
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, byte) in args.bytes().enumerate() {
        match byte {
            b'(' | b'[' => {
                depth += 1;
                if depth > MAX_SIGNATURE_DEPTH {
                    return false;
                }
            }
            b')' | b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                if !canonical_type(&args[start..index]) {
                    return false;
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    depth == 0 && canonical_type(&args[start..])
}

fn balanced_type_delimiters(text: &str) -> bool {
    let mut stack = Vec::new();
    for byte in text.bytes() {
        match byte {
            b'(' | b'[' => stack.push(byte),
            b')' if stack.pop() != Some(b'(') => return false,
            b']' if stack.pop() != Some(b'[') => return false,
            _ => {}
        }
        if stack.len() > MAX_SIGNATURE_DEPTH {
            return false;
        }
    }
    stack.is_empty()
}

fn canonical_type(text: &str) -> bool {
    if text.is_empty() || !balanced_type_delimiters(text) {
        return false;
    }
    let Ok(kind) = Reader::read(text) else {
        return false;
    };
    valid_param_type(&kind) && Writer::write(&kind) == text
}

fn valid_param_type(kind: &ParamType) -> bool {
    match kind {
        ParamType::Int(bits) | ParamType::Uint(bits) => *bits >= 8 && *bits <= 256 && bits % 8 == 0,
        ParamType::FixedBytes(size) => *size >= 1 && *size <= 32,
        ParamType::Array(inner) => valid_param_type(inner),
        ParamType::FixedArray(inner, size) => *size > 0 && valid_param_type(inner),
        ParamType::Tuple(items) => !items.is_empty() && items.iter().all(valid_param_type),
        _ => true,
    }
}

#[cfg(test)]
impl MacroRun {
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        if !valid_id(self.run_id.as_str()) {
            push(&mut errors, "$.runId", "id", "Invalid run ID.");
        }
        if !valid_id(self.macro_id.as_str()) {
            push(&mut errors, "$.macroId", "id", "Invalid macro ID.");
        }
        if self.steps.len() > MAX_STEPS {
            push(
                &mut errors,
                "$.steps",
                "step_limit",
                "A run can contain at most 128 steps.",
            );
        }
        let mut ids = HashSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            if !valid_id(step.step_id.as_str()) {
                push(
                    &mut errors,
                    &format!("$.steps[{index}].stepId"),
                    "id",
                    "Invalid step ID.",
                );
            }
            if !ids.insert(step.step_id.as_str()) {
                push(
                    &mut errors,
                    &format!("$.steps[{index}].stepId"),
                    "duplicate_id",
                    "Step IDs must be unique.",
                );
            }
            if let Some(Value::Object(fields)) = &step.result {
                if fields
                    .keys()
                    .any(|field| !PUBLIC_OUTPUT_FIELDS.contains(&field.as_str()))
                {
                    push(
                        &mut errors,
                        &format!("$.steps[{index}].result"),
                        "result_field",
                        "Result objects may contain only allowlisted public fields.",
                    );
                }
            } else if step.result.is_some() {
                push(
                    &mut errors,
                    &format!("$.steps[{index}].result"),
                    "result_field",
                    "A public step result must be an object of allowlisted fields.",
                );
            }
        }
        let value = serde_json::to_value(self)
            .map_err(|_| vec![error("$", "encoding", "Run cannot be encoded.")])?;
        screen_value(&value, "$", 0, &mut WalkBudget::default(), &mut errors, ScreenScope::Public);
        screen_snapshot_handles(&value, "$", 0, &mut WalkBudget::default(), &mut errors);
        if !json_size(self, MAX_PUBLIC_VALUE_BYTES) {
            push(
                &mut errors,
                "$",
                "size_limit",
                "Run JSON exceeds 1 MiB or cannot be encoded.",
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn validate_for(&self, definition: &MacroDefinition) -> Result<(), Vec<ValidationError>> {
        self.validate()?;
        definition.validate()?;
        let mut errors = Vec::new();
        if self.macro_id != definition.id {
            push(
                &mut errors,
                "$.macroId",
                "run_macro",
                "Run belongs to a different macro.",
            );
        }
        let steps: HashMap<_, _> = definition
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| (step.id.as_str(), (index, step)))
            .collect();
        let mut snapshot_probes = HashMap::new();
        let mut verified_reverts = HashMap::new();
        for step in definition.steps.iter().filter(|step| step.enabled) {
            match &step.kind {
                StepKind::Snapshot { probes } => {
                    snapshot_probes.insert(step.id.as_str(), !probes.is_empty());
                }
                StepKind::Revert { snapshot } => {
                    if let Ok(Reference::Step { id, .. }) = parse_reference(snapshot) {
                        if snapshot_probes.get(id.as_str()) == Some(&true) {
                            verified_reverts.insert(step.id.as_str(), id);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut successful_snapshots = HashSet::new();
        let mut previous_index = None;
        for (index, result) in self.steps.iter().enumerate() {
            let path = format!("$.steps[{index}]");
            let Some(&(definition_index, step)) = steps.get(result.step_id.as_str()) else {
                push(
                    &mut errors,
                    &path,
                    "run_step",
                    "Run contains a step not present in the definition.",
                );
                continue;
            };
            if previous_index.is_some_and(|previous| definition_index <= previous) {
                push(
                    &mut errors,
                    &path,
                    "run_order",
                    "Run steps must follow definition order.",
                );
            }
            previous_index = Some(definition_index);
            if !step.enabled && result.status != RunStatus::Skipped {
                push(
                    &mut errors,
                    &path,
                    "run_step",
                    "Disabled steps must be skipped.",
                );
            }
            if matches!(
                step.kind,
                StepKind::Snapshot { .. } | StepKind::Revert { .. }
            ) && result.request.is_some()
            {
                push(
                    &mut errors,
                    &format!("{path}.request"),
                    "snapshot_handle",
                    "Snapshot and revert requests cannot be retained.",
                );
            }
            if matches!(step.kind, StepKind::Snapshot { .. }) && result.status == RunStatus::Success
            {
                successful_snapshots.insert(step.id.as_str());
            }
            if let Some(Value::Object(fields)) = &result.result {
                if !step.enabled
                    || !matches!(result.status, RunStatus::Success | RunStatus::RolledBack)
                {
                    push(
                        &mut errors,
                        &format!("{path}.result"),
                        "result_status",
                        "Only completed or rolled-back steps may retain result fields.",
                    );
                }
                if fields
                    .keys()
                    .any(|field| !output_fields(&step.kind).contains(&field.as_str()))
                {
                    push(
                        &mut errors,
                        &format!("{path}.result"),
                        "result_field",
                        "Result contains a field not exposed by this step.",
                    );
                }
                if matches!(step.kind, StepKind::Revert { .. })
                    && fields.contains_key("passed")
                    && (!verified_reverts
                        .get(step.id.as_str())
                        .is_some_and(|id| successful_snapshots.contains(id.as_str()))
                        || result.status != RunStatus::Success
                        || fields.get("passed") != Some(&Value::Bool(true)))
                {
                    push(
                        &mut errors,
                        &format!("{path}.result"),
                        "unverified_revert",
                        "Revert passed may only report a successful probe-verified rollback.",
                    );
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
#[path = "macro_model_tests.rs"]
mod tests;
