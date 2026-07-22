//! The `Descriptor` pyclass: a compiled message descriptor plus the resolved
//! Python class references (`refs`) needed to reconstruct enum/message fields
//! on decode.
//!
//! Encoding never strictly needs `refs` — a nested message is read straight
//! off the instance and recurses through its own `__fastproto__`. Only
//! decoding needs to know which class to build, so those references are
//! filled in lazily by the `message()` decorator once the whole module is
//! defined (see `link`).
//!
//! `refs`/`linked` use interior mutability (`OnceLock` + `AtomicBool`) so that
//! linking mutates through a shared `&self`. That keeps every method borrow as
//! a shared borrow: a first-time `link` can never clash with a concurrent
//! `is_linked` read or an in-flight `decode` (which would raise `PyBorrowError`
//! under free-threading if linking took `&mut self`). Linking is idempotent —
//! the first `OnceLock::set` wins and a racing second one is harmlessly dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple, PyType};

use crate::descriptor::{FieldIndex, FieldKind, MapValue, MessageDescriptor};
use crate::{decode, encode, parse};

/// How many low enum values the direct member table covers. Proto enums are
/// overwhelmingly small and zero-based, so most lookups become an array index.
pub(crate) const ENUM_TABLE_SIZE: usize = 64;

/// A resolved reference for one enum/message field, captured at link time so
/// the hot decode/encode paths avoid repeated attribute lookups.
pub(crate) struct LinkedRef {
    /// The Python class (an `IntEnum` or a message dataclass).
    pub(crate) class: Py<PyAny>,
    /// For enums without a custom `_missing_`: the class's
    /// `_value2member_map_` dict, so an int coerces to its member with one
    /// dict lookup instead of an `EnumMeta.__call__` + try/except.
    pub(crate) enum_map: Option<Py<PyDict>>,
    /// Members with values in `0..ENUM_TABLE_SIZE`, indexed by value; `None`
    /// entry = no such member (open-enum int fallback). Built alongside
    /// `enum_map`; values outside the table fall back to the dict.
    pub(crate) enum_table: Option<Vec<Option<Py<PyAny>>>>,
    /// For message classes: the class's own compiled descriptor, saving a
    /// `__fastproto__` getattr + downcast per nested value.
    pub(crate) desc: Option<Py<Descriptor>>,
}

/// Everything decode's `__new__` + setattr construction path needs, verified
/// and captured at link time (see `_ensure_linked` on the Python side).
pub(crate) struct FastInit {
    /// The exact class the fast path was verified for; decode compares its
    /// target class against this by pointer.
    pub(crate) cls: Py<PyAny>,
    /// Pre-built `(cls,)` so each `object.__new__(cls)` call skips the
    /// per-call argument-tuple allocation.
    pub(crate) new_args: Py<PyTuple>,
    /// Per-field dataclass default objects, index-aligned with the fields.
    /// Entries for repeated/map fields are a placeholder `None` — the decoder
    /// always pre-creates those accumulators, so their slot is never consulted.
    pub(crate) defaults: Vec<Py<PyAny>>,
}

pub(crate) type Refs = HashMap<u32, LinkedRef>;

#[pyclass]
pub struct Descriptor {
    pub(crate) inner: MessageDescriptor,
    /// Interned attribute-name objects, index-aligned with `inner.fields`.
    /// Interning makes the per-field getattr/setattr in encode/decode a plain
    /// pointer-keyed lookup with no string allocation.
    pub(crate) field_names: Vec<Py<PyString>>,
    /// Field-number -> field-index lookup for the decoder.
    pub(crate) field_index: FieldIndex,
    /// Written once by `link`; read by `decode` (and `encode` for nested
    /// message fast paths).
    pub(crate) refs: OnceLock<Refs>,
    pub(crate) linked: AtomicBool,
    /// Decode's `__new__` + setattr construction context. Set at link time
    /// only when the Python side verified the class is a plain generated
    /// dataclass; a subclass sharing this descriptor gets the normal
    /// `__init__` path (see [`FastInit`]).
    pub(crate) fast_init: OnceLock<FastInit>,
    /// Byte length of the most recent encode() output, used to pre-size the
    /// next output buffer. Purely a heuristic — any stale value is harmless.
    pub(crate) last_size: AtomicUsize,
}

