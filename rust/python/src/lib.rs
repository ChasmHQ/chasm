use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyAny, PyBytes, PyDict, PyList},
};
use revm::{
    bytecode::OpCode,
    primitives::{Address, B256, Bytes, U256},
};
use sevm_revm_core::{
    AccountSpec, Breakpoint, CHEATCODE_ADDRESS, CONSOLE_ADDRESS, ChainConfig, CommandValue,
    DEFAULT_CALLER, DEFAULT_TARGET, DebugEngine, DebugEvent, FrameContext, FrameKind, PauseReason,
    PrankConfig, SessionConfig, SessionError, Snapshot, StateCommand, TransactionRequest,
};
use std::{io, str::FromStr, time::Duration};

fn python_error(error: SessionError) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn parse_word(value: &Bound<'_, PyAny>) -> PyResult<U256> {
    let text = value.str()?.to_str()?.to_owned();
    let (digits, radix) = text
        .strip_prefix("0x")
        .map_or((text.as_str(), 10), |digits| (digits, 16));
    U256::from_str_radix(digits, radix)
        .map_err(|error| PyValueError::new_err(format!("invalid EVM word: {error}")))
}

fn parse_address(value: &str) -> PyResult<Address> {
    Address::from_str(value)
        .map_err(|error| PyValueError::new_err(format!("invalid address: {error}")))
}

fn word(value: U256) -> String {
    format!("0x{value:x}")
}

fn parse_hash(value: &Bound<'_, PyBytes>) -> PyResult<B256> {
    if value.len()? != 32 {
        return Err(PyValueError::new_err("EVM hashes must contain 32 bytes"));
    }
    Ok(B256::from_slice(value.as_bytes()))
}

fn state_value(
    engine: &DebugEngine,
    py: Python<'_>,
    command: StateCommand,
) -> PyResult<CommandValue> {
    py.detach(|| engine.state(command)).map_err(python_error)
}

fn state_word(engine: &DebugEngine, py: Python<'_>, command: StateCommand) -> PyResult<String> {
    match state_value(engine, py, command)? {
        CommandValue::Word(value) => Ok(word(value)),
        _ => unreachable!(),
    }
}

fn state_number(engine: &DebugEngine, py: Python<'_>, command: StateCommand) -> PyResult<u64> {
    match state_value(engine, py, command)? {
        CommandValue::Number(value) => Ok(value),
        _ => unreachable!(),
    }
}

fn state_bytes(engine: &DebugEngine, py: Python<'_>, command: StateCommand) -> PyResult<Bytes> {
    match state_value(engine, py, command)? {
        CommandValue::Bytes(value) => Ok(value),
        _ => unreachable!(),
    }
}

fn state_bool(engine: &DebugEngine, py: Python<'_>, command: StateCommand) -> PyResult<bool> {
    match state_value(engine, py, command)? {
        CommandValue::Bool(value) => Ok(value),
        _ => unreachable!(),
    }
}

fn frame_kind(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::Call => "call",
        FrameKind::CallCode => "callcode",
        FrameKind::DelegateCall => "delegatecall",
        FrameKind::StaticCall => "staticcall",
        FrameKind::Create => "create",
        FrameKind::Create2 => "create2",
    }
}

fn frame_dict<'py>(py: Python<'py>, frame: FrameContext) -> PyResult<Bound<'py, PyDict>> {
    let result = PyDict::new(py);
    result.set_item("depth", frame.depth)?;
    result.set_item("kind", frame_kind(frame.kind))?;
    result.set_item("address", format!("{:#x}", frame.address))?;
    result.set_item("code_address", format!("{:#x}", frame.code_address))?;
    result.set_item("caller", format!("{:#x}", frame.caller))?;
    result.set_item("value", word(frame.value))?;
    result.set_item("calldata", PyBytes::new(py, &frame.calldata))?;
    result.set_item("is_static", frame.is_static)?;
    result.set_item("pc", frame.pc)?;
    result.set_item("opcode", frame.opcode)?;
    result.set_item("gas_limit", frame.gas_limit)?;
    result.set_item("gas_remaining", frame.gas_remaining)?;
    result.set_item("code", PyBytes::new(py, &frame.code))?;
    Ok(result)
}

