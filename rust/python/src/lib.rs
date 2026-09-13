use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyAny, PyBytes, PyDict},
};
use revm::primitives::{Address, Bytes, U256};
use sevm_revm_core::{
    AccountSpec, Breakpoint, ChainConfig, DEFAULT_CALLER, DEFAULT_TARGET, DebugEngine, DebugEvent,
    PauseReason, SessionConfig, SessionError, Snapshot, TransactionRequest,
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

fn snapshot_dict<'py>(py: Python<'py>, snapshot: Snapshot) -> PyResult<Bound<'py, PyDict>> {
    let result = PyDict::new(py);
    result.set_item("type", "paused")?;
    result.set_item(
        "reason",
        match snapshot.reason {
            PauseReason::Breakpoint => "breakpoint",
            PauseReason::OutOfGas => "out_of_gas",
            PauseReason::Step => "step",
        },
    )?;
    result.set_item("address", format!("{:#x}", snapshot.address))?;
    result.set_item("depth", snapshot.depth)?;
    result.set_item("pc", snapshot.pc)?;
    result.set_item("opcode", snapshot.opcode)?;
    result.set_item("gas_remaining", snapshot.gas_remaining)?;
    result.set_item(
        "stack",
        snapshot.stack.into_iter().map(word).collect::<Vec<_>>(),
    )?;
    result.set_item("memory", PyBytes::new(py, &snapshot.memory))?;
    Ok(result)
}

fn event_dict<'py>(py: Python<'py>, event: DebugEvent) -> PyResult<Bound<'py, PyDict>> {
    match event {
        DebugEvent::Paused(snapshot) => snapshot_dict(py, snapshot),
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
                accounts: vec![caller],
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

    #[pyo3(signature = (init_code, caller=None, value=None, gas_limit=3_000_000))]
    fn create(
        &self,
        py: Python<'_>,
        init_code: &Bound<'_, PyBytes>,
        caller: Option<&str>,
        value: Option<&Bound<'_, PyAny>>,
        gas_limit: u64,
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
        py.detach(|| self.inner.transact(transaction))
            .map_err(python_error)
    }

    #[pyo3(signature = (address, calldata=None, caller=None, value=None, gas_limit=30_000_000))]
    fn call(
        &self,
        py: Python<'_>,
        address: &str,
        calldata: Option<&Bound<'_, PyBytes>>,
        caller: Option<&str>,
        value: Option<&Bound<'_, PyAny>>,
        gas_limit: u64,
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
    use super::{RevmChain, RevmSession, revm_version, serve_stdio};
}