static ENUM_DEFAULT_MISSING: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

/// `enum.Enum._missing_.__func__` — the default "no such value" hook. An enum
/// whose `_missing_` is this exact function cannot observe coercion, so decode
/// may bypass `EnumMeta.__call__` with a `_value2member_map_` lookup.
fn enum_default_missing(py: Python<'_>) -> PyResult<&Py<PyAny>> {
    ENUM_DEFAULT_MISSING.get_or_try_init(py, || {
        py.import("enum")?
            .getattr("Enum")?
            .getattr("_missing_")?
            .getattr("__func__")
            .map(Bound::unbind)
    })
}

impl Descriptor {
    pub(crate) fn refs(&self) -> Option<&Refs> {
        self.refs.get()
    }
}

#[pymethods]
impl Descriptor {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn is_linked(&self) -> bool {
        self.linked.load(Ordering::Acquire)
    }

    /// Field attribute names in declaration order. Used by the Python side to
    /// verify a dataclass matches the descriptor before enabling fast init.
    fn field_names(&self) -> Vec<String> {
        self.inner.fields.iter().map(|f| f.name.clone()).collect()
    }

    /// `(field_number, qualified_type_name)` for every field that references
    /// another class (enum, message, or a map with an enum/message value). The
    /// name is the full proto path (e.g. `pkg.Outer.Inner`); Python resolves it
    /// against the generated module, walking into nested classes as needed.
    fn ref_fields(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for f in &self.inner.fields {
            let needs = match &f.kind {
                FieldKind::Enum | FieldKind::Message => true,
                FieldKind::Map { value, .. } => {
                    matches!(value, MapValue::Enum | MapValue::Message)
                }
                // Native well-known types surface as datetime/timedelta —
                // there is no generated class to link.
                FieldKind::Scalar(_) | FieldKind::Timestamp | FieldKind::Duration => false,
            };
            if needs {
                if let Some(name) = &f.type_name {
                    out.push((f.number, name.clone()));
                }
            }
        }
        out
    }

    /// `{oneof_group_name: [member_field_name, ...]}` for every real oneof.
    ///
    /// Members are listed in field-declaration order. Synthetic single-member
    /// groups that protoc generates for proto3 `optional` are already excluded
    /// by the parser, so only user-written `oneof` groups appear here.
    fn oneofs(&self) -> Vec<(String, Vec<String>)> {
        let mut groups: Vec<(String, Vec<String>)> = self
            .inner
            .oneofs
            .iter()
            .map(|name| (name.clone(), Vec::new()))
            .collect();
        for f in &self.inner.fields {
            if let Some(idx) = f.oneof_index {
                groups[idx as usize].1.push(f.name.clone());
            }
        }
        groups
    }