fn snapshot_dict<'py>(py: Python<'py>, snapshot: Snapshot) -> PyResult<Bound<'py, PyDict>> {
    let result = PyDict::new(py);
    result.set_item("type", "paused")?;
    result.set_item(
        "reason",
        match snapshot.reason {
            PauseReason::Breakpoint => "breakpoint",
            PauseReason::OutOfGas => "out_of_gas",
            PauseReason::Revert => "revert",
            PauseReason::Step => "step",
        },
    )?;
    result.set_item("step", snapshot.step)?;
    result.set_item("address", format!("{:#x}", snapshot.address))?;
    result.set_item("code_address", format!("{:#x}", snapshot.code_address))?;
    result.set_item("caller", format!("{:#x}", snapshot.caller))?;
    result.set_item("origin", format!("{:#x}", snapshot.origin))?;
    result.set_item("value", word(snapshot.value))?;
    result.set_item("calldata", PyBytes::new(py, &snapshot.calldata))?;
    result.set_item("is_static", snapshot.is_static)?;
    result.set_item("depth", snapshot.depth)?;
    result.set_item("pc", snapshot.pc)?;
    result.set_item("opcode", snapshot.opcode)?;
    result.set_item("mnemonic", snapshot.mnemonic)?;
    result.set_item("gas_limit", snapshot.gas_limit)?;
    result.set_item("gas_remaining", snapshot.gas_remaining)?;
    result.set_item("gas_used", snapshot.gas_used)?;
    result.set_item("gas_refund", snapshot.gas_refund)?;
    result.set_item(
        "stack",
        snapshot.stack.into_iter().map(word).collect::<Vec<_>>(),
    )?;
    result.set_item("memory_size", snapshot.memory_size)?;
    result.set_item("memory", PyBytes::new(py, &snapshot.memory))?;
    let frames = PyList::empty(py);
    for frame in snapshot.frames {
        frames.append(frame_dict(py, frame)?)?;
    }
    result.set_item("frames", frames)?;
    Ok(result)
}

fn event_dict<'py>(py: Python<'py>, event: DebugEvent) -> PyResult<Bound<'py, PyDict>> {
    match event {
        DebugEvent::Paused(snapshot) => snapshot_dict(py, *snapshot),
        DebugEvent::HostCall(call) => {
            let result = PyDict::new(py);
            result.set_item("type", "host_call")?;
            result.set_item("address", format!("{:#x}", call.address))?;
            result.set_item("caller", format!("{:#x}", call.caller))?;
            result.set_item("data", PyBytes::new(py, &call.data))?;
            result.set_item("gas_limit", call.gas_limit)?;
            Ok(result)
        }
        DebugEvent::Finished(finished) => {
            let result = PyDict::new(py);
            result.set_item("type", "finished")?;
            result.set_item("success", finished.success)?;
            result.set_item("gas_used", finished.gas_used)?;
            result.set_item("output", PyBytes::new(py, &finished.output))?;
            result.set_item(
                "created_address",
                finished
                    .created_address
                    .map(|address| format!("{address:#x}")),
            )?;
            let storage = finished
                .storage
                .into_iter()
                .map(|slot| {
                    (
                        format!("{:#x}", slot.address),
                        word(slot.key),
                        word(slot.value),
                    )
                })
                .collect::<Vec<_>>();
            result.set_item("storage", storage)?;
            let logs = PyList::empty(py);
            for log in finished.logs {
                let item = PyDict::new(py);
                item.set_item("address", format!("{:#x}", log.address))?;
                item.set_item(
                    "topics",
                    log.topics
                        .into_iter()
                        .map(|topic| word(U256::from_be_slice(topic.as_slice())))
                        .collect::<Vec<_>>(),
                )?;
                item.set_item("data", PyBytes::new(py, &log.data))?;
                logs.append(item)?;
            }
            result.set_item("logs", logs)?;
            Ok(result)
        }
        DebugEvent::Failed(message) => {
            let result = PyDict::new(py);
            result.set_item("type", "failed")?;
            result.set_item("error", message)?;
            Ok(result)
        }
    }
}

