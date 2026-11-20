use super::call_result::CallResult;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub contract: String,
    pub address: String,
    pub deployment_id: Option<String>,
    pub network_epoch: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ContractView {
    pub target: Option<Target>,
    pub selected_function: i32,
    pub arguments: String,
    pub value: String,
    /// What the user typed and its unit (0 wei, 1 gwei, 2 ether); `value` holds the wei.
    pub value_amount: String,
    pub value_unit: i32,
    pub value_error: String,
    pub slot: String,
    pub attach_address: String,
    pub section: i32,
    pub result_view: i32,
    pub interact_view: i32,
    pub result: Option<CallResult>,
    pub storage: String,
    pub storage_raw: Option<Value>,
    pub storage_generation: u64,
    pub storage_busy: bool,
    pub storage_status: String,
    /// Named variables decoded at `storage_block`, and the mapping key reader.
    pub variables: Vec<super::storage_layout::Variable>,
    pub variables_busy: bool,
    pub variables_status: String,
    pub variables_generation: u64,
    pub mapping_variable: String,
    pub mapping_key: String,
    pub call_generation: u64,
    pub result_generation: u64,
    pub call_status: String,
    /// eth_call object of the last Send that reverted before broadcast, so
    /// the step debugger can replay it. Cleared when the next call starts.
    pub reverted_call: Option<Value>,
    /// What the attached address runs, when it is a proxy (or why that is
    /// unknown). Empty until checked.
    pub proxy_note: String,
    /// Where this tab's ABI came from: the workspace build or a verified import.
    pub abi_origin: String,
    pub scroll: std::collections::HashMap<String, f32>,
    pub function_width: f32,
    pub result_height: f32,
    pub revision: u64,
}

impl ContractView {
    pub fn new(target: Option<Target>) -> Self {
        Self {
            target,
            selected_function: -1,
            value: "0".into(),
            slot: "0".into(),
            function_width: 240.0,
            result_height: 220.0,
            ..Default::default()
        }
    }
}

pub fn new_document_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("document-{}", ethers::utils::hex::encode(bytes))
}
