use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyAny, PyBytes, PyDict},
};
use revm::primitives::{Bytes, U256};
use sevm_revm_core::{
    DEFAULT_TARGET, DebugEvent, PauseReason, PrototypeSession, SessionConfig, SessionError,
    Snapshot,
};
use std::time::Duration;

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
struct RevmSession {
    inner: PrototypeSession,
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
            inner: PrototypeSession::start(config),
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
}

#[pyfunction]
fn revm_version() -> &'static str {
    "43.0.2"
}

#[pymodule]
mod _revm {
    #[pymodule_export]
    use super::{RevmSession, revm_version};
}