#[pyclass(module = "sevm._revm")]
struct RevmChain {
    inner: DebugEngine,
}

#[pymethods]
impl RevmChain {
    #[new]
    fn new() -> Self {
        let mut caller = AccountSpec::new(DEFAULT_CALLER, Bytes::new());
        caller.balance = U256::MAX;
        Self {
            inner: DebugEngine::new(ChainConfig {
                accounts: vec![
                    caller,
                    AccountSpec::new(CHEATCODE_ADDRESS, Bytes::from_static(&[0x00])),
                    AccountSpec::new(CONSOLE_ADDRESS, Bytes::from_static(&[0x00])),
                ],
                breakpoints: Vec::new(),
            }),
        }
    }

    fn set_breakpoints(
        &self,
        py: Python<'_>,
        breakpoints: Vec<(String, usize)>,
    ) -> PyResult<usize> {
        let breakpoints = breakpoints
            .into_iter()
            .map(|(address, pc)| {
                Ok(Breakpoint {
                    address: parse_address(&address)?,
                    pc,
                })
            })
            .collect::<PyResult<Vec<_>>>()?;
        py.detach(|| self.inner.set_breakpoints(breakpoints))
            .map_err(python_error)
    }

    #[pyo3(signature = (init_code, caller=None, value=None, gas_limit=3_000_000, commit=true))]
    fn create(
        &self,
        py: Python<'_>,
        init_code: &Bound<'_, PyBytes>,
        caller: Option<&str>,
        value: Option<&Bound<'_, PyAny>>,
        gas_limit: u64,
        commit: bool,
    ) -> PyResult<()> {
        let mut transaction = TransactionRequest::create(
            caller
                .map(parse_address)
                .transpose()?
                .unwrap_or(DEFAULT_CALLER),
            Bytes::copy_from_slice(init_code.as_bytes()),
        );
        transaction.value = value.map(parse_word).transpose()?.unwrap_or(U256::ZERO);
        transaction.gas_limit = gas_limit;
        transaction.commit = commit;
        py.detach(|| self.inner.transact(transaction))
            .map_err(python_error)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, calldata=None, caller=None, value=None, gas_limit=30_000_000, commit=true))]
    fn call(
        &self,
        py: Python<'_>,
        address: &str,
        calldata: Option<&Bound<'_, PyBytes>>,
        caller: Option<&str>,
        value: Option<&Bound<'_, PyAny>>,
        gas_limit: u64,
        commit: bool,
    ) -> PyResult<()> {
        let mut transaction = TransactionRequest::call(
            caller
                .map(parse_address)
                .transpose()?
                .unwrap_or(DEFAULT_CALLER),
            parse_address(address)?,
            calldata
                .map(|data| Bytes::copy_from_slice(data.as_bytes()))
                .unwrap_or_default(),
        );
        transaction.value = value.map(parse_word).transpose()?.unwrap_or(U256::ZERO);
        transaction.gas_limit = gas_limit;
        transaction.commit = commit;
        py.detach(|| self.inner.transact(transaction))
            .map_err(python_error)
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait<'py>(&self, py: Python<'py>, timeout: f64) -> PyResult<Bound<'py, PyDict>> {
        if !timeout.is_finite() || timeout < 0.0 {
            return Err(PyValueError::new_err(
                "timeout must be finite and non-negative",
            ));
        }
        let event = py
            .detach(|| self.inner.wait(Duration::from_secs_f64(timeout)))
            .map_err(python_error)?;
        event_dict(py, event)
    }

    fn set_stack(
        &self,
        py: Python<'_>,
        index: usize,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let value = parse_word(value)?;
        py.detach(|| self.inner.set_stack(index, value))
            .map(word)
            .map_err(python_error)
    }

    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snapshot = py.detach(|| self.inner.snapshot()).map_err(python_error)?;
        snapshot_dict(py, snapshot)
    }

    fn write_memory(
        &self,
        py: Python<'_>,
        offset: usize,
        data: &Bound<'_, PyBytes>,
    ) -> PyResult<usize> {
        let data = Bytes::copy_from_slice(data.as_bytes());
        py.detach(|| self.inner.write_memory(offset, data))
            .map_err(python_error)
    }

    fn set_gas(&self, py: Python<'_>, value: u64) -> PyResult<u64> {
        py.detach(|| self.inner.set_gas(value))
            .map_err(python_error)
    }

    fn set_pc(&self, py: Python<'_>, value: usize) -> PyResult<usize> {
        py.detach(|| self.inner.set_pc(value)).map_err(python_error)
    }

    fn read_storage(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<String> {
        let key = parse_word(key)?;
        py.detach(|| self.inner.read_storage(key))
            .map(word)
            .map_err(python_error)
    }

    fn write_storage(
        &self,
        py: Python<'_>,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let key = parse_word(key)?;
        let value = parse_word(value)?;
        py.detach(|| self.inner.write_storage(key, value))
            .map(word)
            .map_err(python_error)
    }

    fn read_balance(&self, py: Python<'_>, address: &str) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::ReadBalance(parse_address(address)?),
        )
    }

    fn write_balance(
        &self,
        py: Python<'_>,
        address: &str,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteBalance {
                address: parse_address(address)?,
                value: parse_word(value)?,
            },
        )
    }

    fn read_code_at<'py>(&self, py: Python<'py>, address: &str) -> PyResult<Bound<'py, PyBytes>> {
        let value = state_bytes(
            &self.inner,
            py,
            StateCommand::ReadCode(parse_address(address)?),
        )?;
        Ok(PyBytes::new(py, &value))
    }

    fn write_code_at(
        &self,
        py: Python<'_>,
        address: &str,
        code: &Bound<'_, PyBytes>,
    ) -> PyResult<()> {
        state_value(
            &self.inner,
            py,
            StateCommand::WriteCode {
                address: parse_address(address)?,
                code: Bytes::copy_from_slice(code.as_bytes()),
            },
        )?;
        Ok(())
    }

    fn read_nonce(&self, py: Python<'_>, address: &str) -> PyResult<u64> {
        state_number(
            &self.inner,
            py,
            StateCommand::ReadNonce(parse_address(address)?),
        )
    }

    fn write_nonce(&self, py: Python<'_>, address: &str, value: u64) -> PyResult<u64> {
        state_number(
            &self.inner,
            py,
            StateCommand::WriteNonce {
                address: parse_address(address)?,
                value,
            },
        )
    }

    fn read_storage_at(
        &self,
        py: Python<'_>,
        address: &str,
        key: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::ReadStorage {
                address: parse_address(address)?,
                key: parse_word(key)?,
            },
        )
    }

    fn write_storage_at(
        &self,
        py: Python<'_>,
        address: &str,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteStorage {
                address: parse_address(address)?,
                key: parse_word(key)?,
                value: parse_word(value)?,
            },
        )
    }

    fn read_transient(
        &self,
        py: Python<'_>,
        address: &str,
        key: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::ReadTransient {
                address: parse_address(address)?,
                key: parse_word(key)?,
            },
        )
    }

    fn write_transient(
        &self,
        py: Python<'_>,
        address: &str,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteTransient {
                address: parse_address(address)?,
                key: parse_word(key)?,
                value: parse_word(value)?,
            },
        )
    }

    fn warm_storage(&self, py: Python<'_>, address: &str, key: &Bound<'_, PyAny>) -> PyResult<()> {
        state_value(
            &self.inner,
            py,
            StateCommand::WarmStorage {
                address: parse_address(address)?,
                key: parse_word(key)?,
            },
        )?;
        Ok(())
    }

    fn is_storage_warm(
        &self,
        py: Python<'_>,
        address: &str,
        key: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        state_bool(
            &self.inner,
            py,
            StateCommand::IsStorageWarm {
                address: parse_address(address)?,
                key: parse_word(key)?,
            },
        )
    }

    fn read_logs<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let CommandValue::Logs(logs) = state_value(&self.inner, py, StateCommand::Logs)? else {
            unreachable!()
        };
        let output = PyList::empty(py);
        for log in logs {
            let item = PyDict::new(py);
            item.set_item("address", format!("{:#x}", log.address))?;
            item.set_item(
                "topics",
                log.topics
                    .into_iter()
                    .map(|topic| word(U256::from_be_slice(topic.as_slice())))
                    .collect::<Vec<_>>(),
            )?;
            item.set_item("data", PyBytes::new(py, &log.data))?;
            output.append(item)?;
        }
        Ok(output)
    }

    fn read_block_number(&self, py: Python<'_>) -> PyResult<String> {
        state_word(&self.inner, py, StateCommand::ReadBlockNumber)
    }

    fn write_block_number(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteBlockNumber(parse_word(value)?),
        )
    }

    fn read_timestamp(&self, py: Python<'_>) -> PyResult<String> {
        state_word(&self.inner, py, StateCommand::ReadTimestamp)
    }

    fn write_timestamp(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteTimestamp(parse_word(value)?),
        )
    }

    fn read_base_fee(&self, py: Python<'_>) -> PyResult<u64> {
        state_number(&self.inner, py, StateCommand::ReadBaseFee)
    }

    fn write_base_fee(&self, py: Python<'_>, value: u64) -> PyResult<u64> {
        state_number(&self.inner, py, StateCommand::WriteBaseFee(value))
    }

    fn read_chain_id(&self, py: Python<'_>) -> PyResult<u64> {
        state_number(&self.inner, py, StateCommand::ReadChainId)
    }

    fn write_chain_id(&self, py: Python<'_>, value: u64) -> PyResult<u64> {
        state_number(&self.inner, py, StateCommand::WriteChainId(value))
    }

    fn read_coinbase(&self, py: Python<'_>) -> PyResult<String> {
        let value = state_bytes(&self.inner, py, StateCommand::ReadCoinbase)?;
        Ok(format!("{:#x}", Address::from_slice(&value)))
    }

    fn write_coinbase(&self, py: Python<'_>, value: &str) -> PyResult<String> {
        let address = parse_address(value)?;
        state_value(&self.inner, py, StateCommand::WriteCoinbase(address))?;
        Ok(format!("{address:#x}"))
    }

    fn read_prevrandao<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let value = state_bytes(&self.inner, py, StateCommand::ReadPrevrandao)?;
        Ok(PyBytes::new(py, &value))
    }

    fn write_prevrandao<'py>(
        &self,
        py: Python<'py>,
        value: &Bound<'_, PyBytes>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let hash = parse_hash(value)?;
        let output = state_bytes(&self.inner, py, StateCommand::WritePrevrandao(hash))?;
        Ok(PyBytes::new(py, &output))
    }

    fn read_difficulty(&self, py: Python<'_>) -> PyResult<String> {
        state_word(&self.inner, py, StateCommand::ReadDifficulty)
    }

    fn write_difficulty(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
        state_word(
            &self.inner,
            py,
            StateCommand::WriteDifficulty(parse_word(value)?),
        )
    }

    #[pyo3(signature = (new_sender=None, caller=None, persistent=false, new_origin=None, delegate=false))]
    fn configure_prank(
        &self,
        py: Python<'_>,
        new_sender: Option<&str>,
        caller: Option<&str>,
        persistent: bool,
        new_origin: Option<&str>,
        delegate: bool,
    ) -> PyResult<()> {
        let prank = new_sender
            .map(|new_sender| -> PyResult<PrankConfig> {
                Ok(PrankConfig {
                    caller: caller.map(parse_address).transpose()?,
                    new_sender: parse_address(new_sender)?,
                    persistent,
                    new_origin: new_origin.map(parse_address).transpose()?,
                    delegate,
                })
            })
            .transpose()?;
        py.detach(|| self.inner.set_prank(prank))
            .map_err(python_error)
    }

    #[pyo3(signature = (bytecode, keep=false))]
    fn evaluate<'py>(
        &self,
        py: Python<'py>,
        bytecode: &Bound<'_, PyBytes>,
        keep: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytecode = Bytes::copy_from_slice(bytecode.as_bytes());
        let output = py
            .detach(|| self.inner.evaluate(bytecode, keep))
            .map_err(python_error)?;
        Ok(PyBytes::new(py, &output))
    }

    #[pyo3(signature = (bytecode, data, caller, value=None, gas_limit=1_000_000, keep=false))]
    #[allow(clippy::too_many_arguments)]
    fn evaluate_call<'py>(
        &self,
        py: Python<'py>,
        bytecode: &Bound<'_, PyBytes>,
        data: &Bound<'_, PyBytes>,
        caller: &str,
        value: Option<&Bound<'_, PyAny>>,
        gas_limit: u64,
        keep: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let bytecode = Bytes::copy_from_slice(bytecode.as_bytes());
        let data = Bytes::copy_from_slice(data.as_bytes());
        let caller = parse_address(caller)?;
        let value = value.map(parse_word).transpose()?.unwrap_or(U256::ZERO);
        let result = py
            .detach(|| {
                self.inner
                    .evaluate_call(bytecode, data, caller, value, gas_limit, keep)
            })
            .map_err(python_error)?;
        let output = PyDict::new(py);
        output.set_item("success", result.success)?;
        output.set_item("output", PyBytes::new(py, &result.output))?;
        output.set_item("gas_used", result.gas_used)?;
        Ok(output)
    }

    #[pyo3(signature = (opcode, arguments, outputs=1))]
    fn execute_opcode<'py>(
        &self,
        py: Python<'py>,
        opcode: u8,
        arguments: &Bound<'_, PyAny>,
        outputs: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let arguments = arguments
            .try_iter()?
            .map(|item| parse_word(&item?))
            .collect::<PyResult<Vec<_>>>()?;
        let result = py
            .detach(|| self.inner.execute_opcode(opcode, arguments, outputs))
            .map_err(python_error)?;
        let output = PyDict::new(py);
        output.set_item("value", result.value.map(word))?;
        output.set_item("gas_used", result.gas_used)?;
        Ok(output)
    }

    #[pyo3(signature = (output=None, revert=false))]
    fn respond_host(
        &self,
        py: Python<'_>,
        output: Option<&Bound<'_, PyBytes>>,
        revert: bool,
    ) -> PyResult<()> {
        let output = output
            .map(|data| Bytes::copy_from_slice(data.as_bytes()))
            .unwrap_or_default();
        py.detach(|| self.inner.respond_host(output, revert))
            .map_err(python_error)
    }

    fn resume(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.inner.resume()).map_err(python_error)
    }

    #[pyo3(signature = (count=1))]
    fn step(&self, py: Python<'_>, count: usize) -> PyResult<()> {
        py.detach(|| self.inner.step(count)).map_err(python_error)
    }
}