    /// Store resolved `{field_number: class}` references and mark linked.
    ///
    /// `fast_init`, when given, is `(cls, defaults)`: the class the Python
    /// side verified as a plain generated dataclass, and the per-field list of
    /// its dataclass default objects (index-aligned with the descriptor's
    /// fields). Its presence enables decode's `__new__` + setattr construction
    /// fast path for exactly that class.
    ///
    /// Takes `&self` (interior mutability) so it never needs an exclusive borrow.
    /// If two threads race the first link, the first `OnceLock::set` wins and the
    /// other is dropped — both compute the same mapping, so the result is identical.
    #[pyo3(signature = (mapping, fast_init = None))]
    fn link(
        &self,
        py: Python<'_>,
        mapping: &Bound<'_, PyDict>,
        fast_init: Option<(Py<PyAny>, Vec<Py<PyAny>>)>,
    ) -> PyResult<()> {
        let mut refs = HashMap::with_capacity(mapping.len());
        for (k, v) in mapping.iter() {
            let num: u32 = k.extract()?;
            refs.insert(num, build_linked_ref(py, &v)?);
        }
        if let Some((cls, defaults)) = fast_init {
            if defaults.len() != self.inner.fields.len() {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "fast_init defaults must have one entry per field",
                ));
            }
            let new_args = PyTuple::new(py, [cls.bind(py)])?.unbind();
            let _ = self.fast_init.set(FastInit {
                cls,
                new_args,
                defaults,
            });
        }
        let _ = self.refs.set(refs);
        self.linked.store(true, Ordering::Release);
        Ok(())
    }

    /// Encode a message instance to protobuf wire bytes.
    fn encode<'py>(
        &self,
        py: Python<'py>,
        instance: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let hint = self.last_size.load(Ordering::Relaxed);
        let mut buf = Vec::with_capacity(hint.max(16));
        encode::encode_message(py, instance, self, &mut buf, 0)?;
        self.last_size.store(buf.len(), Ordering::Relaxed);
        Ok(PyBytes::new(py, &buf))
    }

    /// Decode wire bytes into a new instance of `cls`.
    fn decode<'py>(
        &self,
        py: Python<'py>,
        cls: &Bound<'py, PyType>,
        data: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        decode::decode_message(py, cls, self, data, 0)
    }
}

/// Inspect a linked class once and capture the fast-path handles the codec
/// needs (see [`LinkedRef`]).
fn build_linked_ref(py: Python<'_>, class: &Bound<'_, PyAny>) -> PyResult<LinkedRef> {
    // Message classes carry their compiled descriptor.
    if let Ok(handle) = class.getattr(pyo3::intern!(py, "__fastproto__")) {
        if let Ok(desc) = handle.downcast_into::<Descriptor>() {
            return Ok(LinkedRef {
                class: class.clone().unbind(),
                enum_map: None,
                enum_table: None,
                desc: Some(desc.unbind()),
            });
        }
    }
    // Enum classes expose _value2member_map_; only take the shortcut when the
    // class keeps the default `_missing_` (a custom hook must stay observable).
    let mut enum_map = None;
    let mut enum_table = None;
    if let Ok(map) = class.getattr(pyo3::intern!(py, "_value2member_map_")) {
        if let (Ok(map), Ok(missing)) = (
            map.downcast_into::<PyDict>(),
            class.getattr(pyo3::intern!(py, "_missing_")),
        ) {
            let default_missing = enum_default_missing(py)?;
            let is_default = missing
                .getattr(pyo3::intern!(py, "__func__"))
                .is_ok_and(|f| f.is(default_missing.bind(py)));
            if is_default {
                let mut table: Vec<Option<Py<PyAny>>> =
                    (0..ENUM_TABLE_SIZE).map(|_| None).collect();
                for (k, v) in map.iter() {
                    if let Ok(value) = k.extract::<i64>() {
                        if let Ok(slot) = usize::try_from(value) {
                            if slot < ENUM_TABLE_SIZE {
                                table[slot] = Some(v.unbind());
                            }
                        }
                    }
                }
                enum_table = Some(table);
                enum_map = Some(map.unbind());
            }
        }
    }
    Ok(LinkedRef {
        class: class.clone().unbind(),
        enum_map,
        enum_table,
        desc: None,
    })
}

/// Parse `DescriptorProto` bytes into a fresh, unlinked [`Descriptor`].
pub fn compile(py: Python<'_>, data: &[u8]) -> PyResult<Descriptor> {
    let inner = parse::parse_message(data)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("bad descriptor: {e:?}")))?;
    let field_names = inner
        .fields
        .iter()
        .map(|f| PyString::intern(py, &f.name).unbind())
        .collect();
    let field_index = FieldIndex::build(&inner.fields);
    Ok(Descriptor {
        inner,
        field_names,
        field_index,
        refs: OnceLock::new(),
        linked: AtomicBool::new(false),
        fast_init: OnceLock::new(),
        last_size: AtomicUsize::new(0),
    })
}
