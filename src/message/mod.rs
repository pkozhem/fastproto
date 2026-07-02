//! The `Descriptor` pyclass: a compiled message descriptor plus the resolved
//! Python class references (`refs`) needed to reconstruct enum/message fields
//! on decode.
//!
//! Encoding never needs `refs` — a nested message is read straight off the
//! instance and recurses through its own `__fastproto__`. Only decoding needs
//! to know which class to build, so those references are filled in lazily by
//! the `message()` decorator once the whole module is defined (see `link`).

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyType};

use crate::descriptor::{FieldKind, MapValue, MessageDescriptor};
use crate::{decode, encode, parse};

#[pyclass]
pub struct Descriptor {
    pub(crate) inner: MessageDescriptor,
    /// field number -> Python class (an `IntEnum` or a message dataclass).
    pub(crate) refs: HashMap<u32, Py<PyAny>>,
    pub(crate) linked: bool,
}

#[pymethods]
impl Descriptor {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn is_linked(&self) -> bool {
        self.linked
    }

    /// `(field_number, short_type_name)` for every field that references
    /// another class (enum, message, or a map with an enum/message value).
    fn ref_fields(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for f in &self.inner.fields {
            let needs = match &f.kind {
                FieldKind::Enum | FieldKind::Message => true,
                FieldKind::Map { value, .. } => {
                    matches!(value, MapValue::Enum | MapValue::Message)
                }
                FieldKind::Scalar(_) => false,
            };
            if needs {
                if let Some(name) = &f.type_name {
                    out.push((f.number, name.clone()));
                }
            }
        }
        out
    }

    /// Store resolved `{field_number: class}` references and mark linked.
    fn link(&mut self, mapping: &Bound<'_, PyDict>) -> PyResult<()> {
        for (k, v) in mapping.iter() {
            let num: u32 = k.extract()?;
            self.refs.insert(num, v.unbind());
        }
        self.linked = true;
        Ok(())
    }

    /// Encode a message instance to protobuf wire bytes.
    fn encode<'py>(
        &self,
        py: Python<'py>,
        instance: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let mut buf = Vec::new();
        encode::encode_message(py, instance, &self.inner, &mut buf)?;
        Ok(PyBytes::new(py, &buf))
    }

    /// Decode wire bytes into a new instance of `cls`.
    fn decode<'py>(
        &self,
        py: Python<'py>,
        cls: &Bound<'py, PyType>,
        data: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        decode::decode_message(py, cls, &self.inner, &self.refs, data)
    }
}

/// Parse `DescriptorProto` bytes into a fresh, unlinked [`Descriptor`].
pub fn compile(data: &[u8]) -> PyResult<Descriptor> {
    let inner = parse::parse_message(data)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("bad descriptor: {e:?}")))?;
    Ok(Descriptor {
        inner,
        refs: HashMap::new(),
        linked: false,
    })
}
