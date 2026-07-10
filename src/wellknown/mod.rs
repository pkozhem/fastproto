//! Native conversions for `google.protobuf.Timestamp` / `Duration`.
//!
//! Timestamp fields surface as Python `datetime` objects and Duration fields
//! as `timedelta` — no wrapper classes. Conversions go through Python-level
//! calls (never the `PyDateTime_*` C API, which is not part of the abi3
//! limited API). The math is exact integer arithmetic on the
//! days/seconds/microseconds components; only sub-microsecond precision is
//! lost (protobuf carries nanoseconds, `datetime`/`timedelta` microseconds).
//!
//! On the wire both types are ordinary messages: field 1 = `seconds` (varint),
//! field 2 = `nanos` (varint).

use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::PyDict;

use crate::wire::{self, Reader, WireType};

static EPOCH: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
static TIMEDELTA: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
static UTC: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

const NANOS_PER_SECOND: i128 = 1_000_000_000;

fn utc(py: Python<'_>) -> PyResult<&Py<PyAny>> {
    UTC.get_or_try_init(py, || {
        Ok::<_, PyErr>(py.import("datetime")?.getattr("timezone")?.getattr("utc")?.unbind())
    })
}

fn epoch(py: Python<'_>) -> PyResult<&Py<PyAny>> {
    EPOCH.get_or_try_init(py, || {
        let datetime = py.import("datetime")?.getattr("datetime")?;
        let tz = utc(py)?.bind(py);
        Ok::<_, PyErr>(datetime.call1((1970, 1, 1, 0, 0, 0, 0, tz))?.unbind())
    })
}

fn timedelta(py: Python<'_>) -> PyResult<&Py<PyAny>> {
    TIMEDELTA.get_or_try_init(py, || {
        Ok::<_, PyErr>(py.import("datetime")?.getattr("timedelta")?.unbind())
    })
}

/// Total nanoseconds represented by a Python `timedelta` (exact).
fn timedelta_total_nanos(delta: &Bound<'_, PyAny>) -> PyResult<i128> {
    let days: i128 = delta.getattr("days")?.extract()?;
    let seconds: i128 = delta.getattr("seconds")?.extract()?;
    let micros: i128 = delta.getattr("microseconds")?.extract()?;
    Ok((days * 86_400 + seconds) * NANOS_PER_SECOND + micros * 1_000)
}

/// Build a Python `timedelta` from whole seconds + (truncated) nanoseconds.
fn timedelta_from<'py>(py: Python<'py>, secs: i64, nanos: i32) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("seconds", secs)?;
    kwargs.set_item("microseconds", nanos / 1_000)?;
    timedelta(py)?.bind(py).call((), Some(&kwargs))
}

/// `datetime` -> Timestamp `(seconds, nanos)`; naive datetimes are read as UTC.
/// Per the protobuf spec, `nanos` is always in `[0, 999_999_999]`.
pub fn datetime_to_parts(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<(i64, i32)> {
    let aware = if value.getattr("tzinfo")?.is_none() {
        let kwargs = PyDict::new(py);
        kwargs.set_item("tzinfo", utc(py)?.bind(py))?;
        value.call_method("replace", (), Some(&kwargs))?
    } else {
        value.clone()
    };
    let delta = aware.call_method1("__sub__", (epoch(py)?.bind(py),))?;
    let total = timedelta_total_nanos(&delta)?;
    let secs = total.div_euclid(NANOS_PER_SECOND) as i64;
    let nanos = total.rem_euclid(NANOS_PER_SECOND) as i32;
    Ok((secs, nanos))
}

/// Timestamp `(seconds, nanos)` -> aware-UTC `datetime` (sub-µs truncated).
pub fn parts_to_datetime<'py>(
    py: Python<'py>,
    secs: i64,
    nanos: i32,
) -> PyResult<Bound<'py, PyAny>> {
    let delta = timedelta_from(py, secs, nanos)?;
    epoch(py)?.bind(py).call_method1("__add__", (delta,))
}

/// `timedelta` -> Duration `(seconds, nanos)`; per the spec, `seconds` and
/// `nanos` carry the same sign.
pub fn timedelta_to_parts(value: &Bound<'_, PyAny>) -> PyResult<(i64, i32)> {
    let total = timedelta_total_nanos(value)?;
    // Rust's `/` and `%` truncate toward zero -> same-sign parts, as required.
    Ok(((total / NANOS_PER_SECOND) as i64, (total % NANOS_PER_SECOND) as i32))
}

/// Duration `(seconds, nanos)` -> `timedelta` (sub-µs truncated).
pub fn parts_to_timedelta<'py>(
    py: Python<'py>,
    secs: i64,
    nanos: i32,
) -> PyResult<Bound<'py, PyAny>> {
    timedelta_from(py, secs, nanos)
}

/// Encode `(seconds, nanos)` as Timestamp/Duration message bytes.
pub fn encode_parts(buf: &mut Vec<u8>, secs: i64, nanos: i32) {
    if secs != 0 {
        wire::write_tag(buf, 1, WireType::Varint);
        wire::write_varint(buf, secs as u64);
    }
    if nanos != 0 {
        wire::write_tag(buf, 2, WireType::Varint);
        wire::write_varint(buf, nanos as i64 as u64);
    }
}

/// Decode Timestamp/Duration message bytes into `(seconds, nanos)`.
pub fn decode_parts(data: &[u8]) -> Result<(i64, i32), crate::wire::WireError> {
    let mut reader = Reader::new(data);
    let (mut secs, mut nanos) = (0_i64, 0_i32);
    while !reader.is_empty() {
        let (number, wire_type) = reader.read_tag()?;
        match (number, wire_type) {
            (1, WireType::Varint) => secs = reader.read_varint()? as i64,
            (2, WireType::Varint) => nanos = reader.read_varint()? as i64 as i32,
            (_, w) => reader.skip(w)?,
        }
    }
    Ok((secs, nanos))
}

#[cfg(test)]
mod tests;