#[pyclass(module = "sevm._revm")]
struct RevmSession {
    inner: DebugEngine,
}

#[pymethods]
impl RevmSession {
    #[new]
    #[pyo3(signature = (bytecode, stop_pc, gas_limit=100_000))]
    fn new(bytecode: &Bound<'_, PyBytes>, stop_pc: usize, gas_limit: u64) -> Self {
        let mut config =
            SessionConfig::new(DEFAULT_TARGET, Bytes::copy_from_slice(bytecode.as_bytes()))
                .with_breakpoint(DEFAULT_TARGET, stop_pc);
        config.gas_limit = gas_limit;
        Self {
            inner: DebugEngine::start(config),
        }
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait<'py>(&self, py: Python<'py>, timeout: f64) -> PyResult<Bound<'py, PyDict>> {
        if !timeout.is_finite() || timeout < 0.0 {
            return Err(PyValueError::new_err(
                "timeout must be finite and non-negative",
            ));
        }
        let event = py
            .detach(|| self.inner.wait(Duration::from_secs_f64(timeout)))
            .map_err(python_error)?;
        event_dict(py, event)
    }

    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snapshot = py.detach(|| self.inner.snapshot()).map_err(python_error)?;
        snapshot_dict(py, snapshot)
    }

    fn set_stack(
        &self,
        py: Python<'_>,
        index: usize,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let value = parse_word(value)?;
        py.detach(|| self.inner.set_stack(index, value))
            .map(word)
            .map_err(python_error)
    }

    fn write_memory(
        &self,
        py: Python<'_>,
        offset: usize,
        data: &Bound<'_, PyBytes>,
    ) -> PyResult<usize> {
        let data = Bytes::copy_from_slice(data.as_bytes());
        py.detach(|| self.inner.write_memory(offset, data))
            .map_err(python_error)
    }

    fn set_gas(&self, py: Python<'_>, value: u64) -> PyResult<u64> {
        py.detach(|| self.inner.set_gas(value))
            .map_err(python_error)
    }

    fn set_pc(&self, py: Python<'_>, value: usize) -> PyResult<usize> {
        py.detach(|| self.inner.set_pc(value)).map_err(python_error)
    }

    fn read_storage(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<String> {
        let key = parse_word(key)?;
        py.detach(|| self.inner.read_storage(key))
            .map(word)
            .map_err(python_error)
    }

    fn write_storage(
        &self,
        py: Python<'_>,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let key = parse_word(key)?;
        let value = parse_word(value)?;
        py.detach(|| self.inner.write_storage(key, value))
            .map(word)
            .map_err(python_error)
    }

    #[pyo3(signature = (bytecode, keep=false))]
    fn evaluate<'py>(
        &self,
        py: Python<'py>,
        bytecode: &Bound<'_, PyBytes>,
        keep: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytecode = Bytes::copy_from_slice(bytecode.as_bytes());
        let output = py
            .detach(|| self.inner.evaluate(bytecode, keep))
            .map_err(python_error)?;
        Ok(PyBytes::new(py, &output))
    }

    #[pyo3(signature = (output=None, revert=false))]
    fn respond_host(
        &self,
        py: Python<'_>,
        output: Option<&Bound<'_, PyBytes>>,
        revert: bool,
    ) -> PyResult<()> {
        let output = output
            .map(|data| Bytes::copy_from_slice(data.as_bytes()))
            .unwrap_or_default();
        py.detach(|| self.inner.respond_host(output, revert))
            .map_err(python_error)
    }

    fn resume(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.inner.resume()).map_err(python_error)
    }

    #[pyo3(signature = (count=1))]
    fn step(&self, py: Python<'_>, count: usize) -> PyResult<()> {
        py.detach(|| self.inner.step(count)).map_err(python_error)
    }
}

#[pyfunction]
fn revm_version() -> &'static str {
    "43.0.2"
}

#[pyfunction]
fn opcode_names<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    for opcode in 0..=u8::MAX {
        let name = OpCode::name_by_op(opcode);
        if name != "Unknown" {
            output.set_item(opcode, name)?;
        }
    }
    Ok(output)
}

#[pyfunction]
fn serve_stdio(py: Python<'_>) -> PyResult<()> {
    py.detach(|| {
        let stdin = io::stdin();
        let stdout = io::stdout();
        sevm_revm_headless::serve(stdin.lock(), stdout.lock())
    })
    .map_err(|error| PyRuntimeError::new_err(error.to_string()))
}

#[pymodule]
mod _revm {
    #[pymodule_export]
    use super::{RevmChain, RevmSession, opcode_names, revm_version, serve_stdio};
}
