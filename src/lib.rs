use pyo3::prelude::*;

// Experimental: route this crate's Rust-side allocations (output buffers,
// descriptor tables) through mimalloc instead of the system allocator.
// Python-object allocations still go through CPython's own allocator, so this
// only affects the native buffers.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
fn compile_descriptor(py: Python<'_>, data: &[u8]) -> PyResult<Descriptor> {
    message::compile(py, data)
}

/// The native core module, imported by the `fastproto` Python package as
/// `fastproto._core`.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Descriptor>()?;
    m.add_function(wrap_pyfunction!(compile_descriptor, m)?)?;
    Ok(())
}
