use pyo3::prelude::*;

mod decode;
mod descriptor;
mod encode;
mod message;
mod parse;
mod wellknown;
mod wire;

use message::Descriptor;

/// Parse `DescriptorProto` bytes into a reusable [`Descriptor`]. Called once per
/// message by the `message()` decorator at import time.
#[pyfunction]
fn compile_descriptor(data: &[u8]) -> PyResult<Descriptor> {
    message::compile(data)
}

/// The native core module, imported by the `fastproto` Python package as
/// `fastproto._core`.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Descriptor>()?;
    m.add_function(wrap_pyfunction!(compile_descriptor, m)?)?;
    Ok(())
}
