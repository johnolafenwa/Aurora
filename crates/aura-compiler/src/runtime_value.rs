use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::{c_void, OsStr};
use std::fmt;
use std::fs::{File as StdFile, OpenOptions};
use std::io::{self, BufRead, Read, Seek, Write};
use std::net::{
    Shutdown, SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream, ToSocketAddrs,
    UdpSocket as StdUdpSocket,
};
use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::{
    Child as StdChild, ChildStderr as StdChildStderr, ChildStdin as StdChildStdin,
    ChildStdout as StdChildStdout, Command as StdCommand, ExitStatus as StdExitStatus,
    Stdio as StdProcessStdio,
};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, Weak};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use base64::Engine as _;
use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use httparse::{
    Request as HttpParseRequest, Response as HttpParseResponse, Status as HttpParseStatus,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{
    accept_with_config as websocket_accept_with_config, client_tls_with_config, Message, WebSocket,
};
use url::Url;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::bytes_codec::{self, BytesCodecError, BytesDataError, BytesResourceError};
use crate::diag::{Diagnostic, Result, Span};
use crate::integer::{IntegerBounds, IntegerKind, IntegerValue};
use crate::json_codec::{self, JsonCodecError, JsonValue};
use crate::randomness::{DeterministicRng, InvalidRandomRange};
use crate::runtime_config::{blocking_io_pool_config, BlockingIoPoolConfig};
use crate::runtime_reactor::{
    IoInterest, ReactorHandle, ReactorSubscription, ReactorSubscriptionKey, RuntimeReactor, WaitKey,
};
use crate::sema::Type;

type HttpHeaders = Vec<(String, String)>;
type HttpRequestHead = (usize, String, String, HttpHeaders, HttpBodyFraming);
type HttpResponseHead = (usize, i32, String, HttpHeaders, HttpBodyFraming);
type HttpRequestParts = (String, String, HttpHeaders, Vec<u8>);

pub(crate) const DIRECT_RUNTIME_TYPE_FIELD: &str = "\0aura:runtime-type";
pub(crate) const DIRECT_RUNTIME_TYPE_SEPARATOR: char = '\0';
pub(crate) const NANOS_PER_MILLISECOND: i128 = 1_000_000;
pub(crate) const NANOS_PER_SECOND: i128 = 1_000_000_000;
pub(crate) const NANOS_PER_MINUTE: i128 = 60 * NANOS_PER_SECOND;

pub(crate) fn validate_retry_runtime_policy(
    max_attempts: i32,
    initial_backoff: i128,
) -> Result<()> {
    if max_attempts < 1 {
        return Err(
            Diagnostic::coded("AU4003", "control.retry max_attempts must be at least 1")
                .with_help("pass max_attempts=1 to run the worker exactly once"),
        );
    }
    if initial_backoff < 0 {
        return Err(Diagnostic::coded(
            "AU4001",
            "control.retry initial_backoff cannot be negative",
        )
        .with_help("pass a zero or positive Duration"));
    }
    duration_to_host_timer(initial_backoff, "control.retry initial_backoff").map_err(|error| {
        Diagnostic::coded(
            "AU4001",
            format!(
                "control.retry initial_backoff is outside the host timer range: {}",
                error
            ),
        )
        .with_help("pass an initial_backoff representable by the current host timer")
    })?;
    Ok(())
}

pub(crate) fn next_retry_runtime_backoff(backoff: i128) -> Result<i128> {
    let next = backoff.checked_mul(2).ok_or_else(|| {
        Diagnostic::coded(
            "AU4002",
            "control.retry backoff overflowed the Duration range",
        )
    })?;
    duration_to_host_timer(next, "control.retry backoff").map_err(|error| {
        Diagnostic::coded(
            "AU4002",
            format!("control.retry backoff exceeded the host timer range: {error}"),
        )
    })?;
    Ok(next)
}

pub(crate) fn propagate_retry_task_cancellation() {
    if current_lightweight_task_cancellation().is_some_and(|context| context.is_cancelled()) {
        panic::panic_any(TaskCancelledSignal);
    }
}

pub(crate) fn render_duration(nanoseconds: i128) -> String {
    let negative = nanoseconds.is_negative();
    let magnitude = nanoseconds.unsigned_abs();
    let whole_milliseconds = magnitude / NANOS_PER_MILLISECOND as u128;
    let fractional_nanoseconds = magnitude % NANOS_PER_MILLISECOND as u128;
    let sign = if negative { "-" } else { "" };

    if fractional_nanoseconds == 0 {
        return format!("{sign}{whole_milliseconds}ms");
    }

    let fractional = format!("{fractional_nanoseconds:06}");
    format!(
        "{sign}{whole_milliseconds}.{}ms",
        fractional.trim_end_matches('0')
    )
}

pub(crate) fn duration_to_milliseconds(nanoseconds: i128) -> f64 {
    exact_i128_ratio_to_f64(nanoseconds, NANOS_PER_MILLISECOND as u128)
}

pub(crate) fn duration_to_seconds(nanoseconds: i128) -> f64 {
    exact_i128_ratio_to_f64(nanoseconds, NANOS_PER_SECOND as u128)
}

fn exact_i128_ratio_to_f64(numerator: i128, denominator: u128) -> f64 {
    debug_assert!(denominator > 0);
    if numerator == 0 {
        return 0.0;
    }

    let negative = numerator.is_negative();
    let numerator = numerator.unsigned_abs();
    let numerator_log2 = 127_i32 - numerator.leading_zeros() as i32;
    let denominator_log2 = 127_i32 - denominator.leading_zeros() as i32;
    let mut exponent = numerator_log2 - denominator_log2;
    let ratio_is_below_exponent = if exponent >= 0 {
        numerator < denominator << (exponent as u32)
    } else {
        numerator << ((-exponent) as u32) < denominator
    };
    if ratio_is_below_exponent {
        exponent -= 1;
    }

    let significand_shift = 52 - exponent;
    let (scaled_numerator, scaled_denominator) = if significand_shift >= 0 {
        (numerator << (significand_shift as u32), denominator)
    } else {
        (numerator, denominator << ((-significand_shift) as u32))
    };
    let mut significand = scaled_numerator / scaled_denominator;
    let remainder = scaled_numerator % scaled_denominator;
    let twice_remainder = remainder * 2;
    if twice_remainder > scaled_denominator
        || (twice_remainder == scaled_denominator && significand & 1 == 1)
    {
        significand += 1;
    }
    if significand == 1_u128 << 53 {
        significand >>= 1;
        exponent += 1;
    }

    debug_assert!((1_u128 << 52..1_u128 << 53).contains(&significand));
    let biased_exponent = u64::try_from(exponent + 1023)
        .expect("an i128 Duration ratio always has a normal f64 exponent");
    let fraction = u64::try_from(significand - (1_u128 << 52))
        .expect("a binary64 significand always fits its fraction field");
    let sign = if negative { 1_u64 << 63 } else { 0 };
    f64::from_bits(sign | (biased_exponent << 52) | fraction)
}

pub(crate) fn duration_to_host_timer(nanoseconds: i128, label: &str) -> io::Result<StdDuration> {
    if nanoseconds < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must be non-negative"),
        ));
    }

    let seconds = nanoseconds / NANOS_PER_SECOND;
    let seconds = u64::try_from(seconds).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} exceeds the host timer range"),
        )
    })?;
    let subsecond_nanoseconds = (nanoseconds % NANOS_PER_SECOND) as u32;
    let duration = StdDuration::new(seconds, subsecond_nanoseconds);
    checked_deadline_after(duration, label)?;
    Ok(duration)
}

fn checked_deadline_after_with<F>(
    now: Instant,
    duration: StdDuration,
    label: &str,
    checked_add: F,
) -> io::Result<Instant>
where
    F: FnOnce(Instant, StdDuration) -> Option<Instant>,
{
    checked_add(now, duration).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} exceeds the host deadline range"),
        )
    })
}

fn checked_deadline_after(duration: StdDuration, label: &str) -> io::Result<Instant> {
    checked_deadline_after_with(Instant::now(), duration, label, |now, duration| {
        now.checked_add(duration)
    })
}

pub(crate) fn nominal_runtime_base_name(name: &str) -> &str {
    name.split_once(DIRECT_RUNTIME_TYPE_SEPARATOR)
        .map_or(name, |(base, _)| base)
}

pub(crate) fn embedded_nominal_runtime_type_name(name: &str) -> Option<&str> {
    name.split_once(DIRECT_RUNTIME_TYPE_SEPARATOR)
        .map(|(_, runtime_type)| runtime_type)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HttpBodyFraming {
    ContentLength(usize),
    Chunked,
    UntilClose,
}

#[derive(Debug)]
pub enum Value {
    Int(IntegerValue),
    Float(f64),
    Bool(bool),
    String(String),
    Tuple(TupleValue),
    Vec(VecValue),
    Array(ArrayValue),
    Set(SetValue),
    Map(MapValue),
    Duration(i128),
    Rng(RngValue),
    Range(RangeValue),
    ModuleNamespace(ModuleNamespaceValue),
    Function(Box<FunctionValue>),
    Unit,
    FfiHandle(FfiHandleValue),
    Instance(InstanceValue),
    EnumVariant(EnumVariantValue),
    Channel(ChannelValue),
    Task(TaskValue),
    TaskGroup(TaskGroupValue),
    File(FileValue),
    TcpListener(TcpListenerValue),
    TcpStream(TcpStreamValue),
    UdpSocket(UdpSocketValue),
    UdpDatagram(UdpDatagramValue),
    HttpListener(HttpListenerValue),
    HttpExchange(HttpExchangeValue),
    HttpResponse(HttpResponseValue),
    WebSocketListener(WebSocketListenerValue),
    WebSocket(WebSocketValue),
    ProcessChild(ProcessChildValue),
    ProcessPipe(ProcessPipeValue),
    ProcessCompleted(ProcessCompletedValue),
    ProcessSupervisor(ProcessSupervisorValue),
    UnixListener(UnixListenerValue),
    UnixStream(UnixStreamValue),
    TlsListener(TlsListenerValue),
    TlsStream(TlsStreamValue),
}

/// A non-null foreign-owned opaque address tagged with its Aura nominal
/// type.
///
/// The address is deliberately stored as a non-zero integer rather than a raw
/// pointer so the general runtime value remains `Send`. Cloning this internal
/// transport value aliases the same foreign object; Aura's semantic move and
/// Transfer rules still prevent source programs from cloning or task-sending
/// an opaque handle.
#[derive(Clone)]
pub struct FfiHandleValue {
    type_name: String,
    address: NonZeroUsize,
}

impl fmt::Debug for FfiHandleValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfiHandleValue")
            .field("type_name", &self.type_name)
            .field("pointer", &"<opaque>")
            .finish()
    }
}

impl FfiHandleValue {
    pub(crate) fn new(type_name: String, pointer: *mut c_void) -> Option<Self> {
        Some(Self {
            type_name,
            address: NonZeroUsize::new(pointer as usize)?,
        })
    }

    pub(crate) fn type_name(&self) -> &str {
        &self.type_name
    }

    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.address.get() as *mut c_void
    }
}

/// A capture-free Aura function value.
///
/// MIR execution resolves `name` through its function table. Native execution
/// additionally carries the uniform boxed-argument thunk address used for
/// indirect calls and task starts. The source metadata keeps task ancestry
/// diagnostics as precise for calls through stored values as for direct names.
#[derive(Clone, Debug)]
pub struct FunctionValue {
    pub name: String,
    pub signature: Type,
    pub source_path: Option<String>,
    pub entry_span: Span,
    pub direct_thunk: Option<i64>,
    pub direct_default_binder: Option<i64>,
    /// Present only for capturing lambdas. Clones share this state so the
    /// interpreter's internal value snapshots cannot accidentally duplicate a
    /// single-use closure environment.
    pub closure_environment: Option<Arc<ClosureEnvironment>>,
}

#[derive(Clone, Debug)]
pub struct ClosureCaptureValue {
    pub name: String,
    pub ty: Type,
    pub value: Value,
    pub source_place: Option<String>,
    pub mutable: bool,
}

#[derive(Debug)]
pub struct ClosureEnvironment {
    captures: Mutex<Option<Vec<ClosureCaptureValue>>>,
    consuming: bool,
    consumed: Mutex<bool>,
}

impl ClosureEnvironment {
    pub fn new(captures: Vec<ClosureCaptureValue>, consuming: bool) -> Self {
        Self {
            captures: Mutex::new(Some(captures)),
            consuming,
            consumed: Mutex::new(false),
        }
    }

    pub(crate) fn arguments(&self, function_name: &str) -> Result<Vec<ClosureCaptureValue>> {
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.consuming {
            let mut consumed = self
                .consumed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *consumed {
                return Err(Diagnostic::new(format!(
                    "closure `{function_name}` has already consumed its captured environment"
                )));
            }
            *consumed = true;
            let captures = captures.as_mut().ok_or_else(|| {
                Diagnostic::new(format!(
                    "closure `{function_name}` has already consumed its captured environment"
                ))
            })?;
            return Ok(captures
                .iter_mut()
                .map(|capture| ClosureCaptureValue {
                    name: capture.name.clone(),
                    ty: capture.ty.clone(),
                    value: std::mem::replace(&mut capture.value, Value::Unit),
                    source_place: capture.source_place.clone(),
                    mutable: capture.mutable,
                })
                .collect());
        }
        let captures = captures
            .as_ref()
            .expect("repeatable closure environments remain initialized");
        let mut copied =
            try_array_buffer(captures.len(), "Array-containing closure environment copy")?;
        for capture in captures {
            copied.push(ClosureCaptureValue {
                name: capture.name.clone(),
                ty: capture.ty.clone(),
                value: try_clone_array_containing_value(&capture.value)?,
                source_place: capture.source_place.clone(),
                mutable: capture.mutable,
            });
        }
        Ok(copied)
    }

    pub(crate) fn write_back_mutable(&self, index: usize, value: Value) -> Result<()> {
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let captures = captures.as_mut().ok_or_else(|| {
            Diagnostic::new("cannot write back into a consumed closure environment")
        })?;
        let capture = captures
            .get_mut(index)
            .ok_or_else(|| Diagnostic::new(format!("closure has no capture at index {index}")))?;
        if !capture.mutable {
            return Err(Diagnostic::new(format!(
                "closure capture `{}` is not mutable",
                capture.name
            )));
        }
        capture.value = value;
        Ok(())
    }

    pub(crate) fn capture_value(&self, index: usize) -> Result<Value> {
        let captures = self
            .captures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let capture = captures
            .as_ref()
            .and_then(|captures| captures.get(index))
            .ok_or_else(|| {
                Diagnostic::new(format!("closure has no live capture at index {index}"))
            })?;
        try_clone_array_containing_value(&capture.value)
    }
}

impl PartialEq for FunctionValue {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.signature == other.signature
            && match (&self.closure_environment, &other.closure_environment) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

#[derive(Clone, Debug)]
pub struct InstanceValue {
    pub class_name: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct EnumVariantValue {
    pub enum_name: String,
    pub variant_name: String,
    pub payloads: Vec<Value>,
}

impl EnumVariantValue {
    pub(crate) fn single_payload(&self) -> Option<&Value> {
        match self.payloads.as_slice() {
            [payload] => Some(payload),
            _ => None,
        }
    }
}

impl PartialEq for InstanceValue {
    fn eq(&self, other: &Self) -> bool {
        if nominal_runtime_base_name(&self.class_name)
            != nominal_runtime_base_name(&other.class_name)
        {
            return false;
        }
        self.fields
            .iter()
            .filter(|(name, _)| name.as_str() != DIRECT_RUNTIME_TYPE_FIELD)
            .eq(other
                .fields
                .iter()
                .filter(|(name, _)| name.as_str() != DIRECT_RUNTIME_TYPE_FIELD))
    }
}

impl PartialEq for EnumVariantValue {
    fn eq(&self, other: &Self) -> bool {
        nominal_runtime_base_name(&self.enum_name) == nominal_runtime_base_name(&other.enum_name)
            && self.variant_name == other.variant_name
            && self.payloads == other.payloads
    }
}

#[derive(Clone, Debug)]
pub struct VecValue {
    pub element_type: Type,
    pub elements: Vec<Value>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum ArrayDType {
    Int32 = 0,
    Int64 = 1,
    Float32 = 2,
    Float64 = 3,
}

impl ArrayDType {
    pub const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Int32),
            1 => Some(Self::Int64),
            2 => Some(Self::Float32),
            3 => Some(Self::Float64),
            _ => None,
        }
    }

    pub const fn runtime_type_name(self) -> &'static str {
        match self {
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float32 => "float32",
            Self::Float64 => "float64",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArrayStorage {
    Int32(Box<[i32]>),
    Int64(Box<[i64]>),
    Float32(Box<[f32]>),
    Float64(Box<[f64]>),
}

impl ArrayStorage {
    pub const fn dtype(&self) -> ArrayDType {
        match self {
            Self::Int32(_) => ArrayDType::Int32,
            Self::Int64(_) => ArrayDType::Int64,
            Self::Float32(_) => ArrayDType::Float32,
            Self::Float64(_) => ArrayDType::Float64,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Int32(values) => values.len(),
            Self::Int64(values) => values.len(),
            Self::Float32(values) => values.len(),
            Self::Float64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayValue {
    pub shape: Box<[usize]>,
    pub storage: ArrayStorage,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum ArrayBinaryOp {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
}

impl ArrayBinaryOp {
    pub const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Add),
            1 => Some(Self::Sub),
            2 => Some(Self::Mul),
            3 => Some(Self::Div),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Add => "addition",
            Self::Sub => "subtraction",
            Self::Mul => "multiplication",
            Self::Div => "division",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum IntegerArithmeticMode {
    Checked = 0,
    Wrapping = 1,
    Saturating = 2,
}

impl IntegerArithmeticMode {
    pub const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Checked),
            1 => Some(Self::Wrapping),
            2 => Some(Self::Saturating),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum ArrayReduction {
    Sum = 0,
    Min = 1,
    Max = 2,
    Mean = 3,
}

impl ArrayReduction {
    pub const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Sum),
            1 => Some(Self::Min),
            2 => Some(Self::Max),
            3 => Some(Self::Mean),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        const NAMES: [&str; 4] = ["sum", "min", "max", "mean"];
        NAMES[self as usize]
    }
}

impl ArrayValue {
    pub fn new(shape: Box<[usize]>, storage: ArrayStorage) -> Result<Self> {
        if shape.is_empty() {
            return Err(Diagnostic::coded(
                "AU4007",
                "array rank must be at least one",
            ));
        }
        let element_count = checked_array_element_count(&shape)?;
        if element_count != storage.len() {
            return Err(Diagnostic::coded(
                "AU4007",
                format!(
                    "array shape {:?} requires {element_count} values, but storage contains {}",
                    shape,
                    storage.len()
                ),
            ));
        }
        Ok(Self { shape, storage })
    }

    pub fn from_vec(vector: &VecValue, shape: Option<&[usize]>) -> Result<Self> {
        let dtype = ArrayDType::from_type(&vector.element_type).ok_or_else(|| {
            Diagnostic::coded(
                "AU4007",
                format!(
                    "Array values require int32, int64, float32, or float64 elements, found `{}`",
                    vector.element_type
                ),
            )
        })?;
        let default_shape = [vector.elements.len()];
        let shape = shape.unwrap_or(&default_shape);
        let element_count = checked_array_shape(shape)?;
        if element_count != vector.elements.len() {
            return Err(Diagnostic::coded(
                "AU4007",
                format!(
                    "array shape {shape:?} requires {element_count} values, but storage contains {}",
                    vector.elements.len()
                ),
            ));
        }
        let shape = try_copy_array_storage(shape, "Array.from_list shape")?;
        Self::from_vec_with_shape(vector, dtype, shape)
    }

    fn from_vec_with_shape(
        vector: &VecValue,
        dtype: ArrayDType,
        shape: Box<[usize]>,
    ) -> Result<Self> {
        let storage = match dtype {
            ArrayDType::Int32 => ArrayStorage::Int32(try_collect_array_storage(
                vector.elements.len(),
                "Array.from_list",
                |index| array_int32_scalar(&vector.elements[index], index),
            )?),
            ArrayDType::Int64 => ArrayStorage::Int64(try_collect_array_storage(
                vector.elements.len(),
                "Array.from_list",
                |index| array_int64_scalar(&vector.elements[index], index),
            )?),
            ArrayDType::Float32 => ArrayStorage::Float32(try_collect_array_storage(
                vector.elements.len(),
                "Array.from_list",
                |index| array_float32_scalar(&vector.elements[index], index),
            )?),
            ArrayDType::Float64 => ArrayStorage::Float64(try_collect_array_storage(
                vector.elements.len(),
                "Array.from_list",
                |index| array_float64_scalar(&vector.elements[index], index),
            )?),
        };
        Self::new(shape, storage)
    }

    pub fn from_values(
        element_type: &Type,
        shape: Box<[usize]>,
        elements: Vec<Value>,
    ) -> Result<Self> {
        let vector = VecValue {
            element_type: element_type.clone(),
            elements,
        };
        let dtype = ArrayDType::from_type(element_type).ok_or_else(|| {
            Diagnostic::coded(
                "AU4007",
                format!(
                    "Array values require int32, int64, float32, or float64 elements, found `{element_type}`"
                ),
            )
        })?;
        Self::from_vec_with_shape(&vector, dtype, shape)
    }

    pub fn zeros(dtype: ArrayDType, shape: Box<[usize]>) -> Result<Self> {
        let len = checked_array_shape(&shape)?;
        let storage = match dtype {
            ArrayDType::Int32 => {
                ArrayStorage::Int32(try_filled_array_storage(len, 0, "Array.zeros")?)
            }
            ArrayDType::Int64 => {
                ArrayStorage::Int64(try_filled_array_storage(len, 0, "Array.zeros")?)
            }
            ArrayDType::Float32 => {
                ArrayStorage::Float32(try_filled_array_storage(len, 0.0, "Array.zeros")?)
            }
            ArrayDType::Float64 => {
                ArrayStorage::Float64(try_filled_array_storage(len, 0.0, "Array.zeros")?)
            }
        };
        Self::new(shape, storage)
    }

    pub fn full(dtype: ArrayDType, shape: Box<[usize]>, value: &Value) -> Result<Self> {
        let mut array = Self::zeros(dtype, shape)?;
        array.fill(value.clone())?;
        Ok(array)
    }

    pub const fn dtype(&self) -> ArrayDType {
        self.storage.dtype()
    }

    pub fn element_type(&self) -> Type {
        Type::named(self.dtype().runtime_type_name())
    }

    pub fn rank(&self) -> usize {
        self.shape.len()
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    pub fn try_shape(&self) -> Result<Box<[usize]>> {
        try_copy_array_storage(&self.shape, "Array shape")
    }

    pub fn try_clone(&self) -> Result<Self> {
        let shape = self.try_shape()?;
        let storage = match &self.storage {
            ArrayStorage::Int32(values) => {
                ArrayStorage::Int32(try_copy_array_storage(values, "Array.clone")?)
            }
            ArrayStorage::Int64(values) => {
                ArrayStorage::Int64(try_copy_array_storage(values, "Array.clone")?)
            }
            ArrayStorage::Float32(values) => {
                ArrayStorage::Float32(try_copy_array_storage(values, "Array.clone")?)
            }
            ArrayStorage::Float64(values) => {
                ArrayStorage::Float64(try_copy_array_storage(values, "Array.clone")?)
            }
        };
        Self::new(shape, storage)
    }

    pub fn shape_value(&self) -> Result<Value> {
        let mut elements = try_array_buffer(self.rank(), "Array.shape result")?;
        elements.extend(self.shape.iter().map(|dimension| {
            Value::Int(IntegerValue::from_i64(
                i64::try_from(*dimension).expect("live array dimensions fit the Aura int64 ABI"),
            ))
        }));
        Ok(Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements,
        }))
    }

    pub fn value_at_flat(&self, index: usize) -> Value {
        match &self.storage {
            ArrayStorage::Int32(values) => Value::Int(IntegerValue::from_i32(values[index])),
            ArrayStorage::Int64(values) => Value::Int(IntegerValue::from_i64(values[index])),
            ArrayStorage::Float32(values) => Value::Float(values[index] as f64),
            ArrayStorage::Float64(values) => Value::Float(values[index]),
        }
    }

    pub fn get(&self, coordinates: &[i64]) -> Result<Value> {
        Ok(self.value_at_flat(self.flat_index(coordinates)?))
    }

    pub fn get_optional(&self, coordinates: &[i64]) -> Result<Option<Value>> {
        Ok(self
            .flat_index(coordinates)
            .ok()
            .map(|index| self.value_at_flat(index)))
    }

    pub fn set(&mut self, coordinates: &[i64], value: Value) -> Result<Value> {
        let index = self.flat_index(coordinates)?;
        self.set_flat(index, value)
    }

    fn set_flat(&mut self, index: usize, value: Value) -> Result<Value> {
        let previous = self.value_at_flat(index);
        match &mut self.storage {
            ArrayStorage::Int32(values) => values[index] = array_int32_scalar(&value, index)?,
            ArrayStorage::Int64(values) => values[index] = array_int64_scalar(&value, index)?,
            ArrayStorage::Float32(values) => values[index] = array_float32_scalar(&value, index)?,
            ArrayStorage::Float64(values) => values[index] = array_float64_scalar(&value, index)?,
        }
        Ok(previous)
    }

    pub fn fill(&mut self, value: Value) -> Result<()> {
        match &mut self.storage {
            ArrayStorage::Int32(values) => values.fill(array_int32_scalar(&value, 0)?),
            ArrayStorage::Int64(values) => values.fill(array_int64_scalar(&value, 0)?),
            ArrayStorage::Float32(values) => values.fill(array_float32_scalar(&value, 0)?),
            ArrayStorage::Float64(values) => values.fill(array_float64_scalar(&value, 0)?),
        }
        Ok(())
    }

    pub fn slice_first_axis(&self, start: Option<i128>, end: Option<i128>) -> Result<Self> {
        let first_dimension = self.shape[0];
        let (start, end) = normalize_slice_bounds(start, end, first_dimension)?;
        let block_len = checked_array_element_count(&self.shape[1..])?;
        // A live Array has a checked row-major shape product. Slice endpoints
        // are bounded by the first dimension, so multiplying either endpoint
        // by the remaining-axis block length cannot exceed that live product.
        let value_start = start * block_len;
        let value_end = end * block_len;
        let mut shape = self.try_shape()?;
        shape[0] = end - start;
        let storage = match &self.storage {
            ArrayStorage::Int32(values) => ArrayStorage::Int32(try_copy_array_storage(
                &values[value_start..value_end],
                "array slice",
            )?),
            ArrayStorage::Int64(values) => ArrayStorage::Int64(try_copy_array_storage(
                &values[value_start..value_end],
                "array slice",
            )?),
            ArrayStorage::Float32(values) => ArrayStorage::Float32(try_copy_array_storage(
                &values[value_start..value_end],
                "array slice",
            )?),
            ArrayStorage::Float64(values) => ArrayStorage::Float64(try_copy_array_storage(
                &values[value_start..value_end],
                "array slice",
            )?),
        };
        Self::new(shape, storage)
    }

    pub fn binary(
        &self,
        other: &Self,
        operation: ArrayBinaryOp,
        mode: IntegerArithmeticMode,
    ) -> Result<Self> {
        if self.shape != other.shape || self.dtype() != other.dtype() {
            return Err(Diagnostic::coded(
                "AU4007",
                format!(
                    "array operands require the same shape and dtype, found {:?} {} and {:?} {}",
                    self.shape,
                    self.dtype().runtime_type_name(),
                    other.shape,
                    other.dtype().runtime_type_name()
                ),
            ));
        }
        let shape = self.try_shape()?;
        let storage = match (&self.storage, &other.storage) {
            (ArrayStorage::Int32(left), ArrayStorage::Int32(right)) => ArrayStorage::Int32(
                array_integer_binary(left, right, operation, mode, IntegerValue::from_i32)?
                    .into_boxed_slice(),
            ),
            (ArrayStorage::Int64(left), ArrayStorage::Int64(right)) => ArrayStorage::Int64(
                array_integer_binary(left, right, operation, mode, IntegerValue::from_i64)?
                    .into_boxed_slice(),
            ),
            (ArrayStorage::Float32(left), ArrayStorage::Float32(right)) => ArrayStorage::Float32(
                array_float32_binary(left, right, operation, mode)?.into_boxed_slice(),
            ),
            (ArrayStorage::Float64(left), ArrayStorage::Float64(right)) => ArrayStorage::Float64(
                array_float64_binary(left, right, operation, mode)?.into_boxed_slice(),
            ),
            _ => unreachable!("matching array dtypes imply matching storage variants"),
        };
        Self::new(shape, storage)
    }

    pub fn scalar_binary(
        &self,
        scalar: &Value,
        scalar_left: bool,
        operation: ArrayBinaryOp,
        mode: IntegerArithmeticMode,
    ) -> Result<Self> {
        let shape = self.try_shape()?;
        let storage = match &self.storage {
            ArrayStorage::Int32(values) => {
                let scalar = array_int32_scalar(scalar, 0)?;
                ArrayStorage::Int32(
                    array_integer_scalar_binary(
                        values,
                        scalar,
                        scalar_left,
                        operation,
                        mode,
                        IntegerValue::from_i32,
                    )?
                    .into_boxed_slice(),
                )
            }
            ArrayStorage::Int64(values) => {
                let scalar = array_int64_scalar(scalar, 0)?;
                ArrayStorage::Int64(
                    array_integer_scalar_binary(
                        values,
                        scalar,
                        scalar_left,
                        operation,
                        mode,
                        IntegerValue::from_i64,
                    )?
                    .into_boxed_slice(),
                )
            }
            ArrayStorage::Float32(values) => {
                let scalar = array_float32_scalar(scalar, 0)?;
                ArrayStorage::Float32(
                    array_float32_scalar_binary(values, scalar, scalar_left, operation, mode)?
                        .into_boxed_slice(),
                )
            }
            ArrayStorage::Float64(values) => {
                let scalar = array_float64_scalar(scalar, 0)?;
                ArrayStorage::Float64(
                    array_float64_scalar_binary(values, scalar, scalar_left, operation, mode)?
                        .into_boxed_slice(),
                )
            }
        };
        Self::new(shape, storage)
    }

    pub fn reduce(&self, reduction: ArrayReduction) -> Result<Value> {
        if self.is_empty() {
            return match (reduction, self.dtype()) {
                (ArrayReduction::Sum, ArrayDType::Int32) => {
                    Ok(Value::Int(IntegerValue::from_i32(0)))
                }
                (ArrayReduction::Sum, ArrayDType::Int64) => {
                    Ok(Value::Int(IntegerValue::from_i64(0)))
                }
                (ArrayReduction::Sum, _) => Ok(Value::Float(0.0)),
                _ => Err(Diagnostic::coded(
                    "AU4007",
                    format!(
                        "cannot compute array {} over an empty array",
                        reduction.name()
                    ),
                )),
            };
        }
        match &self.storage {
            ArrayStorage::Int32(values) => reduce_int32_array(values, reduction),
            ArrayStorage::Int64(values) => reduce_int64_array(values, reduction),
            ArrayStorage::Float32(values) => reduce_float32_array(values, reduction),
            ArrayStorage::Float64(values) => reduce_float64_array(values, reduction),
        }
    }

    pub fn render(&self) -> String {
        let values = match &self.storage {
            ArrayStorage::Int32(values) => {
                values.iter().map(ToString::to_string).collect::<Vec<_>>()
            }
            ArrayStorage::Int64(values) => {
                values.iter().map(ToString::to_string).collect::<Vec<_>>()
            }
            ArrayStorage::Float32(values) => {
                values.iter().map(|value| render_float32(*value)).collect()
            }
            ArrayStorage::Float64(values) => {
                values.iter().map(|value| render_float(*value)).collect()
            }
        };
        format!(
            "Array[{}](shape={:?}, values=[{}])",
            self.dtype().runtime_type_name(),
            self.shape,
            values.join(", ")
        )
    }

    fn flat_index(&self, coordinates: &[i64]) -> Result<usize> {
        if coordinates.len() != self.rank() {
            return Err(Diagnostic::coded(
                "AU4007",
                format!(
                    "array rank {} requires {} coordinates, found {}",
                    self.rank(),
                    self.rank(),
                    coordinates.len()
                ),
            ));
        }
        let mut flat_index = 0_usize;
        for (axis, (&coordinate, &dimension)) in
            coordinates.iter().zip(self.shape.iter()).enumerate()
        {
            let dimension_i128 = dimension as i128;
            let coordinate_i128 = coordinate as i128;
            let normalized = if coordinate_i128 < 0 {
                coordinate_i128 + dimension_i128
            } else {
                coordinate_i128
            };
            if !(0..dimension_i128).contains(&normalized) {
                return Err(Diagnostic::coded(
                    "AU4003",
                    format!(
                        "array coordinate {coordinate} on axis {axis} normalizes to {normalized}, outside 0..{dimension}"
                    ),
                ));
            }
            flat_index = flat_index
                .checked_mul(dimension)
                .and_then(|index| index.checked_add(normalized as usize))
                .expect("validated array shapes and coordinates produce valid row-major offsets");
        }
        Ok(flat_index)
    }
}

impl ArrayDType {
    pub fn from_type(ty: &Type) -> Option<Self> {
        match ty {
            Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
                "int32" => Some(Self::Int32),
                "int64" => Some(Self::Int64),
                "float32" => Some(Self::Float32),
                "float64" => Some(Self::Float64),
                _ => None,
            },
            _ => None,
        }
    }
}

fn checked_array_shape(shape: &[usize]) -> Result<usize> {
    if shape.is_empty() {
        return Err(Diagnostic::coded(
            "AU4007",
            "array rank must be at least one",
        ));
    }
    checked_array_element_count(shape)
}

fn checked_array_element_count(shape: &[usize]) -> Result<usize> {
    for dimension in shape {
        if *dimension > i64::MAX as usize {
            return Err(Diagnostic::coded(
                "AU4005",
                format!("array dimension {dimension} exceeds the Aura int64 shape limit"),
            ));
        }
    }
    if shape.contains(&0) {
        return Ok(0);
    }
    let mut count = 1_usize;
    for dimension in shape {
        count = count.checked_mul(*dimension).ok_or_else(|| {
            Diagnostic::coded(
                "AU4005",
                format!("array shape product overflows host allocation bounds: {shape:?}"),
            )
        })?;
    }
    Ok(count)
}

fn array_allocation_error(label: &str, len: usize) -> Diagnostic {
    Diagnostic::coded(
        "AU4005",
        format!("{label} could not allocate storage for {len} array elements"),
    )
}

#[cfg(test)]
std::thread_local! {
    static ARRAY_ALLOCATION_BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
}

#[cfg(test)]
struct ArrayAllocationBudgetGuard(Option<usize>);

#[cfg(test)]
impl Drop for ArrayAllocationBudgetGuard {
    fn drop(&mut self) {
        ARRAY_ALLOCATION_BUDGET.with(|budget| budget.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn with_array_allocation_budget<T>(
    successful_allocations: usize,
    operation: impl FnOnce() -> T,
) -> T {
    let previous =
        ARRAY_ALLOCATION_BUDGET.with(|budget| budget.replace(Some(successful_allocations)));
    let _guard = ArrayAllocationBudgetGuard(previous);
    operation()
}

pub(crate) fn try_array_buffer<T>(len: usize, label: &str) -> Result<Vec<T>> {
    #[cfg(test)]
    {
        let injected_failure = ARRAY_ALLOCATION_BUDGET.with(|budget| match budget.get() {
            Some(0) => true,
            Some(remaining) => {
                budget.set(Some(remaining - 1));
                false
            }
            None => false,
        });
        if injected_failure {
            return Err(array_allocation_error(label, len));
        }
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| array_allocation_error(label, len))?;
    Ok(values)
}

fn try_filled_array_storage<T: Clone>(len: usize, value: T, label: &str) -> Result<Box<[T]>> {
    let mut values = try_array_buffer(len, label)?;
    values.resize(len, value);
    Ok(values.into_boxed_slice())
}

fn try_copy_array_storage<T: Copy>(values: &[T], label: &str) -> Result<Box<[T]>> {
    let mut copy = try_array_buffer(values.len(), label)?;
    copy.extend_from_slice(values);
    Ok(copy.into_boxed_slice())
}

fn try_collect_array_storage<T>(
    len: usize,
    label: &str,
    mut value_at: impl FnMut(usize) -> Result<T>,
) -> Result<Box<[T]>> {
    let mut values = try_array_buffer(len, label)?;
    for index in 0..len {
        values.push(value_at(index)?);
    }
    Ok(values.into_boxed_slice())
}

fn array_dtype_error(expected: ArrayDType, value: &Value, index: usize) -> Diagnostic {
    Diagnostic::coded(
        "AU4007",
        format!(
            "array {} storage requires {} scalar at flat index {index}, found {}",
            expected.runtime_type_name(),
            expected.runtime_type_name(),
            value.render(),
        ),
    )
}

fn array_int32_scalar(value: &Value, index: usize) -> Result<i32> {
    let Value::Int(value) = value else {
        return Err(array_dtype_error(ArrayDType::Int32, value, index));
    };
    if !matches!(value.runtime_kind(), None | Some(IntegerKind::Int32)) {
        return Err(array_dtype_error(
            ArrayDType::Int32,
            &Value::Int(*value),
            index,
        ));
    }
    value
        .as_i128()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| array_dtype_error(ArrayDType::Int32, &Value::Int(*value), index))
}

fn array_int64_scalar(value: &Value, index: usize) -> Result<i64> {
    let Value::Int(value) = value else {
        return Err(array_dtype_error(ArrayDType::Int64, value, index));
    };
    if !matches!(value.runtime_kind(), None | Some(IntegerKind::Int64)) {
        return Err(array_dtype_error(
            ArrayDType::Int64,
            &Value::Int(*value),
            index,
        ));
    }
    value
        .as_i128()
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| array_dtype_error(ArrayDType::Int64, &Value::Int(*value), index))
}

fn array_float32_scalar(value: &Value, index: usize) -> Result<f32> {
    match value {
        Value::Float(value) => Ok(*value as f32),
        _ => Err(array_dtype_error(ArrayDType::Float32, value, index)),
    }
}

fn array_float64_scalar(value: &Value, index: usize) -> Result<f64> {
    match value {
        Value::Float(value) => Ok(*value),
        _ => Err(array_dtype_error(ArrayDType::Float64, value, index)),
    }
}

fn array_arithmetic_error(
    code: &'static str,
    operation: ArrayBinaryOp,
    index: usize,
    detail: &'static str,
) -> Diagnostic {
    Diagnostic::coded(
        code,
        format!("array {} {detail} at flat index {index}", operation.name()),
    )
}

fn apply_integer_array_operation(
    left: IntegerValue,
    right: IntegerValue,
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
    index: usize,
) -> Result<IntegerValue> {
    let result = match (mode, operation) {
        (IntegerArithmeticMode::Checked, ArrayBinaryOp::Add) => left.checked_add(right),
        (IntegerArithmeticMode::Checked, ArrayBinaryOp::Sub) => left.checked_sub(right),
        (IntegerArithmeticMode::Checked, ArrayBinaryOp::Mul) => left.checked_mul(right),
        (IntegerArithmeticMode::Wrapping, ArrayBinaryOp::Add) => left.wrapping_add(right),
        (IntegerArithmeticMode::Wrapping, ArrayBinaryOp::Sub) => left.wrapping_sub(right),
        (IntegerArithmeticMode::Wrapping, ArrayBinaryOp::Mul) => left.wrapping_mul(right),
        (IntegerArithmeticMode::Saturating, ArrayBinaryOp::Add) => left.saturating_add(right),
        (IntegerArithmeticMode::Saturating, ArrayBinaryOp::Sub) => left.saturating_sub(right),
        (IntegerArithmeticMode::Saturating, ArrayBinaryOp::Mul) => left.saturating_mul(right),
        (_, ArrayBinaryOp::Div) => {
            return Err(Diagnostic::coded(
                "AU4001",
                "array division is supported only for float32 and float64",
            ));
        }
    };
    result
        .filter(|result| result.runtime_kind() == left.runtime_kind())
        .ok_or_else(|| array_arithmetic_error("AU4002", operation, index, "overflowed"))
}

fn array_integer_binary<T>(
    left: &[T],
    right: &[T],
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
    to_integer: impl Fn(T) -> IntegerValue,
) -> Result<Vec<T>>
where
    T: Copy + TryFrom<i128>,
{
    let mut output = try_array_buffer(left.len(), "array arithmetic result")?;
    for (index, (&left, &right)) in left.iter().zip(right).enumerate() {
        let result = apply_integer_array_operation(
            to_integer(left),
            to_integer(right),
            operation,
            mode,
            index,
        )?;
        output.push(
            T::try_from(
                result
                    .as_i128()
                    .expect("int32 and int64 array results fit i128"),
            )
            .map_err(|_| array_arithmetic_error("AU4002", operation, index, "overflowed"))?,
        );
    }
    Ok(output)
}

fn array_integer_scalar_binary<T>(
    values: &[T],
    scalar: T,
    scalar_left: bool,
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
    to_integer: impl Fn(T) -> IntegerValue,
) -> Result<Vec<T>>
where
    T: Copy + TryFrom<i128>,
{
    let mut output = try_array_buffer(values.len(), "array arithmetic result")?;
    for (index, &value) in values.iter().enumerate() {
        let (left, right) = if scalar_left {
            (to_integer(scalar), to_integer(value))
        } else {
            (to_integer(value), to_integer(scalar))
        };
        let result = apply_integer_array_operation(left, right, operation, mode, index)?;
        output.push(
            T::try_from(
                result
                    .as_i128()
                    .expect("int32 and int64 array results fit i128"),
            )
            .map_err(|_| array_arithmetic_error("AU4002", operation, index, "overflowed"))?,
        );
    }
    Ok(output)
}

fn validate_float_array_mode(mode: IntegerArithmeticMode) -> Result<()> {
    if mode != IntegerArithmeticMode::Checked {
        return Err(Diagnostic::coded(
            "AU4001",
            "wrapping and saturating modes are supported only for integer arrays",
        ));
    }
    Ok(())
}

fn apply_float32_array_operation(
    left: f32,
    right: f32,
    operation: ArrayBinaryOp,
    index: usize,
) -> Result<f32> {
    if operation == ArrayBinaryOp::Div && right == 0.0 {
        return Err(array_arithmetic_error(
            "AU4004",
            operation,
            index,
            "has a zero divisor",
        ));
    }
    Ok(match operation {
        ArrayBinaryOp::Add => left + right,
        ArrayBinaryOp::Sub => left - right,
        ArrayBinaryOp::Mul => left * right,
        ArrayBinaryOp::Div => left / right,
    })
}

fn apply_float64_array_operation(
    left: f64,
    right: f64,
    operation: ArrayBinaryOp,
    index: usize,
) -> Result<f64> {
    if operation == ArrayBinaryOp::Div && right == 0.0 {
        return Err(array_arithmetic_error(
            "AU4004",
            operation,
            index,
            "has a zero divisor",
        ));
    }
    Ok(match operation {
        ArrayBinaryOp::Add => left + right,
        ArrayBinaryOp::Sub => left - right,
        ArrayBinaryOp::Mul => left * right,
        ArrayBinaryOp::Div => left / right,
    })
}

fn array_float32_binary(
    left: &[f32],
    right: &[f32],
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
) -> Result<Vec<f32>> {
    validate_float_array_mode(mode)?;
    let mut output = try_array_buffer(left.len(), "array arithmetic result")?;
    for (index, (&left, &right)) in left.iter().zip(right).enumerate() {
        output.push(apply_float32_array_operation(
            left, right, operation, index,
        )?);
    }
    Ok(output)
}

fn array_float64_binary(
    left: &[f64],
    right: &[f64],
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
) -> Result<Vec<f64>> {
    validate_float_array_mode(mode)?;
    let mut output = try_array_buffer(left.len(), "array arithmetic result")?;
    for (index, (&left, &right)) in left.iter().zip(right).enumerate() {
        output.push(apply_float64_array_operation(
            left, right, operation, index,
        )?);
    }
    Ok(output)
}

fn array_float32_scalar_binary(
    values: &[f32],
    scalar: f32,
    scalar_left: bool,
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
) -> Result<Vec<f32>> {
    validate_float_array_mode(mode)?;
    let mut output = try_array_buffer(values.len(), "array arithmetic result")?;
    for (index, &value) in values.iter().enumerate() {
        let (left, right) = if scalar_left {
            (scalar, value)
        } else {
            (value, scalar)
        };
        output.push(apply_float32_array_operation(
            left, right, operation, index,
        )?);
    }
    Ok(output)
}

fn array_float64_scalar_binary(
    values: &[f64],
    scalar: f64,
    scalar_left: bool,
    operation: ArrayBinaryOp,
    mode: IntegerArithmeticMode,
) -> Result<Vec<f64>> {
    validate_float_array_mode(mode)?;
    let mut output = try_array_buffer(values.len(), "array arithmetic result")?;
    for (index, &value) in values.iter().enumerate() {
        let (left, right) = if scalar_left {
            (scalar, value)
        } else {
            (value, scalar)
        };
        output.push(apply_float64_array_operation(
            left, right, operation, index,
        )?);
    }
    Ok(output)
}

fn reduce_int32_array(values: &[i32], reduction: ArrayReduction) -> Result<Value> {
    match reduction {
        ArrayReduction::Sum => {
            let mut sum = IntegerValue::from_i32(0);
            for (index, value) in values.iter().copied().enumerate() {
                sum = apply_integer_array_operation(
                    sum,
                    IntegerValue::from_i32(value),
                    ArrayBinaryOp::Add,
                    IntegerArithmeticMode::Checked,
                    index,
                )?;
            }
            Ok(Value::Int(sum))
        }
        ArrayReduction::Min => Ok(Value::Int(IntegerValue::from_i32(
            *values.iter().min().expect("empty arrays were rejected"),
        ))),
        ArrayReduction::Max => Ok(Value::Int(IntegerValue::from_i32(
            *values.iter().max().expect("empty arrays were rejected"),
        ))),
        ArrayReduction::Mean => {
            let sum = values
                .iter()
                .fold(0.0_f64, |sum, value| sum + *value as f64);
            Ok(Value::Float(sum / values.len() as f64))
        }
    }
}

fn reduce_int64_array(values: &[i64], reduction: ArrayReduction) -> Result<Value> {
    match reduction {
        ArrayReduction::Sum => {
            let mut sum = IntegerValue::from_i64(0);
            for (index, value) in values.iter().copied().enumerate() {
                sum = apply_integer_array_operation(
                    sum,
                    IntegerValue::from_i64(value),
                    ArrayBinaryOp::Add,
                    IntegerArithmeticMode::Checked,
                    index,
                )?;
            }
            Ok(Value::Int(sum))
        }
        ArrayReduction::Min => Ok(Value::Int(IntegerValue::from_i64(
            *values.iter().min().expect("empty arrays were rejected"),
        ))),
        ArrayReduction::Max => Ok(Value::Int(IntegerValue::from_i64(
            *values.iter().max().expect("empty arrays were rejected"),
        ))),
        ArrayReduction::Mean => {
            let sum = values
                .iter()
                .fold(0.0_f64, |sum, value| sum + *value as f64);
            Ok(Value::Float(sum / values.len() as f64))
        }
    }
}

fn reduce_float32_array(values: &[f32], reduction: ArrayReduction) -> Result<Value> {
    if values.iter().any(|value| value.is_nan()) {
        return Ok(Value::Float(f64::NAN));
    }
    match reduction {
        ArrayReduction::Sum => Ok(Value::Float(
            values.iter().fold(0.0_f32, |sum, value| sum + *value) as f64,
        )),
        ArrayReduction::Min => Ok(Value::Float(
            values
                .iter()
                .copied()
                .reduce(f32::min)
                .expect("empty arrays were rejected") as f64,
        )),
        ArrayReduction::Max => Ok(Value::Float(
            values
                .iter()
                .copied()
                .reduce(f32::max)
                .expect("empty arrays were rejected") as f64,
        )),
        ArrayReduction::Mean => {
            let sum = values
                .iter()
                .fold(0.0_f64, |sum, value| sum + *value as f64);
            Ok(Value::Float(sum / values.len() as f64))
        }
    }
}

fn reduce_float64_array(values: &[f64], reduction: ArrayReduction) -> Result<Value> {
    if values.iter().any(|value| value.is_nan()) {
        return Ok(Value::Float(f64::NAN));
    }
    match reduction {
        ArrayReduction::Sum => Ok(Value::Float(
            values.iter().fold(0.0_f64, |sum, value| sum + *value),
        )),
        ArrayReduction::Min => Ok(Value::Float(
            values
                .iter()
                .copied()
                .reduce(f64::min)
                .expect("empty arrays were rejected"),
        )),
        ArrayReduction::Max => Ok(Value::Float(
            values
                .iter()
                .copied()
                .reduce(f64::max)
                .expect("empty arrays were rejected"),
        )),
        ArrayReduction::Mean => {
            let sum = values.iter().fold(0.0_f64, |sum, value| sum + *value);
            Ok(Value::Float(sum / values.len() as f64))
        }
    }
}

fn normalize_slice_endpoint(
    supplied: Option<i128>,
    default: i128,
    label: &str,
    len: i128,
) -> Result<usize> {
    let supplied = supplied.unwrap_or(default);
    let normalized = if supplied < 0 {
        // `len` is non-negative, so adding it to a negative i128 cannot
        // overflow either end of the signed range.
        len + supplied
    } else {
        supplied
    };
    if !(0..=len).contains(&normalized) {
        let message = if supplied < 0 {
            format!("slice {label} `{supplied}` normalizes to `{normalized}`, outside `0..={len}`")
        } else {
            format!("slice {label} `{supplied}` is outside `0..={len}`")
        };
        return Err(Diagnostic::coded("AU4003", message));
    }
    // Aura's supported hosts have pointer widths smaller than i128, and
    // `normalized <= len` where `len` came from a live usize.
    Ok(normalized as usize)
}

pub(crate) fn normalize_slice_bounds(
    start: Option<i128>,
    end: Option<i128>,
    len: usize,
) -> Result<(usize, usize)> {
    // Every supported Rust pointer width fits losslessly in i128.
    let len = len as i128;
    let start = normalize_slice_endpoint(start, 0, "start", len)?;
    let end = normalize_slice_endpoint(end, len, "end", len)?;
    if start > end {
        return Err(Diagnostic::coded(
            "AU4003",
            format!("slice start `{start}` is greater than slice end `{end}`"),
        ));
    }
    Ok((start, end))
}

pub(crate) fn slice_vec_owned(
    vector: &VecValue,
    start: Option<i128>,
    end: Option<i128>,
) -> Result<VecValue> {
    let (start, end) = normalize_slice_bounds(start, end, vector.elements.len())?;
    let mut elements = try_array_buffer(end - start, "Array-containing Vec slice")?;
    for value in &vector.elements[start..end] {
        elements.push(try_clone_array_containing_value(value)?);
    }
    Ok(VecValue {
        element_type: vector.element_type.clone(),
        elements,
    })
}

pub(crate) fn slice_string_owned(
    text: &str,
    start: Option<i128>,
    end: Option<i128>,
) -> Result<String> {
    let scalar_len = text.chars().count();
    let (start, end) = normalize_slice_bounds(start, end, scalar_len)?;
    Ok(text.chars().skip(start).take(end - start).collect())
}

#[derive(Clone, Debug)]
pub struct TupleValue {
    pub element_types: Vec<Type>,
    pub elements: Vec<Value>,
}

impl PartialEq for TupleValue {
    fn eq(&self, other: &Self) -> bool {
        self.elements == other.elements
    }
}

enum ArrayCloneCleanupFrame {
    Values(std::vec::IntoIter<Value>),
    Map {
        entries: std::vec::IntoIter<(Value, Value)>,
        pending_value: Option<Value>,
    },
    Fields(std::collections::btree_map::IntoValues<String, Value>),
}

impl ArrayCloneCleanupFrame {
    fn next_value(&mut self) -> Option<Value> {
        match self {
            Self::Values(values) => values.next(),
            Self::Map {
                entries,
                pending_value,
            } => {
                if let Some(value) = pending_value.take() {
                    return Some(value);
                }
                let (key, value) = entries.next()?;
                *pending_value = Some(value);
                Some(key)
            }
            Self::Fields(fields) => fields.next(),
        }
    }
}

fn drop_array_containing_value_iteratively(value: Value, frames: &mut Vec<ArrayCloneCleanupFrame>) {
    assert!(
        frames.is_empty(),
        "Array clone cleanup frames must be reusable between owned roots"
    );
    let mut next = Some(value);
    loop {
        if let Some(value) = next.take() {
            let frame = match value {
                Value::Tuple(TupleValue { elements, .. })
                | Value::Vec(VecValue { elements, .. })
                | Value::Set(SetValue { elements, .. }) => {
                    Some(ArrayCloneCleanupFrame::Values(elements.into_iter()))
                }
                Value::Map(MapValue { entries, .. }) => Some(ArrayCloneCleanupFrame::Map {
                    entries: entries.into_iter(),
                    pending_value: None,
                }),
                Value::Instance(InstanceValue { fields, .. }) => {
                    Some(ArrayCloneCleanupFrame::Fields(fields.into_values()))
                }
                Value::EnumVariant(EnumVariantValue { payloads, .. }) => {
                    Some(ArrayCloneCleanupFrame::Values(payloads.into_iter()))
                }
                _ => None,
            };
            if let Some(mut frame) = frame {
                next = frame.next_value();
                if next.is_some() {
                    assert!(
                        frames.len() < frames.capacity(),
                        "Array clone cleanup depth must be reserved before traversal"
                    );
                    frames.push(frame);
                }
            }
        } else {
            let Some(frame) = frames.last_mut() else {
                return;
            };
            next = frame.next_value();
            if next.is_none() {
                frames.pop();
            }
        }
    }
}

pub(crate) fn try_clone_array_containing_value(value: &Value) -> Result<Value> {
    enum CloneFrame<'a> {
        Tuple {
            tuple: &'a TupleValue,
            next_index: usize,
            element_types: Vec<Type>,
            elements: Vec<Value>,
        },
        Vec {
            vector: &'a VecValue,
            next_index: usize,
            elements: Vec<Value>,
        },
        Set {
            set: &'a SetValue,
            next_index: usize,
            elements: Vec<Value>,
        },
        MapKey {
            map: &'a MapValue,
            next_index: usize,
            entries: Vec<(Value, Value)>,
            pending_value: &'a Value,
        },
        MapValue {
            map: &'a MapValue,
            next_index: usize,
            entries: Vec<(Value, Value)>,
            pending_key: Value,
        },
        Instance {
            instance: &'a InstanceValue,
            remaining: std::collections::btree_map::Iter<'a, String, Value>,
            fields: BTreeMap<String, Value>,
            pending_name: String,
        },
        EnumVariant {
            variant: &'a EnumVariantValue,
            next_index: usize,
            payloads: Vec<Value>,
        },
    }

    fn drop_frame_values(frame: CloneFrame<'_>, cleanup_frames: &mut Vec<ArrayCloneCleanupFrame>) {
        match frame {
            CloneFrame::Tuple { elements, .. }
            | CloneFrame::Vec { elements, .. }
            | CloneFrame::Set { elements, .. }
            | CloneFrame::EnumVariant {
                payloads: elements, ..
            } => {
                for value in elements {
                    drop_array_containing_value_iteratively(value, cleanup_frames);
                }
            }
            CloneFrame::MapKey { entries, .. } => {
                for (key, value) in entries {
                    drop_array_containing_value_iteratively(key, cleanup_frames);
                    drop_array_containing_value_iteratively(value, cleanup_frames);
                }
            }
            CloneFrame::MapValue {
                entries,
                pending_key,
                ..
            } => {
                drop_array_containing_value_iteratively(pending_key, cleanup_frames);
                for (key, value) in entries {
                    drop_array_containing_value_iteratively(key, cleanup_frames);
                    drop_array_containing_value_iteratively(value, cleanup_frames);
                }
            }
            CloneFrame::Instance { fields, .. } => {
                for value in fields.into_values() {
                    drop_array_containing_value_iteratively(value, cleanup_frames);
                }
            }
        }
    }

    struct CloneState<'a> {
        frames: Vec<CloneFrame<'a>>,
        cleanup_frames: Vec<ArrayCloneCleanupFrame>,
        pending_frame: Option<CloneFrame<'a>>,
        completed: Option<Value>,
    }

    impl<'a> CloneState<'a> {
        fn push_frame(&mut self, frame: CloneFrame<'a>) -> Result<()> {
            debug_assert!(self.pending_frame.is_none());
            self.pending_frame = Some(frame);
            let required_depth = self.frames.len().saturating_add(1);
            if self.cleanup_frames.capacity() < required_depth {
                self.cleanup_frames
                    .try_reserve(required_depth - self.cleanup_frames.len())
                    .map_err(|_| {
                        array_allocation_error(
                            "Array-containing value clone cleanup traversal",
                            required_depth,
                        )
                    })?;
            }
            if self.frames.capacity() < required_depth {
                self.frames.try_reserve(1).map_err(|_| {
                    array_allocation_error("Array-containing value clone traversal", required_depth)
                })?;
            }
            self.frames.push(
                self.pending_frame
                    .take()
                    .expect("a reserved clone frame remains pending"),
            );
            Ok(())
        }
    }

    impl Drop for CloneState<'_> {
        fn drop(&mut self) {
            if let Some(value) = self.completed.take() {
                drop_array_containing_value_iteratively(value, &mut self.cleanup_frames);
            }
            if let Some(frame) = self.pending_frame.take() {
                drop_frame_values(frame, &mut self.cleanup_frames);
            }
            while let Some(frame) = self.frames.pop() {
                drop_frame_values(frame, &mut self.cleanup_frames);
            }
            debug_assert!(self.cleanup_frames.is_empty());
        }
    }

    let mut state = CloneState {
        frames: Vec::new(),
        cleanup_frames: Vec::new(),
        pending_frame: None,
        completed: None,
    };
    let mut next = Some(value);

    loop {
        if let Some(value) = next.take() {
            state.completed = match value {
                Value::Array(array) => Some(Value::Array(array.try_clone()?)),
                Value::Tuple(tuple) => {
                    let mut element_types = try_array_buffer(
                        tuple.element_types.len(),
                        "Array-containing tuple type copy",
                    )?;
                    element_types.extend(tuple.element_types.iter().cloned());
                    let elements =
                        try_array_buffer(tuple.elements.len(), "Array-containing tuple copy")?;
                    if tuple.elements.is_empty() {
                        Some(Value::Tuple(TupleValue {
                            element_types,
                            elements,
                        }))
                    } else {
                        state.push_frame(CloneFrame::Tuple {
                            tuple,
                            next_index: 1,
                            element_types,
                            elements,
                        })?;
                        next = Some(&tuple.elements[0]);
                        None
                    }
                }
                Value::Vec(vector) => {
                    let elements =
                        try_array_buffer(vector.elements.len(), "Array-containing Vec copy")?;
                    if vector.elements.is_empty() {
                        Some(Value::Vec(VecValue {
                            element_type: vector.element_type.clone(),
                            elements,
                        }))
                    } else {
                        state.push_frame(CloneFrame::Vec {
                            vector,
                            next_index: 1,
                            elements,
                        })?;
                        next = Some(&vector.elements[0]);
                        None
                    }
                }
                Value::Set(set) => {
                    let elements =
                        try_array_buffer(set.elements.len(), "Array-containing Set copy")?;
                    if set.elements.is_empty() {
                        Some(Value::Set(SetValue {
                            element_type: set.element_type.clone(),
                            elements,
                        }))
                    } else {
                        state.push_frame(CloneFrame::Set {
                            set,
                            next_index: 1,
                            elements,
                        })?;
                        next = Some(&set.elements[0]);
                        None
                    }
                }
                Value::Map(map) => {
                    let entries = try_array_buffer(map.entries.len(), "Array-containing Map copy")?;
                    if let Some((key, pending_value)) = map.entries.first() {
                        state.push_frame(CloneFrame::MapKey {
                            map,
                            next_index: 1,
                            entries,
                            pending_value,
                        })?;
                        next = Some(key);
                        None
                    } else {
                        Some(Value::Map(MapValue {
                            key_type: map.key_type.clone(),
                            value_type: map.value_type.clone(),
                            entries,
                        }))
                    }
                }
                Value::Instance(instance) => {
                    let mut remaining = instance.fields.iter();
                    if let Some((name, field_value)) = remaining.next() {
                        state.push_frame(CloneFrame::Instance {
                            instance,
                            remaining,
                            fields: BTreeMap::new(),
                            pending_name: name.clone(),
                        })?;
                        next = Some(field_value);
                        None
                    } else {
                        Some(Value::Instance(InstanceValue {
                            class_name: instance.class_name.clone(),
                            fields: BTreeMap::new(),
                        }))
                    }
                }
                Value::EnumVariant(variant) => {
                    let payloads = try_array_buffer(
                        variant.payloads.len(),
                        "Array-containing enum payload copy",
                    )?;
                    if variant.payloads.is_empty() {
                        Some(Value::EnumVariant(EnumVariantValue {
                            enum_name: variant.enum_name.clone(),
                            variant_name: variant.variant_name.clone(),
                            payloads,
                        }))
                    } else {
                        state.push_frame(CloneFrame::EnumVariant {
                            variant,
                            next_index: 1,
                            payloads,
                        })?;
                        next = Some(&variant.payloads[0]);
                        None
                    }
                }
                value => Some(value.clone()),
            };
        } else {
            let frame = state
                .frames
                .pop()
                .expect("an incomplete Array-containing value clone has a parent frame");
            let cloned = state
                .completed
                .take()
                .expect("an Array-containing value clone frame receives one completed child");
            match frame {
                CloneFrame::Tuple {
                    tuple,
                    mut next_index,
                    element_types,
                    mut elements,
                } => {
                    elements.push(cloned);
                    if let Some(element) = tuple.elements.get(next_index) {
                        next_index += 1;
                        state.push_frame(CloneFrame::Tuple {
                            tuple,
                            next_index,
                            element_types,
                            elements,
                        })?;
                        next = Some(element);
                    } else {
                        state.completed = Some(Value::Tuple(TupleValue {
                            element_types,
                            elements,
                        }));
                    }
                }
                CloneFrame::Vec {
                    vector,
                    mut next_index,
                    mut elements,
                } => {
                    elements.push(cloned);
                    if let Some(element) = vector.elements.get(next_index) {
                        next_index += 1;
                        state.push_frame(CloneFrame::Vec {
                            vector,
                            next_index,
                            elements,
                        })?;
                        next = Some(element);
                    } else {
                        state.completed = Some(Value::Vec(VecValue {
                            element_type: vector.element_type.clone(),
                            elements,
                        }));
                    }
                }
                CloneFrame::Set {
                    set,
                    mut next_index,
                    mut elements,
                } => {
                    elements.push(cloned);
                    if let Some(element) = set.elements.get(next_index) {
                        next_index += 1;
                        state.push_frame(CloneFrame::Set {
                            set,
                            next_index,
                            elements,
                        })?;
                        next = Some(element);
                    } else {
                        state.completed = Some(Value::Set(SetValue {
                            element_type: set.element_type.clone(),
                            elements,
                        }));
                    }
                }
                CloneFrame::MapKey {
                    map,
                    next_index,
                    entries,
                    pending_value,
                } => {
                    state.push_frame(CloneFrame::MapValue {
                        map,
                        next_index,
                        entries,
                        pending_key: cloned,
                    })?;
                    next = Some(pending_value);
                }
                CloneFrame::MapValue {
                    map,
                    mut next_index,
                    mut entries,
                    pending_key,
                } => {
                    entries.push((pending_key, cloned));
                    if let Some((key, pending_value)) = map.entries.get(next_index) {
                        next_index += 1;
                        state.push_frame(CloneFrame::MapKey {
                            map,
                            next_index,
                            entries,
                            pending_value,
                        })?;
                        next = Some(key);
                    } else {
                        state.completed = Some(Value::Map(MapValue {
                            key_type: map.key_type.clone(),
                            value_type: map.value_type.clone(),
                            entries,
                        }));
                    }
                }
                CloneFrame::Instance {
                    instance,
                    mut remaining,
                    mut fields,
                    pending_name,
                } => {
                    fields.insert(pending_name, cloned);
                    if let Some((name, field_value)) = remaining.next() {
                        state.push_frame(CloneFrame::Instance {
                            instance,
                            remaining,
                            fields,
                            pending_name: name.clone(),
                        })?;
                        next = Some(field_value);
                    } else {
                        state.completed = Some(Value::Instance(InstanceValue {
                            class_name: instance.class_name.clone(),
                            fields,
                        }));
                    }
                }
                CloneFrame::EnumVariant {
                    variant,
                    mut next_index,
                    mut payloads,
                } => {
                    payloads.push(cloned);
                    if let Some(payload) = variant.payloads.get(next_index) {
                        next_index += 1;
                        state.push_frame(CloneFrame::EnumVariant {
                            variant,
                            next_index,
                            payloads,
                        })?;
                        next = Some(payload);
                    } else {
                        state.completed = Some(Value::EnumVariant(EnumVariantValue {
                            enum_name: variant.enum_name.clone(),
                            variant_name: variant.variant_name.clone(),
                            payloads,
                        }));
                    }
                }
            }
        }

        if next.is_none() && state.frames.is_empty() {
            return Ok(state
                .completed
                .take()
                .expect("an Array-containing value clone completes before its frame stack"));
        }
    }
}

#[derive(Clone, Debug)]
pub struct SetValue {
    pub element_type: Type,
    pub elements: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct MapValue {
    pub key_type: Type,
    pub value_type: Type,
    pub entries: Vec<(Value, Value)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeValue {
    pub start: i128,
    pub end: i128,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModuleNamespaceValue {
    pub path: String,
}

#[derive(Clone)]
pub struct RngValue {
    inner: Arc<Mutex<DeterministicRng>>,
}

impl RngValue {
    pub(crate) fn from_seed(seed: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DeterministicRng::from_seed(seed))),
        }
    }

    pub(crate) fn next_int(
        &self,
        lo: i64,
        hi: i64,
    ) -> std::result::Result<i64, InvalidRandomRange> {
        lock_mutex(&self.inner).next_int(lo, hi)
    }

    pub(crate) fn next_float(&self) -> f64 {
        lock_mutex(&self.inner).next_float()
    }

    pub(crate) fn shuffle(&self, values: &mut [Value]) {
        lock_mutex(&self.inner).shuffle(values);
    }
}

#[derive(Clone)]
pub struct ChannelValue {
    inner: Arc<ChannelState>,
}

struct ChannelState {
    state: Mutex<ChannelInner>,
    producer_tasks: Mutex<WeakTaskRegistry>,
    task_handles: Mutex<WeakTaskRegistry>,
    sender_tasks: Mutex<WeakTaskRegistry>,
    receiver_tasks: Mutex<WeakTaskRegistry>,
    recv_reactor_subscribers: Mutex<HashMap<ReactorSubscriptionKey, ReactorRecvSubscription>>,
    send_reactor_subscribers: Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
    runtime_type_name: Mutex<Option<String>>,
}

struct ChannelInner {
    queue: VecDeque<Value>,
    closed: bool,
    capacity: Option<usize>,
}

struct WeakTaskRegistry {
    tasks: HashMap<usize, Weak<TaskState>>,
    next_prune_at: usize,
}

impl WeakTaskRegistry {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_prune_at: 256,
        }
    }
}

#[derive(Clone)]
pub struct TaskValue {
    inner: Arc<TaskState>,
}

#[derive(Clone)]
pub struct TaskGroupValue {
    inner: Arc<TaskGroupState>,
}

#[derive(Clone)]
pub struct FileValue {
    inner: Arc<FileState>,
}

#[derive(Clone)]
pub struct TcpListenerValue {
    inner: Arc<TcpListenerState>,
}

#[derive(Clone)]
pub struct TcpStreamValue {
    inner: Arc<TcpStreamState>,
}

#[derive(Clone)]
pub struct UdpSocketValue {
    inner: Arc<UdpSocketState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UdpDatagramValue {
    pub address: String,
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub struct HttpListenerValue {
    inner: Arc<HttpListenerState>,
}

#[derive(Clone)]
pub struct HttpExchangeValue {
    inner: Arc<HttpExchangeState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponseValue {
    status: i32,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
pub struct WebSocketListenerValue {
    inner: Arc<WebSocketListenerState>,
}

#[derive(Clone)]
pub struct WebSocketValue {
    inner: Arc<WebSocketState>,
}

#[derive(Clone)]
pub struct ProcessChildValue {
    inner: Arc<ProcessChildState>,
}

#[derive(Clone)]
pub struct ProcessPipeValue {
    inner: Arc<ProcessPipeState>,
}

#[derive(Clone)]
pub struct ProcessCompletedValue {
    inner: Arc<ProcessCompletedState>,
}

#[derive(Clone)]
pub struct ProcessSupervisorValue {
    inner: Arc<ProcessSupervisorState>,
}

#[derive(Clone)]
pub struct UnixListenerValue {
    inner: Arc<UnixListenerState>,
}

#[derive(Clone)]
pub struct UnixStreamValue {
    inner: Arc<UnixStreamState>,
}

#[derive(Clone)]
pub struct TlsListenerValue {
    inner: Arc<TlsListenerState>,
}

#[derive(Clone)]
pub struct TlsStreamValue {
    inner: Arc<TlsStreamState>,
}

struct TaskState {
    handle: Mutex<TaskHandle>,
    ready: Condvar,
    lightweight: bool,
    result_is_repeatable: bool,
    result_observation_claimed: AtomicBool,
    waits_without_deadline: AtomicBool,
    current_wait: Mutex<Option<TaskWaitReachability>>,
    join_dependencies: Mutex<Option<Vec<Weak<TaskState>>>>,
    observed_failure: AtomicBool,
    completion_reactor_subscribers: Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
    group_failure_wake_flags: Mutex<Vec<Arc<RuntimeWakeSignal>>>,
    group_completion_wake_flags: Mutex<Vec<Arc<RuntimeWakeSignal>>>,
    runtime_type_name: Mutex<Option<String>>,
}

struct TaskGroupState {
    tasks: Mutex<Vec<TaskValue>>,
    cancel_flag: Arc<RuntimeWakeSignal>,
    failure_wake_flag: Arc<RuntimeWakeSignal>,
    completion_wake_flag: Arc<RuntimeWakeSignal>,
    parent_flags: Vec<Arc<RuntimeWakeSignal>>,
}

struct FileState {
    file: Mutex<Option<StdFile>>,
}

struct TcpListenerState {
    listener: Mutex<Option<StdTcpListener>>,
}

struct TcpStreamState {
    stream: Mutex<Option<StdTcpStream>>,
}

struct UdpSocketState {
    socket: Mutex<Option<StdUdpSocket>>,
}

struct HttpListenerState {
    listener: Mutex<Option<StdTcpListener>>,
}

struct HttpExchangeState {
    stream: Mutex<Option<TcpStreamValue>>,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct WebSocketListenerState {
    listener: Mutex<Option<StdTcpListener>>,
}

enum WebSocketStateKind {
    Plain(Box<WebSocket<StdTcpStream>>),
    MaybeTls(Box<WebSocket<MaybeTlsStream<StdTcpStream>>>),
}

struct WebSocketState {
    socket: Mutex<Option<WebSocketStateKind>>,
    busy: AtomicBool,
    closed: AtomicBool,
}

struct ProcessChildState {
    child: Mutex<Option<StdChild>>,
    waited: Mutex<Option<StdExitStatus>>,
    process_group_id: Option<i32>,
    stdin: Option<ProcessPipeValue>,
    stdout: Option<ProcessPipeValue>,
    stderr: Option<ProcessPipeValue>,
}

enum ProcessPipeKind {
    Stdin(StdChildStdin),
    Stdout(StdChildStdout),
    Stderr(StdChildStderr),
}

struct ProcessPipeState {
    pipe: Mutex<Option<ProcessPipeKind>>,
}

struct ProcessCompletedState {
    status: Value,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct ProcessSupervisorState {
    services: Mutex<BTreeMap<String, ProcessSupervisorEntry>>,
}

#[derive(Clone)]
struct ProcessSupervisorSpec {
    command: Vec<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    stdin: ProcessStdioConfig,
    stdout: ProcessStdioConfig,
    stderr: ProcessStdioConfig,
    restart: ProcessRestartPolicy,
    backoff: StdDuration,
    max_restarts: Option<i32>,
    group: bool,
}

struct ProcessSupervisorEntry {
    spec: ProcessSupervisorSpec,
    child: Option<ProcessChildValue>,
    restart_count: i32,
    pending_restart_status: Option<StdExitStatus>,
    next_restart_at: Option<Instant>,
}

pub(crate) enum ProcessChildWaitStatus {
    Exited(StdExitStatus),
    TimedOut,
    Cancelled,
    Failed(io::Error),
}

pub(crate) enum ProcessSupervisorWaitStatus {
    Event(Value),
    TimedOut,
    Cancelled,
}

enum SupervisorRestartSchedule {
    Deadline(Instant),
    Failed(Value),
}

#[derive(Clone, Copy)]
pub(crate) enum ProcessStdioConfig {
    Inherit,
    Null,
    Pipe,
}

impl ProcessStdioConfig {
    fn as_stdio(self) -> StdProcessStdio {
        match self {
            Self::Inherit => StdProcessStdio::inherit(),
            Self::Null => StdProcessStdio::null(),
            Self::Pipe => StdProcessStdio::piped(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessRestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[cfg(unix)]
struct UnixListenerState {
    listener: Mutex<Option<StdUnixListener>>,
}

#[cfg(not(unix))]
struct UnixListenerState;

#[cfg(unix)]
struct UnixStreamState {
    stream: Mutex<Option<StdUnixStream>>,
}

#[cfg(not(unix))]
struct UnixStreamState;

struct TlsListenerState {
    listener: Mutex<Option<StdTcpListener>>,
    config: Arc<ServerConfig>,
}

struct PendingTlsServerHandshake {
    stream: rustls::StreamOwned<ServerConnection, StdTcpStream>,
    deadline: Option<Instant>,
}

enum TlsStreamKind {
    Client(rustls::StreamOwned<ClientConnection, StdTcpStream>),
    Server(rustls::StreamOwned<ServerConnection, StdTcpStream>),
}

type TlsClientStream = rustls::StreamOwned<ClientConnection, StdTcpStream>;

struct TlsStreamState {
    stream: Mutex<Option<TlsStreamKind>>,
    busy: AtomicBool,
    closed: AtomicBool,
}

struct ProtocolOperationGuard<'a> {
    busy: &'a AtomicBool,
}

impl Drop for ProtocolOperationGuard<'_> {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TaskExecutionResult {
    Ready(std::result::Result<Value, Diagnostic>),
    Cancelled,
}

pub(crate) struct LightweightTaskFailureSignal(pub(crate) Diagnostic);

enum TaskHandle {
    Running,
    Completed(TaskExecutionResult),
}

#[derive(Clone, Default)]
pub(crate) struct CancellationContext {
    flags: Vec<Arc<RuntimeWakeSignal>>,
}

struct ReactorRecvSubscription {
    target: ReactorSubscription,
    ignore_closed: bool,
}

#[derive(Default)]
struct RuntimeWakeSignal {
    value: AtomicBool,
    reactor_subscribers: Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
}

impl RuntimeWakeSignal {
    fn new(value: bool) -> Self {
        Self {
            value: AtomicBool::new(value),
            reactor_subscribers: Mutex::new(HashMap::new()),
        }
    }

    fn load(&self, ordering: Ordering) -> bool {
        self.value.load(ordering)
    }

    fn store(&self, value: bool, ordering: Ordering) {
        let previous = self.value.swap(value, ordering);
        if value && !previous {
            wake_reactor_subscribers(&self.reactor_subscribers);
        }
    }

    fn subscribe(&self, subscription: &ReactorSubscription) {
        subscribe_reactor_target(&self.reactor_subscribers, subscription);
    }

    fn unsubscribe(&self, subscription: &ReactorSubscription) {
        unsubscribe_reactor_target(&self.reactor_subscribers, subscription);
    }
}

fn subscribe_reactor_target(
    subscribers: &Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
    subscription: &ReactorSubscription,
) {
    lock_mutex(subscribers)
        .entry(subscription.identity())
        .or_insert_with(|| subscription.clone());
}

fn unsubscribe_reactor_target(
    subscribers: &Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
    subscription: &ReactorSubscription,
) {
    lock_mutex(subscribers).remove(&subscription.identity());
}

fn wake_reactor_subscribers(
    subscribers: &Mutex<HashMap<ReactorSubscriptionKey, ReactorSubscription>>,
) {
    let targets = std::mem::take(&mut *lock_mutex(subscribers));
    let mut targets: Vec<_> = targets.into_values().collect();
    targets.sort_unstable_by_key(ReactorSubscription::identity);
    for target in targets {
        let _ = target.wake();
    }
}

enum TaskYield {
    Wait(TaskWaitRegistration),
    YieldNow,
    Exit,
}

#[derive(Clone)]
struct TaskWaitRegistration {
    recv_channels: Vec<ChannelValue>,
    ignore_closed_recv_channels: bool,
    send_channels: Vec<ChannelValue>,
    task_waits: Vec<TaskValue>,
    deadline: Option<Instant>,
    cancellation: Option<CancellationContext>,
    fd_wait: Option<FdWaitRegistration>,
}

#[derive(Clone)]
struct TaskWaitReachability {
    recv_channels: Vec<Weak<ChannelState>>,
    ignore_closed_recv_channels: bool,
    send_channels: Vec<Weak<ChannelState>>,
    task_waits: Vec<Weak<TaskState>>,
    deadline: Option<Instant>,
    cancellation: Option<CancellationContext>,
}

#[derive(Clone, Copy)]
struct FdWaitRegistration {
    fd: libc::c_int,
    events: libc::c_short,
}

struct LightweightTaskContext {
    spawn_target: LightweightTaskSpawnTarget,
    task_id: u64,
    #[cfg_attr(not(test), allow(dead_code))]
    worker_index: usize,
    yielder: Cell<*const Yielder<RuntimeSchedulerWakeReason, TaskYield>>,
    cancellation: Option<CancellationContext>,
    task_state: Weak<TaskState>,
}

struct LightweightTaskRecord {
    state: Arc<TaskState>,
    context: Rc<LightweightTaskContext>,
    coroutine: Coroutine<RuntimeSchedulerWakeReason, TaskYield, TaskExecutionResult>,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
}

type LightweightTaskEntry =
    Box<dyn FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static>;

struct PreparedLightweightTask {
    state: Arc<TaskState>,
    stack: DefaultStack,
    cancellation: Option<CancellationContext>,
    entry: LightweightTaskEntry,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Clone)]
enum LightweightTaskSpawnTarget {
    Local(Rc<RefCell<VecDeque<PreparedLightweightTask>>>),
    Workers(Arc<LightweightWorkerCoordinator>),
}

struct LightweightTaskWait {
    key: WaitKey,
    registration: TaskWaitRegistration,
    subscription: ReactorSubscription,
}

struct LightweightTaskScheduler {
    next_task_id: u64,
    next_wait_epoch: u64,
    spawn_requests: Rc<RefCell<VecDeque<PreparedLightweightTask>>>,
    ready: VecDeque<(u64, RuntimeSchedulerWakeReason)>,
    waiting: BTreeMap<u64, LightweightTaskWait>,
    tasks: BTreeMap<u64, LightweightTaskRecord>,
    reactor: RuntimeReactor,
    reactor_failure: Option<Diagnostic>,
    ready_turns_since_local_reactor_poll: u32,
    ready_turns_since_io_reactor_poll: u32,
    worker_index: usize,
    workers: Option<Arc<LightweightWorkerCoordinator>>,
}

struct LightweightWorkerInbox {
    requests: Mutex<VecDeque<PreparedLightweightTask>>,
    reactor: Mutex<Option<ReactorHandle>>,
    cleanup_complete: AtomicBool,
}

struct LightweightWorkerCoordinator {
    inboxes: Vec<LightweightWorkerInbox>,
    next_worker: AtomicUsize,
    next_task_id: AtomicU64,
    pending_requests: AtomicUsize,
    shutdown: AtomicBool,
    cleanup_workers: AtomicUsize,
    fatal: Mutex<Option<Diagnostic>>,
}

// Deep HTTP, TLS, and WebSocket library frames run on the bounded protocol
// service, keeping ordinary guarded lightweight-task stacks compact.
// The MIR interpreter can nest several large checked-dispatch frames before a
// task reaches a blocking host call. Keep enough headroom for ordinary builtin
// function values; the run-pass fixture suite pins this path.
const LIGHTWEIGHT_TASK_STACK_SIZE: usize = 768 * 1024;
#[cfg(test)]
thread_local! {
    static FAIL_NEXT_LIGHTWEIGHT_TASK_STACK_ALLOCATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_lightweight_task_stack_allocation() {
    FAIL_NEXT_LIGHTWEIGHT_TASK_STACK_ALLOCATION.with(|fail| fail.set(true));
}

pub(crate) fn allocate_lightweight_task_stack(
    stack_size: usize,
) -> std::result::Result<DefaultStack, Diagnostic> {
    #[cfg(test)]
    if FAIL_NEXT_LIGHTWEIGHT_TASK_STACK_ALLOCATION.with(|fail| fail.replace(false)) {
        return Err(Diagnostic::coded(
            "AU4005",
            "injected Aura task stack allocation failure",
        ));
    }
    DefaultStack::new(stack_size).map_err(|error| {
        Diagnostic::coded(
            "AU4005",
            format!("failed to allocate Aura task stack: {error}"),
        )
    })
}

pub(crate) const MAX_FILESYSTEM_READ_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_STREAM_READ_BYTES: usize = 64 * 1024 * 1024;
const MAX_TLS_CONFIG_BYTES: usize = MAX_STREAM_READ_BYTES;
const MAX_UDP_DATAGRAM_BYTES: usize = 65_535;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 << 20;
const MAX_WEBSOCKET_FRAME_BYTES: usize = 16 << 20;
const MAX_WEBSOCKET_WRITE_BUFFER_BYTES: usize = 16 << 20;
const DEFAULT_TLS_HANDSHAKE_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const MIN_SUPERVISOR_RESTART_BACKOFF: StdDuration = StdDuration::from_millis(10);
const TASK_GROUP_CLEANUP_PROBE_TIMEOUT: StdDuration = StdDuration::from_millis(1);
const READY_TURNS_PER_LOCAL_REACTOR_POLL: u32 = 64;
const READY_TURNS_PER_IO_REACTOR_POLL: u32 = 256;

#[derive(Clone)]
struct RuntimeSchedulerRegistration {
    recv_channels: Vec<ChannelValue>,
    ignore_closed_recv_channels: bool,
    send_channels: Vec<ChannelValue>,
    task_waits: Vec<TaskValue>,
    deadline: Option<Instant>,
    cancellation: Option<CancellationContext>,
    waiter: Arc<RuntimeSchedulerWaiter>,
}

struct RuntimeSchedulerWaiter {
    state: Mutex<Option<RuntimeSchedulerWakeReason>>,
    ready: Condvar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeSchedulerWakeReason {
    Ready,
    TimedOut,
    Cancelled,
}

struct RuntimeSchedulerState {
    registrations: BTreeMap<u64, RuntimeSchedulerRegistration>,
}

struct RuntimeScheduler {
    state: Mutex<RuntimeSchedulerState>,
    ready: Condvar,
    next_id: AtomicU64,
}

static RUNTIME_SCHEDULER: OnceLock<Arc<RuntimeScheduler>> = OnceLock::new();

type BlockingIoJob = Box<dyn FnOnce() + Send + 'static>;
#[cfg(test)]
type BlockingIoWorkerEntry = Box<dyn FnOnce() + Send + 'static>;
type ProtocolStepJob = Box<dyn FnOnce() + Send + 'static>;

struct JsonCodecJob {
    operation: Box<dyn FnOnce() -> std::result::Result<JsonValue, JsonCodecError> + Send + 'static>,
    result: Arc<Mutex<Option<std::result::Result<JsonValue, JsonCodecServiceError>>>>,
    completion: ChannelValue,
}

struct BlockingIoAdmissionWaiter {
    id: u64,
    job: Option<BlockingIoJob>,
    deadline: Option<Instant>,
    cancellation: Option<CancellationContext>,
    outcome: Arc<Mutex<Option<BlockingIoAdmissionOutcome>>>,
    completion: ChannelValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingIoAdmissionOutcome {
    Accepted,
    TimedOut,
    Cancelled,
}

struct BlockingIoPoolState {
    queue: VecDeque<BlockingIoJob>,
    admission_waiters: VecDeque<BlockingIoAdmissionWaiter>,
    next_waiter_id: u64,
    active_jobs: usize,
    shutting_down: bool,
}

enum BlockingIoPoolInitialization {
    NotStarted,
    #[allow(dead_code)] // Production workers are process-lifetime; tests join these handles.
    Started(Vec<thread::JoinHandle<()>>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingIoWorkerStartupState {
    Pending,
    Run,
    Stop,
}

struct BlockingIoWorkerStartupGate {
    state: Mutex<BlockingIoWorkerStartupState>,
    ready: Condvar,
}

struct BlockingIoPool {
    config: BlockingIoPoolConfig,
    state: Mutex<BlockingIoPoolState>,
    ready: Condvar,
    idle: Condvar,
    initialization: Mutex<BlockingIoPoolInitialization>,
}

#[cfg(test)]
thread_local! {
    static AFTER_BLOCKING_IO_ADMISSION_REGISTER_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
fn install_after_blocking_io_admission_register_hook(hook: impl FnOnce() + 'static) {
    AFTER_BLOCKING_IO_ADMISSION_REGISTER_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "blocking-I/O admission hook already installed"
        );
    });
}

struct ProtocolStepPool {
    queue: Mutex<VecDeque<ProtocolStepJob>>,
    ready: Condvar,
}

struct JsonCodecPoolState {
    queue: VecDeque<JsonCodecJob>,
    in_flight: usize,
}

struct JsonCodecPool {
    state: Mutex<JsonCodecPoolState>,
    ready: Condvar,
    available: Condvar,
    availability_signal: ChannelValue,
    capacity: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunOutput {
    pub value: Value,
    pub stdout: String,
}

pub(crate) fn cast_numeric_value(value: Value, target: &Type, span: Option<Span>) -> Result<Value> {
    fn render_target_error(span: Option<Span>, message: String) -> Diagnostic {
        match span {
            Some(span) => Diagnostic::at(span, message),
            None => Diagnostic::new(message),
        }
    }

    fn render_source_type(value: &Value) -> String {
        match value {
            Value::Int(_) | Value::Float(_) => {
                unreachable!("numeric source types are handled before render_source_type")
            }
            Value::Bool(_) => "bool".to_string(),
            Value::String(_) => "str".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
            Value::Vec(_) => "list".to_string(),
            Value::Array(_) => "Array".to_string(),
            Value::Set(_) => "set".to_string(),
            Value::Map(_) => "dict".to_string(),
            Value::Duration(_) => "Duration".to_string(),
            Value::Rng(_) => "random.Rng".to_string(),
            Value::Range(_) => "Range".to_string(),
            Value::ModuleNamespace(namespace) => format!("module {}", namespace.path),
            Value::Function(function) => function.signature.to_string(),
            Value::Unit => "None".to_string(),
            Value::FfiHandle(handle) => handle.type_name().to_string(),
            Value::Instance(instance) => {
                nominal_runtime_base_name(&instance.class_name).to_string()
            }
            Value::EnumVariant(variant) => {
                nominal_runtime_base_name(&variant.enum_name).to_string()
            }
            Value::Channel(_) => "Queue".to_string(),
            Value::Task(_) => "Task".to_string(),
            Value::TaskGroup(_) => "TaskGroup".to_string(),
            Value::File(_) => "fs.File".to_string(),
            Value::TcpListener(_) => "net.TcpListener".to_string(),
            Value::TcpStream(_) => "net.TcpStream".to_string(),
            Value::UdpSocket(_) => "net.UdpSocket".to_string(),
            Value::UdpDatagram(_) => "net.UdpDatagram".to_string(),
            Value::HttpListener(_) => "net.HttpListener".to_string(),
            Value::HttpExchange(_) => "net.HttpExchange".to_string(),
            Value::HttpResponse(_) => "net.HttpResponse".to_string(),
            Value::WebSocketListener(_) => "net.WebSocketListener".to_string(),
            Value::WebSocket(_) => "net.WebSocket".to_string(),
            Value::ProcessChild(_) => "process.Child".to_string(),
            Value::ProcessPipe(_) => "process.Pipe".to_string(),
            Value::ProcessCompleted(_) => "process.Completed".to_string(),
            Value::ProcessSupervisor(_) => "process.Supervisor".to_string(),
            Value::UnixListener(_) => "net.UnixListener".to_string(),
            Value::UnixStream(_) => "net.UnixStream".to_string(),
            Value::TlsListener(_) => "net.TlsListener".to_string(),
            Value::TlsStream(_) => "net.TlsStream".to_string(),
        }
    }

    match value {
        Value::Int(value) => {
            if let Some(bounds) = crate::sema::integer_type_bounds(target) {
                if !value.fits_bounds(bounds) {
                    return Err(render_target_error(
                        span,
                        format!("integer value `{}` does not fit in `{}`", value, target),
                    ));
                }
                let value = IntegerKind::from_runtime_type_name(&target.to_string())
                    .and_then(|kind| value.with_runtime_kind(kind))
                    .expect("validated integer values should fit their target runtime kind");
                return Ok(Value::Int(value));
            }
            match target {
                Type::Named(name, args) if args.is_empty() && name == "float32" => {
                    let float = value.to_exact_f32().ok_or_else(|| {
                        render_target_error(
                            span,
                            format!(
                                "integer value `{}` cannot be represented exactly as `float32`",
                                value
                            ),
                        )
                    })?;
                    Ok(Value::Float(float as f64))
                }
                Type::Named(name, args) if args.is_empty() && name == "float64" => {
                    let float = value.to_exact_f64().ok_or_else(|| {
                        render_target_error(
                            span,
                            format!(
                                "integer value `{}` cannot be represented exactly as `float64`",
                                value
                            ),
                        )
                    })?;
                    Ok(Value::Float(float))
                }
                _ => Err(render_target_error(
                    span,
                    format!(
                        "casts are only supported between numeric types, found `integer` and `{}`",
                        target
                    ),
                )),
            }
        }
        Value::Float(value) => {
            if let Some(bounds) = crate::sema::integer_type_bounds(target) {
                if !value.is_finite() {
                    return Err(render_target_error(
                        span,
                        format!("cannot cast non-finite float to `{}`", target),
                    ));
                }
                let truncated = value.trunc();
                let coerced = match bounds {
                    IntegerBounds::Signed { min, max } => {
                        if truncated < min as f64 || truncated > max as f64 {
                            return Err(render_target_error(
                                span,
                                format!(
                                    "integer value `{}` does not fit in `{}`",
                                    truncated, target
                                ),
                            ));
                        }
                        IntegerValue::from_signed(truncated as i128)
                    }
                    IntegerBounds::Unsigned { max } => {
                        if truncated < 0.0 || truncated > max as f64 {
                            return Err(render_target_error(
                                span,
                                format!(
                                    "integer value `{}` does not fit in `{}`",
                                    truncated, target
                                ),
                            ));
                        }
                        IntegerValue::from_literal(truncated as u128)
                    }
                };
                if !coerced.fits_bounds(bounds) {
                    return Err(render_target_error(
                        span,
                        format!("integer value `{}` does not fit in `{}`", coerced, target),
                    ));
                }
                let coerced = IntegerKind::from_runtime_type_name(&target.to_string())
                    .and_then(|kind| coerced.with_runtime_kind(kind))
                    .expect("validated integer values should fit their target runtime kind");
                return Ok(Value::Int(coerced));
            }
            match target {
                Type::Named(name, args) if args.is_empty() && name == "float32" => {
                    Ok(Value::Float((value as f32) as f64))
                }
                Type::Named(name, args) if args.is_empty() && name == "float64" => {
                    Ok(Value::Float(value))
                }
                _ => Err(render_target_error(
                    span,
                    format!(
                        "casts are only supported between numeric types, found `float64` and `{}`",
                        target
                    ),
                )),
            }
        }
        other => Err(render_target_error(
            span,
            format!(
                "casts are only supported between numeric types, found `{}` and `{}`",
                render_source_type(&other),
                target
            ),
        )),
    }
}

impl fmt::Debug for ChannelValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChannelValue(..)")
    }
}

impl fmt::Debug for RngValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RngValue(..)")
    }
}

impl fmt::Debug for TaskValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TaskValue(..)")
    }
}

impl fmt::Debug for TaskGroupValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TaskGroupValue(..)")
    }
}

impl fmt::Debug for FileValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileValue(..)")
    }
}

impl fmt::Debug for TcpListenerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TcpListenerValue(..)")
    }
}

impl fmt::Debug for TcpStreamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TcpStreamValue(..)")
    }
}

impl fmt::Debug for UdpSocketValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UdpSocketValue(..)")
    }
}

impl fmt::Debug for HttpListenerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpListenerValue(..)")
    }
}

impl fmt::Debug for HttpExchangeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpExchangeValue(..)")
    }
}

impl fmt::Debug for WebSocketListenerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebSocketListenerValue(..)")
    }
}

impl fmt::Debug for WebSocketValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebSocketValue(..)")
    }
}

impl fmt::Debug for ProcessChildValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessChildValue(..)")
    }
}

impl fmt::Debug for ProcessPipeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessPipeValue(..)")
    }
}

impl fmt::Debug for ProcessCompletedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessCompletedValue(..)")
    }
}

impl fmt::Debug for ProcessSupervisorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessSupervisorValue(..)")
    }
}

impl fmt::Debug for UnixListenerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnixListenerValue(..)")
    }
}

impl fmt::Debug for UnixStreamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnixStreamValue(..)")
    }
}

impl fmt::Debug for TlsListenerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsListenerValue(..)")
    }
}

impl fmt::Debug for TlsStreamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsStreamValue(..)")
    }
}

impl PartialEq for ChannelValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for RngValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TaskValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TaskGroupValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for FileValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TcpListenerValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TcpStreamValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for UdpSocketValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for HttpListenerValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for HttpExchangeValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for WebSocketListenerValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for WebSocketValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for ProcessChildValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for ProcessPipeValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for ProcessCompletedValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for ProcessSupervisorValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for UnixListenerValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for UnixStreamValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TlsListenerValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl PartialEq for TlsStreamValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_condvar<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condvar.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_timeout_condvar<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: StdDuration,
) -> (MutexGuard<'a, T>, bool) {
    let (guard, timeout_result) = match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => poisoned.into_inner(),
    };
    (guard, timeout_result.timed_out())
}

impl RuntimeSchedulerWaiter {
    fn finish(&self, reason: RuntimeSchedulerWakeReason) {
        let mut state = lock_mutex(&self.state);
        if state.is_none() {
            *state = Some(reason);
            self.ready.notify_all();
        }
    }

    fn wait(&self) -> RuntimeSchedulerWakeReason {
        let mut state = lock_mutex(&self.state);
        loop {
            if let Some(reason) = *state {
                return reason;
            }
            state = wait_condvar(&self.ready, state);
        }
    }
}

struct RuntimeSchedulerHandle {
    id: u64,
    waiter: Arc<RuntimeSchedulerWaiter>,
    scheduler: Arc<RuntimeScheduler>,
}

impl RuntimeSchedulerHandle {
    fn wait(&self) -> RuntimeSchedulerWakeReason {
        self.waiter.wait()
    }
}

impl Drop for RuntimeSchedulerHandle {
    fn drop(&mut self) {
        self.scheduler.unregister(self.id);
    }
}

impl RuntimeScheduler {
    fn start() -> Arc<Self> {
        let scheduler = Arc::new(Self {
            state: Mutex::new(RuntimeSchedulerState {
                registrations: BTreeMap::new(),
            }),
            ready: Condvar::new(),
            next_id: AtomicU64::new(1),
        });
        let worker = scheduler.clone();
        std::thread::spawn(move || worker.run());
        scheduler
    }

    fn register(
        &self,
        recv_channels: Vec<ChannelValue>,
        ignore_closed_recv_channels: bool,
        send_channels: Vec<ChannelValue>,
        task_waits: Vec<TaskValue>,
        deadline: Option<Instant>,
        cancellation: Option<CancellationContext>,
    ) -> RuntimeSchedulerHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let waiter = Arc::new(RuntimeSchedulerWaiter {
            state: Mutex::new(None),
            ready: Condvar::new(),
        });
        let registration = RuntimeSchedulerRegistration {
            recv_channels,
            ignore_closed_recv_channels,
            send_channels,
            task_waits,
            deadline,
            cancellation,
            waiter: waiter.clone(),
        };
        lock_mutex(&self.state)
            .registrations
            .insert(id, registration);
        self.ready.notify_all();
        RuntimeSchedulerHandle {
            id,
            waiter,
            scheduler: runtime_scheduler().clone(),
        }
    }

    fn unregister(&self, id: u64) {
        let mut state = lock_mutex(&self.state);
        if state.registrations.remove(&id).is_some() {
            self.ready.notify_all();
        }
    }

    fn notify(&self) {
        let state = lock_mutex(&self.state);
        self.ready.notify_all();
        drop(state);
    }

    fn run(self: Arc<Self>) {
        loop {
            let mut state = lock_mutex(&self.state);
            while state.registrations.is_empty() {
                state = wait_condvar(&self.ready, state);
            }

            let mut finished = Vec::new();
            let mut next_deadline = None;
            let now = Instant::now();
            for (id, registration) in &state.registrations {
                if registration
                    .cancellation
                    .as_ref()
                    .is_some_and(CancellationContext::is_cancelled)
                {
                    registration
                        .waiter
                        .finish(RuntimeSchedulerWakeReason::Cancelled);
                    finished.push(*id);
                    continue;
                }

                if registration.recv_channels.iter().any(|channel| {
                    channel.is_ready_for_scheduler_recv(registration.ignore_closed_recv_channels)
                }) || registration
                    .send_channels
                    .iter()
                    .any(ChannelValue::is_ready_for_scheduler_send)
                    || registration
                        .task_waits
                        .iter()
                        .any(|task| task.completed_result().is_some())
                {
                    registration
                        .waiter
                        .finish(RuntimeSchedulerWakeReason::Ready);
                    finished.push(*id);
                    continue;
                }

                if let Some(deadline) = registration.deadline {
                    if now >= deadline {
                        registration
                            .waiter
                            .finish(RuntimeSchedulerWakeReason::TimedOut);
                        finished.push(*id);
                        continue;
                    }
                    next_deadline = Some(match next_deadline {
                        Some(current) => std::cmp::min(current, deadline),
                        None => deadline,
                    });
                }
            }

            for id in finished {
                state.registrations.remove(&id);
            }

            if state.registrations.is_empty() {
                continue;
            }

            state = if let Some(deadline) = next_deadline {
                let now = Instant::now();
                let timeout = deadline.saturating_duration_since(now);
                wait_timeout_condvar(&self.ready, state, timeout).0
            } else {
                wait_condvar(&self.ready, state)
            };
            drop(state);
        }
    }
}

fn runtime_scheduler() -> &'static Arc<RuntimeScheduler> {
    RUNTIME_SCHEDULER.get_or_init(RuntimeScheduler::start)
}

fn reactor_failure_diagnostic(operation: &str, error: io::Error) -> Diagnostic {
    Diagnostic::new(format!(
        "Aura runtime reactor failed while {operation}: {error}"
    ))
}

fn notify_runtime_scheduler_if_started() {
    if let Some(scheduler) = RUNTIME_SCHEDULER.get() {
        scheduler.notify();
    }
}

#[derive(Debug)]
struct BlockingIoPoolStartupError {
    message: String,
}

impl fmt::Display for BlockingIoPoolStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AU4006: {}", self.message)
    }
}

impl std::error::Error for BlockingIoPoolStartupError {}

impl BlockingIoWorkerStartupGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BlockingIoWorkerStartupState::Pending),
            ready: Condvar::new(),
        })
    }

    fn wait(&self) -> bool {
        let mut state = lock_mutex(&self.state);
        while *state == BlockingIoWorkerStartupState::Pending {
            state = wait_condvar(&self.ready, state);
        }
        *state == BlockingIoWorkerStartupState::Run
    }

    fn finish(&self, state: BlockingIoWorkerStartupState) {
        *lock_mutex(&self.state) = state;
        self.ready.notify_all();
    }
}

impl BlockingIoPool {
    fn new(config: BlockingIoPoolConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Mutex::new(BlockingIoPoolState {
                queue: VecDeque::new(),
                admission_waiters: VecDeque::new(),
                next_waiter_id: 0,
                active_jobs: 0,
                shutting_down: false,
            }),
            ready: Condvar::new(),
            idle: Condvar::new(),
            initialization: Mutex::new(BlockingIoPoolInitialization::NotStarted),
        })
    }

    fn startup_error(message: String) -> io::Error {
        io::Error::other(BlockingIoPoolStartupError { message })
    }

    fn ensure_started_with<F>(self: &Arc<Self>, spawner: &F) -> io::Result<()>
    where
        F: Fn(
            usize,
            String,
            Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<thread::JoinHandle<()>>,
    {
        let mut initialization = lock_mutex(&self.initialization);
        match &*initialization {
            BlockingIoPoolInitialization::Started(_) => return Ok(()),
            BlockingIoPoolInitialization::Failed(message) => {
                return Err(Self::startup_error(message.clone()));
            }
            BlockingIoPoolInitialization::NotStarted => {}
        }

        let startup_gate = BlockingIoWorkerStartupGate::new();
        let mut workers = Vec::new();
        if let Err(error) = workers.try_reserve_exact(self.config.worker_count) {
            let message = format!(
                "failed to reserve blocking-I/O worker handles for {} workers: {error}",
                self.config.worker_count
            );
            *initialization = BlockingIoPoolInitialization::Failed(message.clone());
            return Err(Self::startup_error(message));
        }
        for worker_index in 0..self.config.worker_count {
            let worker = self.clone();
            let worker_gate = startup_gate.clone();
            let entry: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
                if worker_gate.wait() {
                    worker.run();
                }
            });
            let name = format!("aura-blocking-io-{worker_index}");
            match spawner(worker_index, name, entry) {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    let message =
                        format!("failed to create blocking-I/O worker {worker_index}: {error}");
                    startup_gate.finish(BlockingIoWorkerStartupState::Stop);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    *initialization = BlockingIoPoolInitialization::Failed(message.clone());
                    return Err(Self::startup_error(message));
                }
            }
        }
        *initialization = BlockingIoPoolInitialization::Started(workers);
        startup_gate.finish(BlockingIoWorkerStartupState::Run);
        Ok(())
    }

    fn finish_admission_waiter(
        waiter: &BlockingIoAdmissionWaiter,
        outcome: BlockingIoAdmissionOutcome,
    ) {
        *lock_mutex(&waiter.outcome) = Some(outcome);
    }

    fn signal_admission_waiter(waiter: BlockingIoAdmissionWaiter) {
        let _ = waiter.completion.send(Value::Unit);
        waiter.completion.close();
    }

    fn fill_available_admission_slots_locked(
        &self,
        state: &mut BlockingIoPoolState,
    ) -> Vec<BlockingIoAdmissionWaiter> {
        let Some(capacity) = self.config.queue_capacity else {
            debug_assert!(state.admission_waiters.is_empty());
            return Vec::new();
        };
        let mut finished = Vec::new();
        while state.queue.len() < capacity {
            let Some(mut waiter) = state.admission_waiters.pop_front() else {
                break;
            };
            let outcome = if waiter
                .cancellation
                .as_ref()
                .is_some_and(CancellationContext::is_cancelled)
            {
                BlockingIoAdmissionOutcome::Cancelled
            } else if waiter
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                BlockingIoAdmissionOutcome::TimedOut
            } else {
                state.queue.push_back(
                    waiter
                        .job
                        .take()
                        .expect("a live blocking-I/O admission waiter owns one job"),
                );
                BlockingIoAdmissionOutcome::Accepted
            };
            Self::finish_admission_waiter(&waiter, outcome);
            finished.push(waiter);
        }
        finished
    }

    fn run(self: Arc<Self>) {
        loop {
            let (job, finished_waiters) = {
                let mut state = lock_mutex(&self.state);
                loop {
                    if let Some(job) = state.queue.pop_front() {
                        state.active_jobs += 1;
                        let finished = self.fill_available_admission_slots_locked(&mut state);
                        break (job, finished);
                    }
                    if state.shutting_down {
                        return;
                    }
                    state = wait_condvar(&self.ready, state);
                }
            };
            for waiter in finished_waiters {
                Self::signal_admission_waiter(waiter);
            }
            self.ready.notify_all();
            let _ = panic::catch_unwind(AssertUnwindSafe(job));
            let mut state = lock_mutex(&self.state);
            state.active_jobs = state
                .active_jobs
                .checked_sub(1)
                .expect("each running blocking-I/O job owns one active slot");
            if state.active_jobs == 0
                && state.queue.is_empty()
                && state.admission_waiters.is_empty()
            {
                self.idle.notify_all();
            }
        }
    }

    fn submit(
        self: &Arc<Self>,
        job: BlockingIoJob,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        self.submit_with_spawner(job, deadline, cancellation, &|worker_index, name, entry| {
            thread::Builder::new()
                .name(name)
                .spawn(entry)
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("worker {worker_index} could not start: {error}"),
                    )
                })
        })
    }

    fn submit_with_spawner<F>(
        self: &Arc<Self>,
        job: BlockingIoJob,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
        spawner: &F,
    ) -> io::Result<()>
    where
        F: Fn(
            usize,
            String,
            Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<thread::JoinHandle<()>>,
    {
        check_deadline_and_cancellation(deadline, cancellation)?;
        self.ensure_started_with(spawner)?;
        check_deadline_and_cancellation(deadline, cancellation)?;

        let Some(capacity) = self.config.queue_capacity else {
            let mut state = lock_mutex(&self.state);
            check_deadline_and_cancellation(deadline, cancellation)?;
            if state.shutting_down {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "blocking-I/O pool is shutting down",
                ));
            }
            state.queue.push_back(job);
            drop(state);
            self.ready.notify_one();
            return Ok(());
        };

        let completion = ChannelValue::new();
        let outcome = Arc::new(Mutex::new(None));
        let waiter_id;
        {
            let mut state = lock_mutex(&self.state);
            check_deadline_and_cancellation(deadline, cancellation)?;
            if state.shutting_down {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "blocking-I/O pool is shutting down",
                ));
            }
            if state.admission_waiters.is_empty() && state.queue.len() < capacity {
                state.queue.push_back(job);
                drop(state);
                self.ready.notify_one();
                return Ok(());
            }
            waiter_id = state.next_waiter_id;
            state.next_waiter_id = state.next_waiter_id.wrapping_add(1);
            state
                .admission_waiters
                .push_back(BlockingIoAdmissionWaiter {
                    id: waiter_id,
                    job: Some(job),
                    deadline,
                    cancellation: cancellation.cloned(),
                    outcome: outcome.clone(),
                    completion: completion.clone(),
                });
            #[cfg(test)]
            AFTER_BLOCKING_IO_ADMISSION_REGISTER_HOOK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
        }
        self.ready.notify_all();

        let wake = completion.recv_result_with_deadline(deadline, cancellation);
        let removal_outcome = match wake {
            RecvValueResult::TimedOut => Some(BlockingIoAdmissionOutcome::TimedOut),
            RecvValueResult::Cancelled => Some(BlockingIoAdmissionOutcome::Cancelled),
            RecvValueResult::Value(_) | RecvValueResult::Closed => None,
        };
        if let Some(removal_outcome) = removal_outcome {
            let mut removed = None;
            let mut state = lock_mutex(&self.state);
            if let Some(position) = state
                .admission_waiters
                .iter()
                .position(|waiter| waiter.id == waiter_id)
            {
                let waiter = state
                    .admission_waiters
                    .remove(position)
                    .expect("the located blocking-I/O waiter should remain present");
                Self::finish_admission_waiter(&waiter, removal_outcome);
                removed = Some(waiter);
            }
            drop(state);
            if let Some(waiter) = removed {
                Self::signal_admission_waiter(waiter);
            }
            self.ready.notify_all();
        }

        let result = match lock_mutex(&outcome)
            .expect("every completed blocking-I/O admission has one outcome")
        {
            BlockingIoAdmissionOutcome::Accepted => Ok(()),
            BlockingIoAdmissionOutcome::TimedOut => Err(timeout_resource_error()),
            BlockingIoAdmissionOutcome::Cancelled => Err(cancelled_resource_error()),
        };
        result
    }

    #[cfg(test)]
    fn worker_start_count(&self) -> usize {
        match &*lock_mutex(&self.initialization) {
            BlockingIoPoolInitialization::Started(workers) => workers.len(),
            BlockingIoPoolInitialization::NotStarted | BlockingIoPoolInitialization::Failed(_) => 0,
        }
    }

    #[cfg(test)]
    fn admission_waiter_count(&self) -> usize {
        lock_mutex(&self.state).admission_waiters.len()
    }

    #[cfg(test)]
    fn pending_job_count(&self) -> usize {
        lock_mutex(&self.state).queue.len()
    }

    #[cfg(test)]
    fn active_job_count(&self) -> usize {
        lock_mutex(&self.state).active_jobs
    }

    #[cfg(test)]
    fn wait_for_admission_waiters_for_test(&self, expected: usize) {
        let deadline = Instant::now() + StdDuration::from_secs(2);
        while self.admission_waiter_count() < expected {
            assert!(
                Instant::now() < deadline,
                "blocking-I/O admission waiter did not register"
            );
            thread::yield_now();
        }
    }

    #[cfg(test)]
    fn wait_until_idle_for_test(&self) {
        let mut state = lock_mutex(&self.state);
        while state.active_jobs != 0
            || !state.queue.is_empty()
            || !state.admission_waiters.is_empty()
        {
            state = wait_condvar(&self.idle, state);
        }
    }

    #[cfg(test)]
    fn shutdown_for_test(&self) {
        let waiters = {
            let mut state = lock_mutex(&self.state);
            state.shutting_down = true;
            let waiters: Vec<_> = state.admission_waiters.drain(..).collect();
            for waiter in &waiters {
                Self::finish_admission_waiter(waiter, BlockingIoAdmissionOutcome::Cancelled);
            }
            waiters
        };
        for waiter in waiters {
            Self::signal_admission_waiter(waiter);
        }
        self.ready.notify_all();
        let workers = {
            let mut initialization = lock_mutex(&self.initialization);
            match std::mem::replace(
                &mut *initialization,
                BlockingIoPoolInitialization::Failed("pool was shut down".to_string()),
            ) {
                BlockingIoPoolInitialization::Started(workers) => workers,
                BlockingIoPoolInitialization::NotStarted
                | BlockingIoPoolInitialization::Failed(_) => Vec::new(),
            }
        };
        for worker in workers {
            worker.join().expect("blocking-I/O test worker should stop");
        }
    }
}

fn blocking_io_pool() -> io::Result<&'static Arc<BlockingIoPool>> {
    static POOL: OnceLock<std::result::Result<Arc<BlockingIoPool>, String>> = OnceLock::new();
    match POOL.get_or_init(|| {
        blocking_io_pool_config()
            .map(BlockingIoPool::new)
            .map_err(|diagnostic| diagnostic.message)
    }) {
        Ok(pool) => Ok(pool),
        Err(message) => Err(BlockingIoPool::startup_error(message.clone())),
    }
}

const PROTOCOL_STEP_QUEUE_CAPACITY: usize = 64;
const PROTOCOL_STEP_WORKER_COUNT: usize = 2;
const PROTOCOL_STEP_WORKER_STACK_SIZE: usize = 2 * 1024 * 1024;

impl ProtocolStepPool {
    fn start() -> Arc<Self> {
        let pool = Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(PROTOCOL_STEP_QUEUE_CAPACITY)),
            ready: Condvar::new(),
        });
        for worker_index in 0..PROTOCOL_STEP_WORKER_COUNT {
            let worker = pool.clone();
            thread::Builder::new()
                .name(format!("aura-protocol-step-{worker_index}"))
                .stack_size(PROTOCOL_STEP_WORKER_STACK_SIZE)
                .spawn(move || worker.run())
                .expect("Aura protocol-step worker should start");
        }
        pool
    }

    fn run(self: Arc<Self>) {
        loop {
            let job = {
                let mut queue = lock_mutex(&self.queue);
                loop {
                    if let Some(job) = queue.pop_front() {
                        break job;
                    }
                    queue = wait_condvar(&self.ready, queue);
                }
            };
            job();
        }
    }

    fn try_submit(&self, job: ProtocolStepJob) -> std::result::Result<(), ProtocolStepJob> {
        let mut queue = lock_mutex(&self.queue);
        if queue.len() >= PROTOCOL_STEP_QUEUE_CAPACITY {
            return Err(job);
        }
        queue.push_back(job);
        drop(queue);
        self.ready.notify_one();
        Ok(())
    }
}

fn protocol_step_pool() -> &'static Arc<ProtocolStepPool> {
    // Runtime-global by design: the bounded service is shared by every
    // lightweight scheduler and its workers live until process exit.
    static POOL: OnceLock<Arc<ProtocolStepPool>> = OnceLock::new();
    POOL.get_or_init(ProtocolStepPool::start)
}

const JSON_CODEC_WORKER_COUNT: usize = 2;
const JSON_CODEC_IN_FLIGHT_CAPACITY: usize = JSON_CODEC_WORKER_COUNT;
const JSON_CODEC_WORKER_STACK_SIZE: usize = 2 * 1024 * 1024;

impl JsonCodecPool {
    fn start() -> Arc<Self> {
        Self::start_with_limits(JSON_CODEC_WORKER_COUNT, JSON_CODEC_IN_FLIGHT_CAPACITY)
    }

    fn start_with_limits(worker_count: usize, capacity: usize) -> Arc<Self> {
        let pool = Arc::new(Self {
            state: Mutex::new(JsonCodecPoolState {
                queue: VecDeque::with_capacity(capacity),
                in_flight: 0,
            }),
            ready: Condvar::new(),
            available: Condvar::new(),
            availability_signal: ChannelValue::with_capacity(capacity),
            capacity,
        });
        for worker_index in 0..worker_count {
            let worker = pool.clone();
            thread::Builder::new()
                .name(format!("aura-json-codec-{worker_index}"))
                .stack_size(JSON_CODEC_WORKER_STACK_SIZE)
                .spawn(move || worker.run())
                .expect("Aura JSON codec worker should start");
        }
        pool
    }

    fn run(self: Arc<Self>) {
        loop {
            let job = {
                let mut state = lock_mutex(&self.state);
                loop {
                    if let Some(job) = state.queue.pop_front() {
                        break job;
                    }
                    state = wait_condvar(&self.ready, state);
                }
            };
            let outcome = panic::catch_unwind(AssertUnwindSafe(job.operation))
                .map_err(|payload| JsonCodecServiceError::Panicked(task_panic_message(&*payload)))
                .and_then(|outcome| outcome.map_err(JsonCodecServiceError::Codec));
            *lock_mutex(&job.result) = Some(outcome);
            let _ = job.completion.send(Value::Unit);
            job.completion.close();
            let mut state = lock_mutex(&self.state);
            state.in_flight = state
                .in_flight
                .checked_sub(1)
                .expect("every JSON codec job owns one in-flight slot");
            drop(state);
            self.available.notify_one();
            let _ = self.availability_signal.try_send(Value::Unit);
        }
    }

    fn try_reserve(&self) -> bool {
        let mut state = lock_mutex(&self.state);
        if state.in_flight >= self.capacity {
            return false;
        }
        state.in_flight += 1;
        true
    }

    fn release_reservation(&self) {
        let mut state = lock_mutex(&self.state);
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("released JSON codec reservations are in flight");
        drop(state);
        self.available.notify_one();
        let _ = self.availability_signal.try_send(Value::Unit);
    }

    fn submit_reserved(&self, job: JsonCodecJob) {
        let mut state = lock_mutex(&self.state);
        debug_assert!(state.queue.len() < self.capacity);
        state.queue.push_back(job);
        drop(state);
        self.ready.notify_one();
    }

    fn reserve_blocking(&self) {
        let mut state = lock_mutex(&self.state);
        while state.in_flight >= self.capacity {
            state = wait_condvar(&self.available, state);
        }
        state.in_flight += 1;
    }
}

fn json_codec_pool() -> &'static Arc<JsonCodecPool> {
    // Runtime-global by design: bounded codec workers are isolated from the
    // protocol and blocking-I/O services and live until process exit.
    static POOL: OnceLock<Arc<JsonCodecPool>> = OnceLock::new();
    POOL.get_or_init(JsonCodecPool::start)
}

pub(crate) fn wait_for_runtime_scheduler(
    recv_channels: Vec<ChannelValue>,
    ignore_closed_recv_channels: bool,
    send_channels: Vec<ChannelValue>,
    task_waits: Vec<TaskValue>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> RuntimeSchedulerWakeReason {
    if cancellation.is_some_and(CancellationContext::is_cancelled) {
        return RuntimeSchedulerWakeReason::Cancelled;
    }
    if recv_channels
        .iter()
        .any(|channel| channel.is_ready_for_scheduler_recv(ignore_closed_recv_channels))
        || send_channels
            .iter()
            .any(ChannelValue::is_ready_for_scheduler_send)
        || task_waits
            .iter()
            .any(|task| task.completed_result().is_some())
    {
        return RuntimeSchedulerWakeReason::Ready;
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return RuntimeSchedulerWakeReason::TimedOut;
    }

    if let Some(reason) = yield_current_lightweight_wait(TaskWaitRegistration {
        recv_channels: recv_channels.clone(),
        ignore_closed_recv_channels,
        send_channels: send_channels.clone(),
        task_waits: task_waits.clone(),
        deadline,
        cancellation: cancellation.cloned(),
        fd_wait: None,
    }) {
        return reason;
    }

    runtime_scheduler()
        .register(
            recv_channels,
            ignore_closed_recv_channels,
            send_channels,
            task_waits,
            deadline,
            cancellation.cloned(),
        )
        .wait()
}

enum SelectRuntimeSource {
    Queue(ChannelValue),
    Task(TaskValue),
    Deadline(Instant),
}

enum ValidatedSelectRuntimeSource {
    Queue(ChannelValue),
    Task(TaskValue),
    Deadline(StdDuration),
}

#[cfg(test)]
thread_local! {
    static AFTER_SELECT_SOURCE_VALIDATION_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
    static AFTER_SELECT_QUEUE_COMMIT_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_select_source_validation_hook(hook: impl FnOnce() + 'static) {
    AFTER_SELECT_SOURCE_VALIDATION_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "a select-source validation hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
pub(crate) fn install_after_select_queue_commit_hook(hook: impl FnOnce() + 'static) {
    AFTER_SELECT_QUEUE_COMMIT_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "an after-select queue commit hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

fn select_runtime_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::coded("AU4001", message)
}

fn select_runtime_sources(values: Vec<Value>) -> Result<Vec<SelectRuntimeSource>> {
    #[cfg(test)]
    let after_validation_hook =
        AFTER_SELECT_SOURCE_VALIDATION_HOOK.with(|slot| slot.borrow_mut().take());
    if values.is_empty() {
        return Err(select_runtime_error(
            "select runtime requires at least one Queue, Task, or Duration source",
        ));
    }
    if i64::try_from(values.len()).is_err() {
        return Err(select_runtime_error(
            "select runtime source count exceeds the int64 outcome-index range",
        ));
    }

    let validated_sources = values
        .into_iter()
        .enumerate()
        .map(|(index, value)| match value {
            Value::Channel(channel) => Ok(ValidatedSelectRuntimeSource::Queue(channel)),
            Value::Task(task) => Ok(ValidatedSelectRuntimeSource::Task(task)),
            Value::Duration(nanoseconds) => {
                let duration = duration_to_host_timer(nanoseconds, "select deadline")
                    .map_err(|error| select_runtime_error(error.to_string()))?;
                Ok(ValidatedSelectRuntimeSource::Deadline(duration))
            }
            _ => Err(select_runtime_error(format!(
                "select source {index} must be a Queue, Task, or Duration"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;

    #[cfg(test)]
    if let Some(hook) = after_validation_hook {
        hook();
    }

    let wait_started = Instant::now();
    validated_sources
        .into_iter()
        .map(|source| match source {
            ValidatedSelectRuntimeSource::Queue(channel) => Ok(SelectRuntimeSource::Queue(channel)),
            ValidatedSelectRuntimeSource::Task(task) => Ok(SelectRuntimeSource::Task(task)),
            ValidatedSelectRuntimeSource::Deadline(duration) => wait_started
                .checked_add(duration)
                .map(SelectRuntimeSource::Deadline)
                .ok_or_else(|| {
                    select_runtime_error("select deadline exceeds the host deadline range")
                }),
        })
        .collect()
}

fn select_runtime_probe(
    sources: &[SelectRuntimeSource],
    cancellation: Option<&CancellationContext>,
) -> Option<Value> {
    if cancellation.is_some_and(CancellationContext::is_cancelled) {
        return Some(select_outcome_cancelled());
    }

    let now = Instant::now();
    for (index, source) in sources.iter().enumerate() {
        let index = i64::try_from(index).expect("select source count was validated");
        match source {
            SelectRuntimeSource::Queue(channel) => match channel.try_recv() {
                TryRecvResult::Value(value) => {
                    #[cfg(test)]
                    AFTER_SELECT_QUEUE_COMMIT_HOOK.with(|slot| {
                        if let Some(hook) = slot.borrow_mut().take() {
                            hook();
                        }
                    });
                    return Some(select_outcome_queue(index, queue_receive_item(value)));
                }
                TryRecvResult::Closed => {
                    #[cfg(test)]
                    AFTER_SELECT_QUEUE_COMMIT_HOOK.with(|slot| {
                        if let Some(hook) = slot.borrow_mut().take() {
                            hook();
                        }
                    });
                    return Some(select_outcome_queue(index, queue_receive_closed()));
                }
                TryRecvResult::Empty => {}
            },
            SelectRuntimeSource::Task(task) => match task.completed_result_observed() {
                Some(TaskExecutionResult::Ready(Ok(value))) => {
                    return Some(select_outcome_task(index, task_result_ready(value)));
                }
                Some(TaskExecutionResult::Ready(Err(error))) => {
                    return Some(select_outcome_task(index, task_result_error(error.message)));
                }
                Some(TaskExecutionResult::Cancelled) => {
                    return Some(select_outcome_task(index, task_result_cancelled()));
                }
                None => {}
            },
            SelectRuntimeSource::Deadline(deadline) if now >= *deadline => {
                return Some(select_outcome_deadline(index));
            }
            SelectRuntimeSource::Deadline(_) => {}
        }
    }
    None
}

/// Shared Phase 5.8 heterogeneous wait primitive used by the MIR and direct
/// adapters. Descriptors are already-evaluated source values in source order.
///
/// Validation and non-repeatable Task observation claims happen before any
/// readiness is observed. The wait itself is one persistent-reactor
/// registration; wakeups trigger cancellation-first, original-index
/// arbitration and never choose a winner directly.
pub(crate) fn select_runtime_values(
    values: Vec<Value>,
    cancellation: Option<&CancellationContext>,
) -> Result<Value> {
    let sources = select_runtime_sources(values)?;
    for source in &sources {
        if let SelectRuntimeSource::Task(task) = source {
            task.claim_result_observation()?;
        }
    }

    let recv_channels = sources
        .iter()
        .filter_map(|source| match source {
            SelectRuntimeSource::Queue(channel) => Some(channel.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let task_waits = sources
        .iter()
        .filter_map(|source| match source {
            SelectRuntimeSource::Task(task) => Some(task.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let deadline = sources.iter().filter_map(|source| match source {
        SelectRuntimeSource::Deadline(deadline) => Some(*deadline),
        _ => None,
    });
    let deadline = deadline.min();

    loop {
        if let Some(outcome) = select_runtime_probe(&sources, cancellation) {
            return Ok(outcome);
        }

        match wait_for_runtime_scheduler(
            recv_channels.clone(),
            false,
            Vec::new(),
            task_waits.clone(),
            deadline,
            cancellation,
        ) {
            RuntimeSchedulerWakeReason::Cancelled => return Ok(select_outcome_cancelled()),
            RuntimeSchedulerWakeReason::Ready | RuntimeSchedulerWakeReason::TimedOut => {}
        }
    }
}

pub(crate) fn sleep_with_runtime_scheduler(
    duration: StdDuration,
    cancellation: Option<&CancellationContext>,
) -> io::Result<RuntimeSchedulerWakeReason> {
    let deadline = deadline_from_timeout_labeled(Some(duration), "sleep duration")?;
    Ok(wait_for_runtime_scheduler(
        Vec::new(),
        false,
        Vec::new(),
        Vec::new(),
        deadline,
        cancellation,
    ))
}

thread_local! {
    static CURRENT_LIGHTWEIGHT_TASK_CONTEXT: RefCell<Option<Rc<LightweightTaskContext>>> =
        const { RefCell::new(None) };
    static CURRENT_LIGHTWEIGHT_TASK_CANCELLATION: std::cell::RefCell<Option<CancellationContext>> =
        const { std::cell::RefCell::new(None) };
    static CURRENT_LIGHTWEIGHT_TASK_EXIT: std::cell::RefCell<Option<TaskExecutionResult>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(test)]
    static AFTER_TASK_WAIT_SUBSCRIBE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
    #[cfg(test)]
    static NEXT_TASK_WAIT_READY_ENQUEUE_PROBE: RefCell<Option<TaskWaitReadyEnqueueProbe>> =
        const { RefCell::new(None) };
    #[cfg(test)]
    static AFTER_TASK_GROUP_SEND_REACHABILITY_INITIAL_CHECK_HOOK:
        RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_task_group_send_reachability_initial_check_hook(
    hook: impl FnOnce() + 'static,
) {
    AFTER_TASK_GROUP_SEND_REACHABILITY_INITIAL_CHECK_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "a task-group send-reachability hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

static LIGHTWEIGHT_TASK_PANIC_HOOK: Once = Once::new();

#[cfg(test)]
struct TaskWaitReadyEnqueueProbe {
    subscription: Option<ReactorSubscriptionKey>,
    counter: Arc<AtomicUsize>,
}

#[cfg(test)]
pub(crate) fn install_after_task_wait_subscribe_hook(hook: impl FnOnce() + 'static) {
    AFTER_TASK_WAIT_SUBSCRIBE_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "a task-wait subscription race hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
pub(crate) fn install_next_task_wait_ready_enqueue_counter(counter: Arc<AtomicUsize>) {
    NEXT_TASK_WAIT_READY_ENQUEUE_PROBE.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "a task-wait ready-enqueue counter is already installed"
        );
        *slot.borrow_mut() = Some(TaskWaitReadyEnqueueProbe {
            subscription: None,
            counter,
        });
    });
}

#[cfg(test)]
pub(crate) fn clear_task_wait_ready_enqueue_counter() {
    NEXT_TASK_WAIT_READY_ENQUEUE_PROBE.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
fn bind_task_wait_ready_enqueue_counter(subscription: ReactorSubscriptionKey) {
    NEXT_TASK_WAIT_READY_ENQUEUE_PROBE.with(|slot| {
        if let Some(probe) = slot.borrow_mut().as_mut() {
            probe.subscription.get_or_insert(subscription);
        }
    });
}

#[cfg(test)]
fn record_task_wait_ready_enqueue(subscription: ReactorSubscriptionKey) {
    NEXT_TASK_WAIT_READY_ENQUEUE_PROBE.with(|slot| {
        let slot = slot.borrow();
        if let Some(probe) = slot
            .as_ref()
            .filter(|probe| probe.subscription == Some(subscription))
        {
            probe.counter.fetch_add(1, Ordering::SeqCst);
        }
    });
}

struct LightweightTaskContextGuard {
    previous: Option<Rc<LightweightTaskContext>>,
    previous_cancellation: Option<CancellationContext>,
}

impl Drop for LightweightTaskContextGuard {
    fn drop(&mut self) {
        CURRENT_LIGHTWEIGHT_TASK_CONTEXT.with(|slot| *slot.borrow_mut() = self.previous.take());
        CURRENT_LIGHTWEIGHT_TASK_CANCELLATION
            .with(|slot| *slot.borrow_mut() = self.previous_cancellation.take());
    }
}

fn enter_lightweight_task_context(
    context: &Rc<LightweightTaskContext>,
) -> LightweightTaskContextGuard {
    let previous = CURRENT_LIGHTWEIGHT_TASK_CONTEXT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let previous = slot.take();
        *slot = Some(context.clone());
        previous
    });
    let previous_cancellation = CURRENT_LIGHTWEIGHT_TASK_CANCELLATION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let previous = slot.clone();
        *slot = context.cancellation.clone();
        previous
    });
    LightweightTaskContextGuard {
        previous,
        previous_cancellation,
    }
}

fn with_current_lightweight_task_context<T>(
    f: impl FnOnce(&LightweightTaskContext) -> T,
) -> Option<T> {
    let context = CURRENT_LIGHTWEIGHT_TASK_CONTEXT.with(|slot| slot.borrow().clone());
    context.as_deref().map(f)
}

fn current_lightweight_task() -> Option<TaskValue> {
    with_current_lightweight_task_context(|context| context.task_state.upgrade())
        .flatten()
        .map(|inner| TaskValue { inner })
}

pub(crate) fn current_lightweight_task_cancellation() -> Option<CancellationContext> {
    CURRENT_LIGHTWEIGHT_TASK_CANCELLATION.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_lightweight_task_id() -> Option<u64> {
    with_current_lightweight_task_context(|context| context.task_id)
}

#[cfg(test)]
pub(crate) fn current_lightweight_worker_index() -> Option<usize> {
    with_current_lightweight_task_context(|context| context.worker_index)
}

#[derive(Debug)]
pub(crate) struct TaskCancelledSignal;

fn task_panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else {
        "task panicked with a non-string payload".to_string()
    }
}

fn install_lightweight_task_panic_hook() {
    LIGHTWEIGHT_TASK_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if info.payload().is::<TaskCancelledSignal>()
                || info.payload().is::<LightweightTaskFailureSignal>()
            {
                return;
            }
            previous(info);
        }));
    });
}

fn finalize_task_execution<F>(entry: F) -> TaskExecutionResult
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic>,
{
    match panic::catch_unwind(AssertUnwindSafe(entry)) {
        Ok(result) => TaskExecutionResult::Ready(result),
        Err(payload) if payload.is::<TaskCancelledSignal>() => TaskExecutionResult::Cancelled,
        Err(payload) => match payload.downcast::<LightweightTaskFailureSignal>() {
            Ok(signal) => TaskExecutionResult::Ready(Err(signal.0)),
            Err(payload) => TaskExecutionResult::Ready(Err(Diagnostic::new(format!(
                "internal error: Aura task panicked: {}",
                task_panic_message(&*payload)
            )))),
        },
    }
}

pub(crate) fn run_blocking_io<T, F>(
    operation: F,
    cancellation: Option<&CancellationContext>,
) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    run_blocking_io_with_deadline(operation, None, cancellation)
}

pub(crate) fn run_protocol_step<T, F>(operation: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    run_protocol_step_with_admission(operation, protocol_step_pool(), None, None)
}

fn run_protocol_step_with_admission<T, F>(
    operation: F,
    pool: &Arc<ProtocolStepPool>,
    admission_deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    check_deadline_and_cancellation(admission_deadline, cancellation)?;
    let completion = ChannelValue::new();
    let result = Arc::new(Mutex::new(None::<io::Result<T>>));
    let result_slot = result.clone();
    let completion_signal = completion.clone();
    let mut job: ProtocolStepJob = Box::new(move || {
        let outcome = panic::catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|payload| {
            Err(io::Error::other(format!(
                "protocol step panicked: {}",
                task_panic_message(&*payload)
            )))
        });
        *lock_mutex(&result_slot) = Some(outcome);
        let _ = completion_signal.send(Value::Unit);
        completion_signal.close();
    });

    loop {
        match pool.try_submit(job) {
            Ok(()) => break,
            Err(returned) => {
                job = returned;
                check_deadline_and_cancellation(admission_deadline, cancellation)?;
                if yield_now_current_lightweight_task().is_none() {
                    thread::yield_now();
                }
            }
        }
    }

    // Protocol state is moved into each bounded step. Always await that step,
    // even after cancellation or timeout, so callers can restore owned state
    // before consulting the reactor for the next readiness wait.
    match completion.recv_result_with_deadline(None, None) {
        RecvValueResult::Value(_) | RecvValueResult::Closed => {
            lock_mutex(&result).take().unwrap_or_else(|| {
                Err(io::Error::other(
                    "protocol step completed without returning a result",
                ))
            })
        }
        RecvValueResult::TimedOut | RecvValueResult::Cancelled => {
            unreachable!("protocol-step completion waits have no deadline or cancellation")
        }
    }
}

fn run_protocol_step_before<T, F>(
    operation: F,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    check_deadline_and_cancellation(deadline, cancellation)?;
    let result =
        run_protocol_step_with_admission(operation, protocol_step_pool(), deadline, cancellation)?;
    check_deadline_and_cancellation(deadline, cancellation)?;
    Ok(result)
}

fn run_protocol_state_step<S, T, F>(
    state: S,
    operation: F,
    admission_deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(S, io::Result<T>)>
where
    S: Send + 'static,
    T: Send + 'static,
    F: FnOnce(&mut S) -> T + Send + 'static,
{
    run_protocol_state_step_on_pool(
        state,
        operation,
        protocol_step_pool(),
        admission_deadline,
        cancellation,
    )
}

fn run_protocol_state_step_on_pool<S, T, F>(
    state: S,
    operation: F,
    pool: &Arc<ProtocolStepPool>,
    admission_deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(S, io::Result<T>)>
where
    S: Send + 'static,
    T: Send + 'static,
    F: FnOnce(&mut S) -> T + Send + 'static,
{
    let state = Arc::new(Mutex::new(Some(state)));
    let worker_state = state.clone();
    let result = run_protocol_step_with_admission(
        move || {
            let mut owned_state = lock_mutex(&worker_state)
                .take()
                .expect("protocol state should be present before its worker step");
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| operation(&mut owned_state)))
                .map_err(|payload| {
                    io::Error::other(format!(
                        "protocol state step panicked: {}",
                        task_panic_message(&*payload)
                    ))
                });
            *lock_mutex(&worker_state) = Some(owned_state);
            Ok(outcome)
        },
        pool,
        admission_deadline,
        cancellation,
    );
    let state = lock_mutex(&state)
        .take()
        .expect("protocol state should be restored after every worker outcome");
    Ok((
        state,
        match result {
            Ok(outcome) => outcome,
            Err(error) => Err(error),
        },
    ))
}

fn run_blocking_io_with_deadline<T, F>(
    operation: F,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    let pool = match blocking_io_pool() {
        Ok(pool) => pool,
        Err(error) => raise_blocking_io_pool_startup_failure(error),
    };
    run_blocking_io_with_deadline_on_pool(operation, deadline, cancellation, pool)
}

fn raise_blocking_io_pool_startup_failure(error: io::Error) -> ! {
    let message = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<BlockingIoPoolStartupError>())
        .map(|source| source.message.clone())
        .unwrap_or_else(|| format!("failed to initialize the blocking-I/O pool: {error}"));
    panic::panic_any(LightweightTaskFailureSignal(Diagnostic::coded(
        "AU4006", message,
    )));
}

fn preserve_blocking_io_submission_error(result: io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .get_ref()
                .is_some_and(|source| source.is::<BlockingIoPoolStartupError>()) =>
        {
            raise_blocking_io_pool_startup_failure(error)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn catch_lightweight_task_failure<T>(
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match panic::catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(payload) => match payload.downcast::<LightweightTaskFailureSignal>() {
            Ok(signal) => Err(signal.0),
            Err(payload) => panic::resume_unwind(payload),
        },
    }
}

fn run_blocking_io_with_deadline_on_pool<T, F>(
    operation: F,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
    pool: &Arc<BlockingIoPool>,
) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    check_deadline_and_cancellation(deadline, cancellation)?;

    let completion = ChannelValue::new();
    let result = Arc::new(Mutex::new(None::<io::Result<T>>));
    let result_slot = result.clone();
    let completion_signal = completion.clone();
    preserve_blocking_io_submission_error(pool.submit(
        Box::new(move || {
            let outcome = panic::catch_unwind(AssertUnwindSafe(operation))
                .map_err(|payload| {
                    io::Error::other(format!(
                        "blocking I/O operation panicked: {}",
                        task_panic_message(&*payload)
                    ))
                })
                .and_then(|outcome| outcome);
            *lock_mutex(&result_slot) = Some(outcome);
            let _ = completion_signal.send(Value::Unit);
            completion_signal.close();
        }),
        deadline,
        cancellation,
    ))?;

    match completion.recv_result_with_deadline(deadline, cancellation) {
        RecvValueResult::Value(_) => lock_mutex(&result).take().unwrap_or_else(|| {
            Err(io::Error::other(
                "blocking I/O task completed without returning a result",
            ))
        }),
        RecvValueResult::TimedOut => Err(timeout_resource_error()),
        RecvValueResult::Cancelled => Err(cancelled_resource_error()),
        RecvValueResult::Closed => lock_mutex(&result).take().unwrap_or_else(|| {
            Err(io::Error::other(
                "blocking I/O wait ended before the task completed",
            ))
        }),
    }
}

fn yield_current_lightweight_task(wait: TaskYield) -> Option<RuntimeSchedulerWakeReason> {
    // Do not keep the task context's Rc alive on the coroutine stack across a
    // suspension. A generated task may be force-reset at that point, and any
    // stack-owned Rc would be deliberately abandoned with its generated
    // frames.
    let yielder_ptr = with_current_lightweight_task_context(|context| context.yielder.get())?;
    if yielder_ptr.is_null() {
        None
    } else {
        let yielder = unsafe { &*yielder_ptr };
        Some(yielder.suspend(wait))
    }
}

fn yield_current_lightweight_wait(
    wait: TaskWaitRegistration,
) -> Option<RuntimeSchedulerWakeReason> {
    yield_current_lightweight_task(TaskYield::Wait(wait))
}

fn yield_now_current_lightweight_task() -> Option<RuntimeSchedulerWakeReason> {
    yield_current_lightweight_task(TaskYield::YieldNow)
}

pub(crate) fn yield_now_with_runtime_scheduler() {
    let _ = yield_now_current_lightweight_task();
}

fn exit_current_lightweight_task(result: TaskExecutionResult) -> ! {
    CURRENT_LIGHTWEIGHT_TASK_EXIT.with(|slot| *slot.borrow_mut() = Some(result));
    let _ = yield_current_lightweight_task(TaskYield::Exit);
    std::process::abort()
}

pub(crate) fn fail_current_lightweight_task(diagnostic: Diagnostic) -> ! {
    exit_current_lightweight_task(TaskExecutionResult::Ready(Err(diagnostic)))
}

pub(crate) fn cancel_current_lightweight_task_boundary() -> ! {
    exit_current_lightweight_task(TaskExecutionResult::Cancelled)
}

impl TaskWaitRegistration {
    fn reachability_snapshot(&self) -> TaskWaitReachability {
        TaskWaitReachability {
            recv_channels: self
                .recv_channels
                .iter()
                .map(|channel| Arc::downgrade(&channel.inner))
                .collect(),
            ignore_closed_recv_channels: self.ignore_closed_recv_channels,
            send_channels: self
                .send_channels
                .iter()
                .map(|channel| Arc::downgrade(&channel.inner))
                .collect(),
            task_waits: self
                .task_waits
                .iter()
                .map(|task| Arc::downgrade(&task.inner))
                .collect(),
            deadline: self.deadline,
            cancellation: self.cancellation.clone(),
        }
    }

    fn ready_reason(&self, fd_ready: bool) -> Option<RuntimeSchedulerWakeReason> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationContext::is_cancelled)
        {
            return Some(RuntimeSchedulerWakeReason::Cancelled);
        }
        if self
            .recv_channels
            .iter()
            .any(|channel| channel.is_ready_for_scheduler_recv(self.ignore_closed_recv_channels))
            || self
                .send_channels
                .iter()
                .any(ChannelValue::is_ready_for_scheduler_send)
            || self
                .task_waits
                .iter()
                .any(|task| task.completed_result().is_some())
        {
            return Some(RuntimeSchedulerWakeReason::Ready);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Some(RuntimeSchedulerWakeReason::TimedOut);
        }
        if fd_ready {
            return Some(RuntimeSchedulerWakeReason::Ready);
        }
        None
    }

    fn subscribe_reactor(&self, subscription: &ReactorSubscription) {
        for channel in &self.recv_channels {
            channel.subscribe_reactor_recv(subscription, self.ignore_closed_recv_channels);
        }
        for channel in &self.send_channels {
            channel.subscribe_reactor_send(subscription);
        }
        for task in &self.task_waits {
            task.subscribe_reactor_completion(subscription);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.subscribe_reactor(subscription);
        }
        #[cfg(test)]
        bind_task_wait_ready_enqueue_counter(subscription.identity());
        #[cfg(test)]
        AFTER_TASK_WAIT_SUBSCRIBE_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
    }

    fn unsubscribe_reactor(&self, subscription: &ReactorSubscription) {
        for channel in &self.recv_channels {
            channel.unsubscribe_reactor_recv(subscription);
        }
        for channel in &self.send_channels {
            channel.unsubscribe_reactor_send(subscription);
        }
        for task in &self.task_waits {
            task.unsubscribe_reactor_completion(subscription);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.unsubscribe_reactor(subscription);
        }
    }
}

fn new_lightweight_task_state(result_is_repeatable: bool) -> Arc<TaskState> {
    Arc::new(TaskState {
        handle: Mutex::new(TaskHandle::Running),
        ready: Condvar::new(),
        lightweight: true,
        result_is_repeatable,
        result_observation_claimed: AtomicBool::new(false),
        waits_without_deadline: AtomicBool::new(false),
        current_wait: Mutex::new(None),
        join_dependencies: Mutex::new(None),
        observed_failure: AtomicBool::new(false),
        completion_reactor_subscribers: Mutex::new(HashMap::new()),
        group_failure_wake_flags: Mutex::new(Vec::new()),
        group_completion_wake_flags: Mutex::new(Vec::new()),
        runtime_type_name: Mutex::new(None),
    })
}

fn complete_lightweight_task_state(task_state: &Arc<TaskState>, result: TaskExecutionResult) {
    task_state
        .waits_without_deadline
        .store(false, Ordering::SeqCst);
    *lock_mutex(&task_state.current_wait) = None;
    *lock_mutex(&task_state.join_dependencies) = None;
    {
        let mut state = lock_mutex(&task_state.handle);
        *state = TaskHandle::Completed(result.clone());
        task_state.ready.notify_all();
    }
    wake_reactor_subscribers(&task_state.completion_reactor_subscribers);
    notify_group_failure_wake_flags(task_state, &result);
    notify_group_completion_wake_flags(task_state);
    notify_runtime_scheduler_if_started();
}

fn cancel_unadmitted_lightweight_task(request: PreparedLightweightTask) -> Option<Diagnostic> {
    let PreparedLightweightTask {
        state,
        stack,
        cancellation,
        entry,
        forced_exit_cleanup,
    } = request;
    drop(entry);
    drop(cancellation);
    drop(stack);
    let cleanup_panic = forced_exit_cleanup.and_then(|cleanup| {
        panic::catch_unwind(AssertUnwindSafe(cleanup))
            .err()
            .map(|payload| {
                Diagnostic::new(format!(
                    "internal error: Aura task cleanup panicked: {}",
                    task_panic_message(&*payload)
                ))
            })
    });
    complete_lightweight_task_state(&state, TaskExecutionResult::Cancelled);
    cleanup_panic
}

fn prepare_lightweight_task(
    cancellation: Option<CancellationContext>,
    stack_size: Option<usize>,
    result_is_repeatable: bool,
    entry: LightweightTaskEntry,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
) -> std::result::Result<(TaskValue, PreparedLightweightTask), Diagnostic> {
    let stack = allocate_lightweight_task_stack(stack_size.unwrap_or(LIGHTWEIGHT_TASK_STACK_SIZE))?;
    let state = new_lightweight_task_state(result_is_repeatable);
    let task = TaskValue {
        inner: state.clone(),
    };
    Ok((
        task,
        PreparedLightweightTask {
            state,
            stack,
            cancellation,
            entry,
            forced_exit_cleanup,
        },
    ))
}

pub(crate) fn decode_lightweight_worker_count(
    override_value: Option<&OsStr>,
    available_workers: usize,
) -> std::result::Result<usize, Diagnostic> {
    let Some(raw) = override_value else {
        return if available_workers > 0 {
            Ok(available_workers)
        } else {
            Err(Diagnostic::coded(
                "AU4006",
                "Aura runtime detected zero available workers",
            ))
        };
    };
    let rendered = raw.to_string_lossy();
    let valid_digits = !rendered.is_empty() && rendered.bytes().all(|byte| byte.is_ascii_digit());
    if valid_digits {
        if let Ok(workers) = rendered.parse::<usize>() {
            if workers > 0 {
                return Ok(workers);
            }
        }
    }
    Err(Diagnostic::coded(
        "AU4006",
        format!("invalid AURA_WORKERS value `{rendered}`: expected a positive integer"),
    ))
}

fn lightweight_worker_count() -> std::result::Result<usize, Diagnostic> {
    let available = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    decode_lightweight_worker_count(std::env::var_os("AURA_WORKERS").as_deref(), available)
}

#[derive(Clone, Copy, Default)]
struct LightweightWorkerFaults {
    #[cfg(test)]
    reactor_initialization_at: Option<usize>,
    #[cfg(test)]
    worker_panic_at: Option<usize>,
    #[cfg(test)]
    thread_spawn_at: Option<usize>,
}

impl LightweightWorkerFaults {
    fn reactor_initialization_error(self, worker_index: usize) -> Option<io::Error> {
        #[cfg(test)]
        if self.reactor_initialization_at == Some(worker_index) {
            return Some(io::Error::other(
                "injected Aura worker reactor initialization failure",
            ));
        }
        let _ = worker_index;
        None
    }

    fn should_panic_worker(self, worker_index: usize) -> bool {
        #[cfg(test)]
        if self.worker_panic_at == Some(worker_index) {
            return true;
        }
        let _ = worker_index;
        false
    }

    fn thread_spawn_error(self, worker_index: usize) -> Option<io::Error> {
        #[cfg(test)]
        if self.thread_spawn_at == Some(worker_index) {
            return Some(io::Error::other(
                "injected Aura worker thread spawn failure",
            ));
        }
        let _ = worker_index;
        None
    }
}

impl LightweightWorkerCoordinator {
    fn new(worker_count: usize) -> Arc<Self> {
        Arc::new(Self {
            inboxes: (0..worker_count)
                .map(|_| LightweightWorkerInbox {
                    requests: Mutex::new(VecDeque::new()),
                    reactor: Mutex::new(None),
                    cleanup_complete: AtomicBool::new(false),
                })
                .collect(),
            next_worker: AtomicUsize::new(0),
            next_task_id: AtomicU64::new(1),
            pending_requests: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            cleanup_workers: AtomicUsize::new(0),
            fatal: Mutex::new(None),
        })
    }

    fn submit(&self, request: PreparedLightweightTask) {
        let worker_index = self.next_worker.fetch_add(1, Ordering::SeqCst) % self.inboxes.len();
        self.pending_requests.fetch_add(1, Ordering::SeqCst);
        lock_mutex(&self.inboxes[worker_index].requests).push_back(request);
        let reactor = lock_mutex(&self.inboxes[worker_index].reactor).clone();
        if let Some(reactor) = reactor {
            if let Err(error) = reactor.notify_control() {
                self.fail(Diagnostic::coded(
                    "AU4005",
                    format!("failed to wake Aura worker {worker_index}: {error}"),
                ));
            }
        }
    }

    fn register_reactor(&self, worker_index: usize, reactor: ReactorHandle) {
        *lock_mutex(&self.inboxes[worker_index].reactor) = Some(reactor.clone());
        if self.shutdown.load(Ordering::SeqCst) {
            let _ = reactor.notify_control();
        }
    }

    fn take_request(&self, worker_index: usize) -> Option<PreparedLightweightTask> {
        let request = lock_mutex(&self.inboxes[worker_index].requests).pop_front()?;
        self.pending_requests.fetch_sub(1, Ordering::SeqCst);
        Some(request)
    }

    fn next_task_id(&self) -> u64 {
        self.next_task_id.fetch_add(1, Ordering::SeqCst)
    }

    fn fail(&self, diagnostic: Diagnostic) {
        let mut fatal = lock_mutex(&self.fatal);
        if fatal.is_none() {
            *fatal = Some(diagnostic);
        }
        drop(fatal);
        self.begin_shutdown();
    }

    fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.notify_all_workers();
    }

    fn notify_all_workers(&self) {
        for inbox in &self.inboxes {
            if let Some(reactor) = lock_mutex(&inbox.reactor).as_ref() {
                let _ = reactor.notify_control();
            }
        }
    }

    fn fatal_diagnostic(&self) -> Option<Diagnostic> {
        lock_mutex(&self.fatal).clone()
    }

    fn mark_cleanup_complete(&self, worker_index: usize) {
        if self.inboxes[worker_index]
            .cleanup_complete
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        self.cleanup_workers.fetch_add(1, Ordering::SeqCst);
        self.notify_all_workers();
    }

    fn cleanup_is_globally_complete(&self) -> bool {
        self.cleanup_workers.load(Ordering::SeqCst) == self.inboxes.len()
            && self.pending_requests.load(Ordering::SeqCst) == 0
    }
}

impl LightweightTaskSpawnTarget {
    fn submit(&self, request: PreparedLightweightTask) {
        match self {
            Self::Local(requests) => requests.borrow_mut().push_back(request),
            Self::Workers(workers) => workers.submit(request),
        }
    }
}

impl LightweightTaskScheduler {
    #[cfg(test)]
    fn new() -> Self {
        Self {
            next_task_id: 1,
            next_wait_epoch: 1,
            spawn_requests: Rc::new(RefCell::new(VecDeque::new())),
            ready: VecDeque::new(),
            waiting: BTreeMap::new(),
            tasks: BTreeMap::new(),
            reactor: RuntimeReactor::new()
                .expect("the Aura lightweight-task reactor must initialize"),
            reactor_failure: None,
            ready_turns_since_local_reactor_poll: 0,
            ready_turns_since_io_reactor_poll: 0,
            worker_index: 0,
            workers: None,
        }
    }

    fn new_for_worker(
        worker_index: usize,
        workers: Arc<LightweightWorkerCoordinator>,
        faults: LightweightWorkerFaults,
    ) -> std::result::Result<Self, Diagnostic> {
        let reactor = faults
            .reactor_initialization_error(worker_index)
            .map_or_else(RuntimeReactor::new, Err)
            .map_err(|error| {
                Diagnostic::coded(
                    "AU4005",
                    format!("failed to initialize Aura worker {worker_index} reactor: {error}"),
                )
            })?;
        workers.register_reactor(worker_index, reactor.handle());
        Ok(Self {
            next_task_id: 1,
            next_wait_epoch: 1,
            spawn_requests: Rc::new(RefCell::new(VecDeque::new())),
            ready: VecDeque::new(),
            waiting: BTreeMap::new(),
            tasks: BTreeMap::new(),
            reactor,
            reactor_failure: None,
            ready_turns_since_local_reactor_poll: 0,
            ready_turns_since_io_reactor_poll: 0,
            worker_index,
            workers: Some(workers),
        })
    }

    #[cfg(test)]
    fn spawn_task<F>(
        &mut self,
        cancellation: Option<CancellationContext>,
        entry: F,
    ) -> std::result::Result<TaskValue, Diagnostic>
    where
        F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    {
        self.spawn_task_with_stack(cancellation, None, entry)
    }

    #[cfg(test)]
    fn spawn_task_with_stack<F>(
        &mut self,
        cancellation: Option<CancellationContext>,
        stack_size: Option<usize>,
        entry: F,
    ) -> std::result::Result<TaskValue, Diagnostic>
    where
        F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    {
        self.spawn_task_with_forced_exit_cleanup(cancellation, stack_size, entry, None)
    }

    #[cfg(test)]
    fn spawn_task_with_forced_exit_cleanup<F>(
        &mut self,
        cancellation: Option<CancellationContext>,
        stack_size: Option<usize>,
        entry: F,
        forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
    ) -> std::result::Result<TaskValue, Diagnostic>
    where
        F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    {
        let (task, request) = prepare_lightweight_task(
            cancellation,
            stack_size,
            true,
            Box::new(entry),
            forced_exit_cleanup,
        )?;
        self.admit_spawn_request(request);
        Ok(task)
    }

    fn admit_spawn_request(&mut self, request: PreparedLightweightTask) {
        let task_id = if let Some(workers) = &self.workers {
            workers.next_task_id()
        } else {
            let task_id = self.next_task_id;
            self.next_task_id += 1;
            task_id
        };
        let spawn_target = self
            .workers
            .as_ref()
            .map(|workers| LightweightTaskSpawnTarget::Workers(workers.clone()))
            .unwrap_or_else(|| LightweightTaskSpawnTarget::Local(self.spawn_requests.clone()));

        let context = Rc::new(LightweightTaskContext {
            spawn_target,
            task_id,
            worker_index: self.worker_index,
            yielder: Cell::new(std::ptr::null()),
            cancellation: request.cancellation,
            task_state: Arc::downgrade(&request.state),
        });
        let coroutine = Coroutine::with_stack(request.stack, move |yielder, _| {
            let context_was_installed = with_current_lightweight_task_context(|context| {
                context.yielder.set(yielder as *const _);
            })
            .is_some();
            debug_assert!(
                context_was_installed,
                "a lightweight coroutine must resume inside its owned task context"
            );
            finalize_task_execution(request.entry)
        });

        self.tasks.insert(
            task_id,
            LightweightTaskRecord {
                state: request.state,
                context,
                coroutine,
                forced_exit_cleanup: request.forced_exit_cleanup,
            },
        );
        self.ready
            .push_back((task_id, RuntimeSchedulerWakeReason::Ready));
    }

    fn drain_spawn_requests(&mut self) {
        loop {
            let request = self.spawn_requests.borrow_mut().pop_front();
            let Some(request) = request else {
                return;
            };
            self.admit_spawn_request(request);
        }
    }

    fn drain_worker_requests(&mut self, admit: bool) {
        let Some(workers) = self.workers.clone() else {
            return;
        };
        while let Some(request) = workers.take_request(self.worker_index) {
            if admit {
                self.admit_spawn_request(request);
            } else {
                if let Some(diagnostic) = cancel_unadmitted_lightweight_task(request) {
                    workers.fail(diagnostic);
                }
            }
        }
    }

    fn complete_task(
        &mut self,
        task_id: u64,
        task_state: &Arc<TaskState>,
        result: TaskExecutionResult,
    ) {
        self.disarm_wait(task_id);
        complete_lightweight_task_state(task_state, result);
    }

    fn resume_task(&mut self, task_id: u64, reason: RuntimeSchedulerWakeReason) {
        let Some(mut record) = self.tasks.remove(&task_id) else {
            return;
        };
        self.disarm_wait(task_id);
        record
            .state
            .waits_without_deadline
            .store(false, Ordering::SeqCst);
        *lock_mutex(&record.state.current_wait) = None;
        let _guard = enter_lightweight_task_context(&record.context);
        let outcome = record.coroutine.resume(reason);
        // A running coroutine may prepare children, but it never aliases the
        // scheduler itself. Admit those owned requests before deciding what
        // happens to the parent so a child can be waited immediately and FIFO
        // order remains child-before-parent on a yielding turn.
        self.drain_spawn_requests();
        self.drain_worker_requests(true);
        match outcome {
            CoroutineResult::Yield(TaskYield::Wait(wait)) => {
                self.tasks.insert(task_id, record);
                if let Some(reason) = wait.ready_reason(false) {
                    self.ready.push_back((task_id, reason));
                } else {
                    self.arm_wait(task_id, wait);
                }
            }
            CoroutineResult::Yield(TaskYield::YieldNow) => {
                self.tasks.insert(task_id, record);
                self.ready
                    .push_back((task_id, RuntimeSchedulerWakeReason::Ready));
            }
            CoroutineResult::Yield(TaskYield::Exit) => {
                record.context.yielder.set(std::ptr::null());
                let result = CURRENT_LIGHTWEIGHT_TASK_EXIT
                    .with(|slot| slot.borrow_mut().take())
                    .unwrap_or_else(|| {
                        TaskExecutionResult::Ready(Err(Diagnostic::new(
                            "internal error: lightweight task exited without a result",
                        )))
                    });
                if let Some(cleanup) = record.forced_exit_cleanup.take() {
                    // Direct-backend tasks suspend below generated Cranelift frames. Those
                    // frames cannot be crossed by corosensei's forced Rust unwind on all
                    // supported platforms, so their owned state is deliberately externalized
                    // into this scheduler callback before the stack is reset.
                    unsafe {
                        record.coroutine.force_reset();
                    }
                    cleanup();
                } else {
                    // Pure-Rust and MIR tasks can unwind normally, preserving Drop values on
                    // cancellation and failure.
                    record.coroutine.force_unwind();
                }
                // Forced cleanup and Rust destructors run while the task
                // context is still active and may synchronously start owned
                // child work. Admit those requests before retiring the parent.
                self.drain_spawn_requests();
                self.complete_task(task_id, &record.state, result);
            }
            CoroutineResult::Return(result) => {
                record.context.yielder.set(std::ptr::null());
                self.complete_task(task_id, &record.state, result);
            }
        }
    }

    fn arm_wait(&mut self, task_id: u64, registration: TaskWaitRegistration) {
        let key = WaitKey(task_id, self.next_wait_epoch);
        self.next_wait_epoch = self
            .next_wait_epoch
            .checked_add(1)
            .expect("Aura lightweight-task wait epoch exhausted");
        if let Err(error) = self.reactor.begin_wait(key) {
            self.record_reactor_failure("beginning a task wait", error);
            return;
        }

        let subscription = ReactorSubscription::new(key, self.reactor.handle());
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| {
            registration.subscribe_reactor(&subscription);
        })) {
            registration.unsubscribe_reactor(&subscription);
            let _ = self.reactor.cancel_wait(key);
            panic::resume_unwind(payload);
        }
        if let Some(reason) = registration.ready_reason(false) {
            #[cfg(test)]
            record_task_wait_ready_enqueue(subscription.identity());
            registration.unsubscribe_reactor(&subscription);
            let _ = self.reactor.cancel_wait(key);
            self.ready.push_back((task_id, reason));
            return;
        }

        let armed = registration
            .deadline
            .map(|deadline| self.reactor.add_deadline(key, deadline))
            .transpose()
            .and_then(|_| self.arm_fd_wait(key, registration.fd_wait));
        if let Err(error) = armed {
            registration.unsubscribe_reactor(&subscription);
            let _ = self.reactor.cancel_wait(key);
            self.record_reactor_failure("registering a task wait", error);
            return;
        }

        let waits_without_deadline = registration.deadline.is_none();
        let current_wait = registration.reachability_snapshot();
        self.waiting.insert(
            task_id,
            LightweightTaskWait {
                key,
                registration,
                subscription,
            },
        );
        if let Some(record) = self.tasks.get(&task_id) {
            record
                .state
                .waits_without_deadline
                .store(waits_without_deadline, Ordering::SeqCst);
            *lock_mutex(&record.state.current_wait) = Some(current_wait);
        }
    }

    fn arm_fd_wait(&mut self, key: WaitKey, fd_wait: Option<FdWaitRegistration>) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(fd_wait) = fd_wait {
            let mut interest = None;
            if fd_wait.events & (libc::POLLIN | libc::POLLPRI) != 0 {
                interest = Some(IoInterest::READABLE);
            }
            if fd_wait.events & libc::POLLOUT != 0 {
                interest = Some(
                    interest
                        .map(|current| current | IoInterest::WRITABLE)
                        .unwrap_or(IoInterest::WRITABLE),
                );
            }
            let interest = interest.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Aura runtime descriptor wait has no supported interest",
                )
            })?;
            self.reactor.add_fd(key, fd_wait.fd, interest)?;
        }
        #[cfg(not(unix))]
        let _ = (key, fd_wait);
        Ok(())
    }

    fn disarm_wait(&mut self, task_id: u64) -> Option<TaskWaitRegistration> {
        self.take_wait_registration(task_id, true)
    }

    fn take_ready_wait(&mut self, task_id: u64) -> Option<TaskWaitRegistration> {
        self.take_wait_registration(task_id, false)
    }

    fn take_wait_registration(
        &mut self,
        task_id: u64,
        cancel_reactor_wait: bool,
    ) -> Option<TaskWaitRegistration> {
        let waiting = self.waiting.remove(&task_id)?;
        if let Some(record) = self.tasks.get(&task_id) {
            record
                .state
                .waits_without_deadline
                .store(false, Ordering::SeqCst);
            *lock_mutex(&record.state.current_wait) = None;
        }
        waiting
            .registration
            .unsubscribe_reactor(&waiting.subscription);
        if cancel_reactor_wait {
            if let Err(error) = self.reactor.cancel_wait(waiting.key) {
                self.record_reactor_failure("retiring a task wait", error);
            }
        }
        Some(waiting.registration)
    }

    fn record_reactor_failure(&mut self, operation: &str, error: io::Error) {
        if self.reactor_failure.is_none() {
            self.reactor_failure = Some(reactor_failure_diagnostic(operation, error));
        }
    }

    fn admit_reactor_keys(&mut self, keys: Vec<WaitKey>) {
        for key in keys {
            let Some(current) = self.waiting.get(&key.0) else {
                continue;
            };
            if current.key != key {
                continue;
            }
            #[cfg(test)]
            let subscription_identity = current.subscription.identity();
            let registration = self
                .take_ready_wait(key.0)
                .expect("the matching reactor wait remains registered");
            if let Some(reason) = registration.ready_reason(registration.fd_wait.is_some()) {
                #[cfg(test)]
                record_task_wait_ready_enqueue(subscription_identity);
                self.ready.push_back((key.0, reason));
            } else {
                self.arm_wait(key.0, registration);
            }
        }
    }

    fn admit_reactor_events_nonblocking(&mut self) -> io::Result<()> {
        let keys = self.reactor.poll_nonblocking()?;
        self.admit_reactor_keys(keys);
        Ok(())
    }

    fn admit_local_reactor_events_nonblocking(&mut self) -> io::Result<()> {
        let keys = self.reactor.poll_local_nonblocking()?;
        self.admit_reactor_keys(keys);
        Ok(())
    }

    #[cfg(test)]
    fn wait_for_external_events(&mut self) -> io::Result<()> {
        let keys = self.reactor.poll(None)?;
        self.admit_reactor_keys(keys);
        Ok(())
    }

    fn run_worker(&mut self, root: &TaskValue) {
        let workers = self
            .workers
            .clone()
            .expect("a worker scheduler must have a coordinator");
        loop {
            self.drain_worker_requests(true);
            if let Some(diagnostic) = self.reactor_failure.take() {
                workers.fail(diagnostic);
            }
            if workers.fatal_diagnostic().is_some() {
                workers.begin_shutdown();
            }
            if root.completed_result().is_some() {
                workers.begin_shutdown();
            }
            if workers.shutdown.load(Ordering::SeqCst) {
                break;
            }

            if !self.ready.is_empty() {
                self.ready_turns_since_local_reactor_poll += 1;
                self.ready_turns_since_io_reactor_poll += 1;
                let poll_result =
                    if self.ready_turns_since_io_reactor_poll >= READY_TURNS_PER_IO_REACTOR_POLL {
                        self.ready_turns_since_local_reactor_poll = 0;
                        self.ready_turns_since_io_reactor_poll = 0;
                        self.admit_reactor_events_nonblocking()
                    } else if self.ready_turns_since_local_reactor_poll
                        >= READY_TURNS_PER_LOCAL_REACTOR_POLL
                    {
                        self.ready_turns_since_local_reactor_poll = 0;
                        self.admit_local_reactor_events_nonblocking()
                    } else {
                        Ok(())
                    };
                if let Err(error) = poll_result {
                    workers.fail(reactor_failure_diagnostic("admitting ready events", error));
                    continue;
                }
                if let Some(diagnostic) = self.reactor_failure.take() {
                    workers.fail(diagnostic);
                    continue;
                }
            }

            if let Some((task_id, reason)) = self.ready.pop_front() {
                self.resume_task(task_id, reason);
                continue;
            }

            self.ready_turns_since_local_reactor_poll = 0;
            self.ready_turns_since_io_reactor_poll = 0;
            match self.reactor.poll(None) {
                Ok(keys) => self.admit_reactor_keys(keys),
                Err(error) => workers.fail(reactor_failure_diagnostic("waiting", error)),
            }
        }

        self.retire_all_tasks();
        workers.mark_cleanup_complete(self.worker_index);
        loop {
            self.drain_worker_requests(false);
            if workers.cleanup_is_globally_complete() {
                workers.notify_all_workers();
                break;
            }
            let _ = self.reactor.poll(Some(StdDuration::from_millis(10)));
        }
    }

    fn retire_all_tasks(&mut self) {
        let task_ids: Vec<_> = self.waiting.keys().copied().collect();
        for task_id in task_ids {
            self.disarm_wait(task_id);
        }
        self.drain_spawn_requests();
        while let Some((task_id, record)) = self.tasks.pop_first() {
            self.cancel_task_record(task_id, record);
            self.drain_spawn_requests();
        }
    }

    fn cancel_task_record(&mut self, task_id: u64, mut record: LightweightTaskRecord) {
        record
            .state
            .waits_without_deadline
            .store(false, Ordering::SeqCst);
        *lock_mutex(&record.state.current_wait) = None;
        *lock_mutex(&record.state.join_dependencies) = None;
        let _guard = enter_lightweight_task_context(&record.context);
        record.context.yielder.set(std::ptr::null());
        if let Some(cleanup) = record.forced_exit_cleanup.take() {
            if record.coroutine.started() {
                // Direct-backend frames cannot be crossed by a Rust unwind.
                unsafe {
                    record.coroutine.force_reset();
                }
            } else {
                // Before its first resume the coroutine contains only the
                // owned initial closure, so release those captures normally.
                record.coroutine.force_unwind();
            }
            cleanup();
        } else {
            record.coroutine.force_unwind();
        }
        self.drain_spawn_requests();
        self.complete_task(task_id, &record.state, TaskExecutionResult::Cancelled);
    }
}

impl Drop for LightweightTaskScheduler {
    fn drop(&mut self) {
        // Root completion and reactor failures may leave structured children
        // suspended. Retire every admitted or prepared request explicitly so
        // no task handle stays Running and direct-task external state is never
        // abandoned.
        self.retire_all_tasks();
    }
}

fn enqueue_lightweight_task<R>(
    cancellation: Option<CancellationContext>,
    stack_size: Option<usize>,
    result_is_repeatable: bool,
    entry: LightweightTaskEntry,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    R: FnOnce(&TaskValue),
{
    let Some(spawn_target) =
        with_current_lightweight_task_context(|context| context.spawn_target.clone())
    else {
        return Err(Diagnostic::new(
            "lightweight Aura task start requires an active task scheduler",
        ));
    };
    let (task, request) = prepare_lightweight_task(
        cancellation,
        stack_size,
        result_is_repeatable,
        entry,
        forced_exit_cleanup,
    )?;
    register_before_submit(&task);
    spawn_target.submit(request);
    Ok(task)
}

pub(crate) fn spawn_lightweight_task<F>(entry: F) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    enqueue_lightweight_task(None, None, true, Box::new(entry), None, |_| {})
}

#[cfg(test)]
pub(crate) fn spawn_lightweight_task_with_result_repeatability<F>(
    result_is_repeatable: bool,
    entry: F,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    enqueue_lightweight_task(
        None,
        None,
        result_is_repeatable,
        Box::new(entry),
        None,
        |_| {},
    )
}

pub(crate) fn spawn_lightweight_task_with_result_repeatability_registered<F, R>(
    result_is_repeatable: bool,
    entry: F,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    R: FnOnce(&TaskValue),
{
    enqueue_lightweight_task(
        None,
        None,
        result_is_repeatable,
        Box::new(entry),
        None,
        register_before_submit,
    )
}

#[cfg(test)]
pub(crate) fn spawn_lightweight_task_with_stack<F>(
    stack_size: usize,
    entry: F,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    enqueue_lightweight_task(None, Some(stack_size), true, Box::new(entry), None, |_| {})
}

#[cfg(test)]
pub(crate) fn spawn_lightweight_task_with_stack_and_result_repeatability<F>(
    stack_size: usize,
    result_is_repeatable: bool,
    entry: F,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    enqueue_lightweight_task(
        None,
        Some(stack_size),
        result_is_repeatable,
        Box::new(entry),
        None,
        |_| {},
    )
}

pub(crate) fn spawn_lightweight_task_with_stack_and_result_repeatability_registered<F, R>(
    stack_size: usize,
    result_is_repeatable: bool,
    entry: F,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    R: FnOnce(&TaskValue),
{
    enqueue_lightweight_task(
        None,
        Some(stack_size),
        result_is_repeatable,
        Box::new(entry),
        None,
        register_before_submit,
    )
}

fn notify_group_failure_wake_flags(task_state: &TaskState, result: &TaskExecutionResult) {
    if !matches!(result, TaskExecutionResult::Ready(Err(_))) {
        return;
    }
    let flags = lock_mutex(&task_state.group_failure_wake_flags).clone();
    if flags.is_empty() {
        return;
    }
    for flag in flags {
        flag.store(true, Ordering::SeqCst);
    }
    notify_runtime_scheduler_if_started();
}

fn notify_group_completion_wake_flags(task_state: &TaskState) {
    let flags = lock_mutex(&task_state.group_completion_wake_flags).clone();
    if flags.is_empty() {
        return;
    }
    for flag in flags {
        flag.store(true, Ordering::SeqCst);
    }
    notify_runtime_scheduler_if_started();
}

pub(crate) fn spawn_lightweight_task_with_cancellation<F>(
    cancellation: CancellationContext,
    entry: F,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    enqueue_lightweight_task(
        Some(cancellation),
        None,
        true,
        Box::new(entry),
        None,
        |_| {},
    )
}

/// Starts a task whose generated stack must be reset instead of force-unwound.
///
/// # Safety
///
/// On a forced exit, every resource that cannot safely be abandoned with the
/// coroutine stack must be owned and released by `forced_exit_cleanup`. The
/// callback must not panic: scheduler teardown relies on it completing so the
/// remaining task records can also be retired.
#[cfg(test)]
pub(crate) unsafe fn spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup<F, C>(
    cancellation: CancellationContext,
    entry: F,
    forced_exit_cleanup: C,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    C: FnOnce() + Send + 'static,
{
    unsafe {
        spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack(
            cancellation,
            None,
            entry,
            forced_exit_cleanup,
        )
    }
}

/// Starts a generated task with an optional explicit stack size.
///
/// # Safety
///
/// This has the same forced-exit ownership requirements as
/// `spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup`.
#[cfg(test)]
pub(crate) unsafe fn spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack<
    F,
    C,
>(
    cancellation: CancellationContext,
    stack_size: Option<usize>,
    entry: F,
    forced_exit_cleanup: C,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    C: FnOnce() + Send + 'static,
{
    enqueue_lightweight_task(
        Some(cancellation),
        stack_size,
        true,
        Box::new(entry),
        Some(Box::new(forced_exit_cleanup)),
        |_| {},
    )
}

/// Starts a generated task whose result-observation repeatability is known by
/// the compiler and whose generated stack must be reset on forced exit.
///
/// # Safety
///
/// This has the same forced-exit ownership requirements as
/// `spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup`.
#[cfg(test)]
pub(crate) unsafe fn spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack_and_result_repeatability<
    F,
    C,
>(
    cancellation: CancellationContext,
    stack_size: Option<usize>,
    result_is_repeatable: bool,
    entry: F,
    forced_exit_cleanup: C,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    C: FnOnce() + Send + 'static,
{
    enqueue_lightweight_task(
        Some(cancellation),
        stack_size,
        result_is_repeatable,
        Box::new(entry),
        Some(Box::new(forced_exit_cleanup)),
        |_| {},
    )
}

pub(crate) unsafe fn spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack_and_result_repeatability_registered<
    F,
    C,
    R,
>(
    cancellation: CancellationContext,
    stack_size: Option<usize>,
    result_is_repeatable: bool,
    entry: F,
    forced_exit_cleanup: C,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    C: FnOnce() + Send + 'static,
    R: FnOnce(&TaskValue),
{
    enqueue_lightweight_task(
        Some(cancellation),
        stack_size,
        result_is_repeatable,
        Box::new(entry),
        Some(Box::new(forced_exit_cleanup)),
        register_before_submit,
    )
}

fn run_lightweight_root_task_on_workers_with_faults(
    worker_count: usize,
    entry: LightweightTaskEntry,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
    faults: LightweightWorkerFaults,
) -> std::result::Result<Value, Diagnostic> {
    let workers = LightweightWorkerCoordinator::new(worker_count);
    let (root, request) = prepare_lightweight_task(None, None, true, entry, forced_exit_cleanup)?;
    workers.submit(request);

    let mut threads = Vec::with_capacity(worker_count);
    let mut failed_spawn_at = None;
    for worker_index in 0..worker_count {
        let workers_for_thread = workers.clone();
        let root_for_thread = root.clone();
        let builder = thread::Builder::new().name(format!("aura-worker-{worker_index}"));
        let spawn_result = if let Some(error) = faults.thread_spawn_error(worker_index) {
            Err(error)
        } else {
            builder.spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    if faults.should_panic_worker(worker_index) {
                        panic!("injected Aura worker panic");
                    }
                    match LightweightTaskScheduler::new_for_worker(
                        worker_index,
                        workers_for_thread.clone(),
                        faults,
                    ) {
                        Ok(mut scheduler) => scheduler.run_worker(&root_for_thread),
                        Err(error) => {
                            workers_for_thread.fail(error);
                            while let Some(request) = workers_for_thread.take_request(worker_index)
                            {
                                if let Some(diagnostic) =
                                    cancel_unadmitted_lightweight_task(request)
                                {
                                    workers_for_thread.fail(diagnostic);
                                }
                            }
                            workers_for_thread.mark_cleanup_complete(worker_index);
                            while !workers_for_thread.cleanup_is_globally_complete() {
                                while let Some(request) =
                                    workers_for_thread.take_request(worker_index)
                                {
                                    if let Some(diagnostic) =
                                        cancel_unadmitted_lightweight_task(request)
                                    {
                                        workers_for_thread.fail(diagnostic);
                                    }
                                }
                                thread::yield_now();
                            }
                        }
                    }
                }));
                if let Err(payload) = outcome {
                    workers_for_thread.fail(Diagnostic::new(format!(
                        "internal error: Aura worker {worker_index} panicked: {}",
                        task_panic_message(&*payload)
                    )));
                    while let Some(request) = workers_for_thread.take_request(worker_index) {
                        if let Some(diagnostic) = cancel_unadmitted_lightweight_task(request) {
                            workers_for_thread.fail(diagnostic);
                        }
                    }
                    workers_for_thread.mark_cleanup_complete(worker_index);
                    while !workers_for_thread.cleanup_is_globally_complete() {
                        while let Some(request) = workers_for_thread.take_request(worker_index) {
                            if let Some(diagnostic) = cancel_unadmitted_lightweight_task(request) {
                                workers_for_thread.fail(diagnostic);
                            }
                        }
                        thread::yield_now();
                    }
                }
            })
        };
        match spawn_result {
            Ok(worker) => threads.push(worker),
            Err(error) => {
                workers.fail(Diagnostic::coded(
                    "AU4005",
                    format!("failed to start Aura worker {worker_index}: {error}"),
                ));
                failed_spawn_at = Some(worker_index);
                break;
            }
        }
    }

    if let Some(first_missing) = failed_spawn_at {
        for worker_index in first_missing..worker_count {
            workers.mark_cleanup_complete(worker_index);
        }
        while !workers.cleanup_is_globally_complete() {
            for worker_index in first_missing..worker_count {
                while let Some(request) = workers.take_request(worker_index) {
                    if let Some(diagnostic) = cancel_unadmitted_lightweight_task(request) {
                        workers.fail(diagnostic);
                    }
                }
            }
            thread::yield_now();
        }
    }

    for worker in threads {
        if let Err(payload) = worker.join() {
            workers.fail(Diagnostic::new(format!(
                "internal error: Aura worker thread panicked: {}",
                task_panic_message(&*payload)
            )));
        }
    }

    if let Some(diagnostic) = workers.fatal_diagnostic() {
        return Err(diagnostic);
    }
    match root.completed_result() {
        Some(TaskExecutionResult::Ready(result)) => result,
        Some(TaskExecutionResult::Cancelled) => {
            Err(Diagnostic::new("root Aura task was cancelled"))
        }
        None => Err(Diagnostic::new(
            "internal error: Aura workers stopped before the root task completed",
        )),
    }
}

fn run_lightweight_root_task_on_workers(
    worker_count: usize,
    entry: LightweightTaskEntry,
    forced_exit_cleanup: Option<Box<dyn FnOnce() + Send>>,
) -> std::result::Result<Value, Diagnostic> {
    run_lightweight_root_task_on_workers_with_faults(
        worker_count,
        entry,
        forced_exit_cleanup,
        LightweightWorkerFaults::default(),
    )
}

#[cfg(test)]
pub(crate) fn run_lightweight_root_task_with_worker_count<F>(
    worker_count: usize,
    entry: F,
) -> std::result::Result<Value, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    install_lightweight_task_panic_hook();
    if worker_count == 0 {
        return Err(Diagnostic::coded(
            "AU4006",
            "Aura runtime requires at least one worker",
        ));
    }
    run_lightweight_root_task_on_workers(worker_count, Box::new(entry), None)
}

pub(crate) fn run_lightweight_root_task<F>(entry: F) -> std::result::Result<Value, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
{
    crate::runtime_config::validate_runtime_configuration()?;
    install_lightweight_task_panic_hook();
    run_lightweight_root_task_on_workers(lightweight_worker_count()?, Box::new(entry), None)
}

/// Runs a generated root task whose stack must be reset on forced exit.
///
/// The cleanup is deliberately not a language cleanup thunk: it may only
/// discard externalized direct-runtime state and release host-owned state.
///
/// # Safety
///
/// On a forced exit, every resource that cannot safely be abandoned with the
/// coroutine stack must be owned and released by `forced_exit_cleanup`. The
/// callback must not panic: scheduler teardown relies on it completing so the
/// remaining task records can also be retired.
pub(crate) unsafe fn run_lightweight_root_task_with_forced_exit_cleanup<F, C>(
    entry: F,
    forced_exit_cleanup: C,
) -> std::result::Result<Value, Diagnostic>
where
    F: FnOnce() -> std::result::Result<Value, Diagnostic> + Send + 'static,
    C: FnOnce() + Send + 'static,
{
    crate::runtime_config::validate_runtime_configuration()?;
    install_lightweight_task_panic_hook();
    run_lightweight_root_task_on_workers(
        lightweight_worker_count()?,
        Box::new(entry),
        Some(Box::new(forced_exit_cleanup)),
    )
}

impl PartialEq for VecValue {
    fn eq(&self, other: &Self) -> bool {
        self.elements == other.elements
    }
}

impl PartialEq for SetValue {
    fn eq(&self, other: &Self) -> bool {
        if self.elements.len() != other.elements.len() {
            return false;
        }
        for element in &self.elements {
            let mut found = false;
            for candidate in &other.elements {
                if candidate == element {
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
        true
    }
}

impl PartialEq for MapValue {
    fn eq(&self, other: &Self) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        for (key, value) in &self.entries {
            let mut matched = false;
            for (candidate_key, candidate_value) in &other.entries {
                if candidate_key == key {
                    matched = candidate_value == value;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
        true
    }
}

fn clone_json_runtime_tree(root: &Value) -> Option<Value> {
    enum CloneFrame<'a> {
        Array {
            variant: &'a EnumVariantValue,
            vector: &'a VecValue,
            next_index: usize,
            elements: Vec<Value>,
        },
        Object {
            variant: &'a EnumVariantValue,
            map: &'a MapValue,
            next_index: usize,
            entries: Vec<(Value, Value)>,
            pending_key: Value,
        },
    }

    fn enum_value(variant: &EnumVariantValue, payloads: Vec<Value>) -> Value {
        Value::EnumVariant(EnumVariantValue {
            enum_name: variant.enum_name.clone(),
            variant_name: variant.variant_name.clone(),
            payloads,
        })
    }

    let mut frames = Vec::new();
    let mut next = Some(root);
    let mut completed = None;
    loop {
        if let Some(value) = next.take() {
            let Value::EnumVariant(variant) = value else {
                return None;
            };
            if nominal_runtime_base_name(&variant.enum_name) != "json.Value" {
                return None;
            }
            completed = match (variant.variant_name.as_str(), variant.payloads.as_slice()) {
                ("Null", []) => Some(enum_value(variant, Vec::new())),
                ("Bool", [Value::Bool(value)]) => {
                    Some(enum_value(variant, vec![Value::Bool(*value)]))
                }
                ("Int", [Value::Int(value)]) => Some(enum_value(variant, vec![Value::Int(*value)])),
                ("Float", [Value::Float(value)]) => {
                    Some(enum_value(variant, vec![Value::Float(*value)]))
                }
                ("String", [Value::String(value)]) => {
                    Some(enum_value(variant, vec![Value::String(value.clone())]))
                }
                ("Array", [Value::Vec(vector)]) => {
                    let elements = Vec::with_capacity(vector.elements.len());
                    if vector.elements.is_empty() {
                        Some(enum_value(
                            variant,
                            vec![Value::Vec(VecValue {
                                element_type: vector.element_type.clone(),
                                elements,
                            })],
                        ))
                    } else {
                        frames.push(CloneFrame::Array {
                            variant,
                            vector,
                            next_index: 1,
                            elements,
                        });
                        next = Some(&vector.elements[0]);
                        None
                    }
                }
                ("Object", [Value::Map(map)]) => {
                    let entries = Vec::with_capacity(map.entries.len());
                    if map.entries.is_empty() {
                        Some(enum_value(
                            variant,
                            vec![Value::Map(MapValue {
                                key_type: map.key_type.clone(),
                                value_type: map.value_type.clone(),
                                entries,
                            })],
                        ))
                    } else {
                        let (Value::String(key), value) = &map.entries[0] else {
                            return None;
                        };
                        frames.push(CloneFrame::Object {
                            variant,
                            map,
                            next_index: 1,
                            entries,
                            pending_key: Value::String(key.clone()),
                        });
                        next = Some(value);
                        None
                    }
                }
                _ => return None,
            };
        } else {
            let frame = frames.pop()?;
            match frame {
                CloneFrame::Array {
                    variant,
                    vector,
                    mut next_index,
                    mut elements,
                } => {
                    elements.push(completed.take()?);
                    if let Some(value) = vector.elements.get(next_index) {
                        next_index += 1;
                        frames.push(CloneFrame::Array {
                            variant,
                            vector,
                            next_index,
                            elements,
                        });
                        next = Some(value);
                    } else {
                        completed = Some(enum_value(
                            variant,
                            vec![Value::Vec(VecValue {
                                element_type: vector.element_type.clone(),
                                elements,
                            })],
                        ));
                    }
                }
                CloneFrame::Object {
                    variant,
                    map,
                    mut next_index,
                    mut entries,
                    pending_key,
                } => {
                    entries.push((pending_key, completed.take()?));
                    if let Some((key, value)) = map.entries.get(next_index) {
                        let Value::String(key) = key else {
                            return None;
                        };
                        next_index += 1;
                        frames.push(CloneFrame::Object {
                            variant,
                            map,
                            next_index,
                            entries,
                            pending_key: Value::String(key.clone()),
                        });
                        next = Some(value);
                    } else {
                        completed = Some(enum_value(
                            variant,
                            vec![Value::Map(MapValue {
                                key_type: map.key_type.clone(),
                                value_type: map.value_type.clone(),
                                entries,
                            })],
                        ));
                    }
                }
            }
        }

        if next.is_none() && frames.is_empty() {
            return completed;
        }
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        if matches!(
            self,
            Value::EnumVariant(variant)
                if nominal_runtime_base_name(&variant.enum_name) == "json.Value"
        ) {
            if let Some(cloned) = clone_json_runtime_tree(self) {
                return cloned;
            }
        }

        match self {
            Self::Int(value) => Self::Int(*value),
            Self::Float(value) => Self::Float(*value),
            Self::Bool(value) => Self::Bool(*value),
            Self::String(value) => Self::String(value.clone()),
            Self::Tuple(value) => Self::Tuple(value.clone()),
            Self::Vec(value) => Self::Vec(value.clone()),
            Self::Array(value) => Self::Array(value.clone()),
            Self::Set(value) => Self::Set(value.clone()),
            Self::Map(value) => Self::Map(value.clone()),
            Self::Duration(value) => Self::Duration(*value),
            Self::Rng(value) => Self::Rng(value.clone()),
            Self::Range(value) => Self::Range(value.clone()),
            Self::ModuleNamespace(value) => Self::ModuleNamespace(value.clone()),
            Self::Function(value) => Self::Function(value.clone()),
            Self::Unit => Self::Unit,
            Self::FfiHandle(value) => Self::FfiHandle(value.clone()),
            Self::Instance(value) => Self::Instance(value.clone()),
            Self::EnumVariant(value) => Self::EnumVariant(value.clone()),
            Self::Channel(value) => Self::Channel(value.clone()),
            Self::Task(value) => Self::Task(value.clone()),
            Self::TaskGroup(value) => Self::TaskGroup(value.clone()),
            Self::File(value) => Self::File(value.clone()),
            Self::TcpListener(value) => Self::TcpListener(value.clone()),
            Self::TcpStream(value) => Self::TcpStream(value.clone()),
            Self::UdpSocket(value) => Self::UdpSocket(value.clone()),
            Self::UdpDatagram(value) => Self::UdpDatagram(value.clone()),
            Self::HttpListener(value) => Self::HttpListener(value.clone()),
            Self::HttpExchange(value) => Self::HttpExchange(value.clone()),
            Self::HttpResponse(value) => Self::HttpResponse(value.clone()),
            Self::WebSocketListener(value) => Self::WebSocketListener(value.clone()),
            Self::WebSocket(value) => Self::WebSocket(value.clone()),
            Self::ProcessChild(value) => Self::ProcessChild(value.clone()),
            Self::ProcessPipe(value) => Self::ProcessPipe(value.clone()),
            Self::ProcessCompleted(value) => Self::ProcessCompleted(value.clone()),
            Self::ProcessSupervisor(value) => Self::ProcessSupervisor(value.clone()),
            Self::UnixListener(value) => Self::UnixListener(value.clone()),
            Self::UnixStream(value) => Self::UnixStream(value.clone()),
            Self::TlsListener(value) => Self::TlsListener(value.clone()),
            Self::TlsStream(value) => Self::TlsStream(value.clone()),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(left), Value::Int(right)) => left == right,
            (Value::Duration(left), Value::Duration(right)) => left == right,
            (Value::Float(left), Value::Float(right)) => left == right,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::String(left), Value::String(right)) => left == right,
            (Value::Tuple(left), Value::Tuple(right)) => left == right,
            (Value::Vec(left), Value::Vec(right)) => left == right,
            (Value::Array(left), Value::Array(right)) => left == right,
            (Value::Set(left), Value::Set(right)) => left == right,
            (Value::Map(left), Value::Map(right)) => left == right,
            (Value::Rng(left), Value::Rng(right)) => left == right,
            (Value::Range(left), Value::Range(right)) => left == right,
            (Value::ModuleNamespace(left), Value::ModuleNamespace(right)) => left == right,
            (Value::Function(left), Value::Function(right)) => left == right,
            (Value::Unit, Value::Unit) => true,
            (Value::Instance(left), Value::Instance(right)) => left == right,
            (Value::EnumVariant(left), Value::EnumVariant(right)) => left == right,
            (Value::Channel(left), Value::Channel(right)) => left == right,
            (Value::Task(left), Value::Task(right)) => left == right,
            (Value::TaskGroup(left), Value::TaskGroup(right)) => left == right,
            (Value::File(left), Value::File(right)) => left == right,
            (Value::TcpListener(left), Value::TcpListener(right)) => left == right,
            (Value::TcpStream(left), Value::TcpStream(right)) => left == right,
            (Value::UdpSocket(left), Value::UdpSocket(right)) => left == right,
            (Value::UdpDatagram(left), Value::UdpDatagram(right)) => left == right,
            (Value::HttpListener(left), Value::HttpListener(right)) => left == right,
            (Value::HttpExchange(left), Value::HttpExchange(right)) => left == right,
            (Value::HttpResponse(left), Value::HttpResponse(right)) => left == right,
            (Value::WebSocketListener(left), Value::WebSocketListener(right)) => left == right,
            (Value::WebSocket(left), Value::WebSocket(right)) => left == right,
            (Value::ProcessChild(left), Value::ProcessChild(right)) => left == right,
            (Value::ProcessPipe(left), Value::ProcessPipe(right)) => left == right,
            (Value::ProcessCompleted(left), Value::ProcessCompleted(right)) => left == right,
            (Value::ProcessSupervisor(left), Value::ProcessSupervisor(right)) => left == right,
            (Value::UnixListener(left), Value::UnixListener(right)) => left == right,
            (Value::UnixStream(left), Value::UnixStream(right)) => left == right,
            (Value::TlsListener(left), Value::TlsListener(right)) => left == right,
            (Value::TlsStream(left), Value::TlsStream(right)) => left == right,
            _ => false,
        }
    }
}

impl Value {
    pub fn render(&self) -> String {
        enum RenderAction<'a> {
            Value(&'a Value),
            Text(&'a str),
            Static(&'static str),
        }

        let mut rendered = String::new();
        let mut actions = vec![RenderAction::Value(self)];
        while let Some(action) = actions.pop() {
            match action {
                RenderAction::Text(text) | RenderAction::Static(text) => {
                    rendered.push_str(text);
                }
                RenderAction::Value(value) => match value {
                    Value::Int(value) => rendered.push_str(&value.to_string()),
                    Value::Float(value) => rendered.push_str(&render_float(*value)),
                    Value::Bool(value) => rendered.push_str(if *value { "true" } else { "false" }),
                    Value::String(value) => rendered.push_str(value),
                    Value::Tuple(tuple) => {
                        rendered.push('(');
                        actions.push(RenderAction::Static(")"));
                        if tuple.elements.len() == 1 {
                            actions.push(RenderAction::Static(","));
                        }
                        for (index, value) in tuple.elements.iter().enumerate().rev() {
                            actions.push(RenderAction::Value(value));
                            if index > 0 {
                                actions.push(RenderAction::Static(", "));
                            }
                        }
                    }
                    Value::Vec(values) => {
                        rendered.push('[');
                        actions.push(RenderAction::Static("]"));
                        for (index, value) in values.elements.iter().enumerate().rev() {
                            actions.push(RenderAction::Value(value));
                            if index > 0 {
                                actions.push(RenderAction::Static(", "));
                            }
                        }
                    }
                    Value::Array(array) => rendered.push_str(&array.render()),
                    Value::Set(values) => {
                        if values.elements.is_empty() {
                            rendered.push_str("set()");
                            continue;
                        }
                        rendered.push('{');
                        actions.push(RenderAction::Static("}"));
                        for (index, value) in values.elements.iter().enumerate().rev() {
                            actions.push(RenderAction::Value(value));
                            if index > 0 {
                                actions.push(RenderAction::Static(", "));
                            }
                        }
                    }
                    Value::Map(map) => {
                        rendered.push('{');
                        actions.push(RenderAction::Static("}"));
                        for (index, (key, value)) in map.entries.iter().enumerate().rev() {
                            actions.push(RenderAction::Value(value));
                            actions.push(RenderAction::Static(": "));
                            actions.push(RenderAction::Value(key));
                            if index > 0 {
                                actions.push(RenderAction::Static(", "));
                            }
                        }
                    }
                    Value::Duration(value) => rendered.push_str(&render_duration(*value)),
                    Value::Rng(_) => rendered.push_str("<rng>"),
                    Value::Range(range) => {
                        rendered.push_str(&format!("range({}, {})", range.start, range.end));
                    }
                    Value::ModuleNamespace(namespace) => {
                        rendered.push_str(&format!("<module {}>", namespace.path));
                    }
                    Value::Function(function) => {
                        rendered.push_str(&format!("<function {}>", function.name));
                    }
                    Value::Unit => {}
                    Value::FfiHandle(handle) => {
                        rendered.push_str(&format!("<opaque {}>", handle.type_name()));
                    }
                    Value::Channel(_) => rendered.push_str("<queue>"),
                    Value::Task(_) => rendered.push_str("<task>"),
                    Value::TaskGroup(_) => rendered.push_str("<tasks>"),
                    Value::File(_) => rendered.push_str("<file>"),
                    Value::TcpListener(_) => rendered.push_str("<tcp-listener>"),
                    Value::TcpStream(_) => rendered.push_str("<tcp-stream>"),
                    Value::UdpSocket(_) => rendered.push_str("<udp-socket>"),
                    Value::UdpDatagram(datagram) => rendered.push_str(&format!(
                        "<udp-datagram {} {} bytes>",
                        datagram.address,
                        datagram.data.len()
                    )),
                    Value::HttpListener(_) => rendered.push_str("<http-listener>"),
                    Value::HttpExchange(_) => rendered.push_str("<http-exchange>"),
                    Value::HttpResponse(response) => rendered.push_str(&format!(
                        "<http-response {} {} bytes>",
                        response.status,
                        response.body.len()
                    )),
                    Value::WebSocketListener(_) => rendered.push_str("<websocket-listener>"),
                    Value::WebSocket(_) => rendered.push_str("<websocket>"),
                    Value::ProcessChild(_) => rendered.push_str("<process-child>"),
                    Value::ProcessPipe(_) => rendered.push_str("<process-pipe>"),
                    Value::ProcessCompleted(completed) => rendered.push_str(&format!(
                        "<process-completed {}>",
                        completed.status().render()
                    )),
                    Value::ProcessSupervisor(_) => rendered.push_str("<process-supervisor>"),
                    Value::UnixListener(_) => rendered.push_str("<unix-listener>"),
                    Value::UnixStream(_) => rendered.push_str("<unix-stream>"),
                    Value::TlsListener(_) => rendered.push_str("<tls-listener>"),
                    Value::TlsStream(_) => rendered.push_str("<tls-stream>"),
                    Value::Instance(instance) => {
                        rendered.push_str(nominal_runtime_base_name(&instance.class_name));
                        rendered.push('(');
                        actions.push(RenderAction::Static(")"));
                        let fields = instance
                            .fields
                            .iter()
                            .filter(|(name, _)| name.as_str() != DIRECT_RUNTIME_TYPE_FIELD)
                            .collect::<Vec<_>>();
                        for (index, (name, value)) in fields.into_iter().enumerate().rev() {
                            actions.push(RenderAction::Value(value));
                            actions.push(RenderAction::Static("="));
                            actions.push(RenderAction::Text(name));
                            if index > 0 {
                                actions.push(RenderAction::Static(", "));
                            }
                        }
                    }
                    Value::EnumVariant(variant) => {
                        rendered.push_str(nominal_runtime_base_name(&variant.enum_name));
                        rendered.push('.');
                        rendered.push_str(&variant.variant_name);
                        if !variant.payloads.is_empty() {
                            rendered.push('(');
                            actions.push(RenderAction::Static(")"));
                            for (index, payload) in variant.payloads.iter().enumerate().rev() {
                                actions.push(RenderAction::Value(payload));
                                if index > 0 {
                                    actions.push(RenderAction::Static(", "));
                                }
                            }
                        }
                    }
                },
            }
        }
        rendered
    }
}

pub(crate) fn render_float(value: f64) -> String {
    format!("{value:?}")
}

pub(crate) fn render_float32(value: f32) -> String {
    format!("{value:?}")
}

pub(crate) const MAX_FORMAT_COMPONENT: usize = 1_000_000;
pub(crate) const MAX_STRING_BYTES: usize = 64 * 1024 * 1024;

fn append_string_with_limit(output: &mut String, text: &str, limit: usize) -> Result<()> {
    let required = output
        .len()
        .checked_add(text.len())
        .ok_or_else(|| Diagnostic::coded("AU4005", "string allocation size overflow"))?;
    if required > limit {
        return Err(Diagnostic::coded(
            "AU4005",
            format!("string output exceeds the maintained {limit}-byte allocation limit"),
        ));
    }
    output
        .try_reserve(text.len())
        .map_err(|_| Diagnostic::coded("AU4005", "string allocation failed"))?;
    output.push_str(text);
    Ok(())
}

pub(crate) fn append_string_checked(output: &mut String, text: &str) -> Result<()> {
    append_string_with_limit(output, text, MAX_STRING_BYTES)
}

pub(crate) fn concat_strings_checked(mut left: String, right: &str) -> Result<String> {
    append_string_checked(&mut left, right)?;
    Ok(left)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FormatSpecErrorKind {
    Syntax,
    Type,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FormatSpecError {
    pub(crate) kind: FormatSpecErrorKind,
    pub(crate) message: String,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FormatAlignment {
    Left,
    Center,
    Right,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FormatSign {
    Minus,
    Plus,
    Space,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedFormatSpec {
    pub(crate) fill: char,
    pub(crate) alignment: Option<FormatAlignment>,
    pub(crate) sign: Option<FormatSign>,
    pub(crate) width: Option<usize>,
    pub(crate) zero_pad: bool,
    pub(crate) grouping: bool,
    pub(crate) precision: Option<usize>,
    pub(crate) ty: Option<char>,
}

fn format_syntax_error(message: impl Into<String>) -> FormatSpecError {
    FormatSpecError {
        kind: FormatSpecErrorKind::Syntax,
        message: message.into(),
    }
}

fn format_type_error(message: impl Into<String>) -> FormatSpecError {
    FormatSpecError {
        kind: FormatSpecErrorKind::Type,
        message: message.into(),
    }
}

pub(crate) fn parse_format_spec(
    source: &str,
) -> std::result::Result<ParsedFormatSpec, FormatSpecError> {
    if source.contains(['{', '}']) {
        return Err(format_syntax_error(
            "format specifications cannot contain nested replacement fields",
        ));
    }
    let chars = source.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut fill = ' ';
    let mut alignment = None;
    let alignment_for = |ch| match ch {
        '<' => Some(FormatAlignment::Left),
        '^' => Some(FormatAlignment::Center),
        '>' => Some(FormatAlignment::Right),
        _ => None,
    };
    if let Some(&first) = chars.first() {
        if let Some(value) = alignment_for(first) {
            alignment = Some(value);
            index = 1;
        } else if let Some(&second) = chars.get(1) {
            if let Some(value) = alignment_for(second) {
                fill = first;
                alignment = Some(value);
                index = 2;
            }
        }
    }

    let sign = match chars.get(index).copied() {
        Some('+') => {
            index += 1;
            Some(FormatSign::Plus)
        }
        Some('-') => {
            index += 1;
            Some(FormatSign::Minus)
        }
        Some(' ') => {
            index += 1;
            Some(FormatSign::Space)
        }
        _ => None,
    };

    let width_start = index;
    while chars.get(index).is_some_and(char::is_ascii_digit) {
        index += 1;
    }
    let width = if index > width_start {
        let value = chars[width_start..index]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .map_err(|_| format_syntax_error("format width is not a valid decimal integer"))?;
        if value > MAX_FORMAT_COMPONENT {
            return Err(format_syntax_error(format!(
                "format width cannot exceed {MAX_FORMAT_COMPONENT}"
            )));
        }
        Some(value)
    } else {
        None
    };
    let zero_pad = index > width_start && chars[width_start] == '0';

    let grouping = chars.get(index) == Some(&',');
    if grouping {
        index += 1;
    }

    let precision = if chars.get(index) == Some(&'.') {
        index += 1;
        let start = index;
        while chars.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
        if start == index {
            return Err(format_syntax_error(
                "format precision requires decimal digits after `.`",
            ));
        }
        let value = chars[start..index]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .map_err(|_| format_syntax_error("format precision is not a valid decimal integer"))?;
        if value > MAX_FORMAT_COMPONENT {
            return Err(format_syntax_error(format!(
                "format precision cannot exceed {MAX_FORMAT_COMPONENT}"
            )));
        }
        Some(value)
    } else {
        None
    };

    let ty = chars.get(index).copied();
    if let Some(ty) = ty {
        if !matches!(ty, 'd' | 'f' | 'e' | 'x' | 'X' | 'b' | 'o' | '%' | 's') {
            return Err(format_syntax_error(format!(
                "unsupported format type `{ty}`; supported codes are d, f, e, x, X, b, o, %, and s"
            )));
        }
        index += 1;
    }
    if index != chars.len() {
        return Err(format_syntax_error(format!(
            "malformed format specification `{source}`"
        )));
    }
    Ok(ParsedFormatSpec {
        fill,
        alignment,
        sign,
        width,
        zero_pad,
        grouping,
        precision,
        ty,
    })
}

fn format_type_family(ty: &Type) -> (&'static str, bool, bool, bool) {
    match ty {
        Type::Named(name, args) if args.is_empty() && name == "str" => ("str", true, false, false),
        Type::Named(name, args)
            if args.is_empty() && IntegerKind::from_runtime_type_name(name).is_some() =>
        {
            ("integer", false, true, true)
        }
        Type::Named(name, args)
            if args.is_empty() && matches!(name.as_str(), "float32" | "float64") =>
        {
            ("float", false, false, true)
        }
        _ => ("value", false, false, false),
    }
}

pub(crate) fn validate_format_spec_for_type(
    spec: &ParsedFormatSpec,
    value_type: &Type,
) -> std::result::Result<(), FormatSpecError> {
    let (family, is_string, is_integer, is_numeric) = format_type_family(value_type);
    match spec.ty {
        Some('s') if !is_string => {
            return Err(format_type_error(format!(
                "format code `s` requires `str`, found {family}"
            )))
        }
        Some('d' | 'x' | 'X' | 'b' | 'o') if !is_integer => {
            return Err(format_type_error(format!(
                "integer format code requires an integer value, found {family}"
            )))
        }
        Some('f' | 'e' | '%') if !is_numeric => {
            return Err(format_type_error(format!(
                "numeric format code requires an integer or floating value, found {family}"
            )))
        }
        Some(_) | None => {}
    }
    if spec.sign.is_some() && !is_numeric {
        return Err(format_type_error(
            "a format sign is valid only for numeric values",
        ));
    }
    if spec.zero_pad && !is_numeric {
        return Err(format_type_error(
            "zero-padding shorthand is valid only for numeric values",
        ));
    }
    if spec.grouping && !matches!(spec.ty, Some('d' | 'f' | '%')) {
        return Err(format_type_error(
            "the thousands separator is valid only with d, f, and %",
        ));
    }
    if spec.precision.is_some() && !matches!(spec.ty, Some('s' | 'f' | 'e' | '%')) {
        return Err(format_type_error(
            "precision requires s, f, e, or %; integer precision is available only through f, e, and %",
        ));
    }
    Ok(())
}

fn group_decimal_digits(value: &str) -> String {
    let (head, tail) = value.split_once('.').unwrap_or((value, ""));
    let mut grouped = String::with_capacity(value.len() + value.len() / 3);
    let first = head.len() % 3;
    let mut index = 0usize;
    if first != 0 {
        grouped.push_str(&head[..first]);
        index = first;
    }
    while index < head.len() {
        if !grouped.is_empty() {
            grouped.push(',');
        }
        grouped.push_str(&head[index..index + 3]);
        index += 3;
    }
    if !tail.is_empty() {
        grouped.push('.');
        grouped.push_str(tail);
    }
    grouped
}

fn normalize_scientific_exponent(rendered: String) -> String {
    let Some((mantissa, exponent)) = rendered.split_once('e') else {
        return rendered;
    };
    let parsed = exponent.parse::<i32>().unwrap_or(0);
    format!("{mantissa}e{parsed:+03}")
}

fn integer_magnitude(value: IntegerValue, radix: u32, uppercase: bool) -> (bool, String) {
    let (negative, magnitude) = match value.representation() {
        crate::integer::IntegerRepresentation::Signed(value) if value < 0 => {
            (true, value.unsigned_abs())
        }
        crate::integer::IntegerRepresentation::Signed(value) => (false, value as u128),
        crate::integer::IntegerRepresentation::Unsigned(value) => (false, value),
    };
    let rendered = match (radix, uppercase) {
        (2, _) => format!("{magnitude:b}"),
        (8, _) => format!("{magnitude:o}"),
        (16, false) => format!("{magnitude:x}"),
        (16, true) => format!("{magnitude:X}"),
        _ => magnitude.to_string(),
    };
    (negative, rendered)
}

fn apply_numeric_sign(mut magnitude: String, negative: bool, sign: Option<FormatSign>) -> String {
    let prefix = if negative {
        Some('-')
    } else {
        match sign {
            Some(FormatSign::Plus) => Some('+'),
            Some(FormatSign::Space) => Some(' '),
            Some(FormatSign::Minus) | None => None,
        }
    };
    if let Some(prefix) = prefix {
        magnitude.insert(0, prefix);
    }
    magnitude
}

fn apply_format_width(rendered: String, spec: &ParsedFormatSpec, numeric: bool) -> Result<String> {
    let width = spec.width.unwrap_or(0);
    let length = rendered.chars().count();
    if length >= width {
        if rendered.len() > MAX_STRING_BYTES {
            return Err(Diagnostic::coded(
                "AU4005",
                format!(
                    "string output exceeds the maintained {MAX_STRING_BYTES}-byte allocation limit"
                ),
            ));
        }
        return Ok(rendered);
    }
    let padding = width - length;
    if spec.zero_pad && spec.alignment.is_none() && numeric {
        let Some(padding_bytes) = padding.checked_add(rendered.len()) else {
            return Err(Diagnostic::coded(
                "AU4005",
                "string allocation size overflow",
            ));
        };
        if padding_bytes > MAX_STRING_BYTES {
            return Err(Diagnostic::coded(
                "AU4005",
                format!(
                    "string output exceeds the maintained {MAX_STRING_BYTES}-byte allocation limit"
                ),
            ));
        }
        let mut output = String::new();
        if output.try_reserve_exact(padding_bytes).is_err() {
            return Err(Diagnostic::coded("AU4005", "string allocation failed"));
        }
        let sign_bytes = rendered
            .chars()
            .next()
            .filter(|prefix| matches!(prefix, '+' | '-' | ' '))
            .map_or(0, char::len_utf8);
        output.push_str(&rendered[..sign_bytes]);
        output.extend(std::iter::repeat_n('0', padding));
        output.push_str(&rendered[sign_bytes..]);
        return Ok(output);
    }
    let alignment = spec.alignment.unwrap_or(if numeric {
        FormatAlignment::Right
    } else {
        FormatAlignment::Left
    });
    let (left, right) = match alignment {
        FormatAlignment::Left => (0, padding),
        FormatAlignment::Right => (padding, 0),
        FormatAlignment::Center => (padding / 2, padding - padding / 2),
    };
    let fill = if spec.zero_pad { '0' } else { spec.fill };
    let padding_bytes = padding
        .checked_mul(fill.len_utf8())
        .and_then(|bytes| bytes.checked_add(rendered.len()))
        .ok_or_else(|| Diagnostic::coded("AU4005", "string allocation size overflow"))?;
    if padding_bytes > MAX_STRING_BYTES {
        return Err(Diagnostic::coded(
            "AU4005",
            format!(
                "string output exceeds the maintained {MAX_STRING_BYTES}-byte allocation limit"
            ),
        ));
    }
    let mut output = String::new();
    output
        .try_reserve_exact(padding_bytes)
        .map_err(|_| Diagnostic::coded("AU4005", "string allocation failed"))?;
    output.extend(std::iter::repeat_n(fill, left));
    output.push_str(&rendered);
    output.extend(std::iter::repeat_n(fill, right));
    Ok(output)
}

pub(crate) fn format_runtime_value(
    value: &Value,
    value_type: &Type,
    source: &str,
) -> Result<String> {
    let spec = parse_format_spec(source).map_err(|error| {
        Diagnostic::coded(
            match error.kind {
                FormatSpecErrorKind::Syntax => "AU1101",
                FormatSpecErrorKind::Type => "AU2002",
            },
            error.message,
        )
    })?;
    validate_format_spec_for_type(&spec, value_type)
        .map_err(|error| Diagnostic::coded("AU2002", error.message))?;
    let (family, _is_string, is_integer, is_numeric) = format_type_family(value_type);
    let runtime_matches_static_family = match family {
        "str" => matches!(value, Value::String(_)),
        "integer" => matches!(value, Value::Int(_)),
        "float" => matches!(value, Value::Float(_)),
        _ => true,
    };
    if !runtime_matches_static_family {
        return Err(Diagnostic::coded(
            "AU4001",
            format!(
                "internal format contract mismatch: runtime value does not match static {family} type"
            ),
        ));
    }

    let rendered = match (spec.ty, value) {
        (Some('s'), Value::String(text)) => {
            if spec.precision.is_none() && text.len() > MAX_STRING_BYTES {
                return Err(Diagnostic::coded(
                    "AU4005",
                    format!(
                        "string output exceeds the maintained {MAX_STRING_BYTES}-byte allocation limit"
                    ),
                ));
            }
            text.chars()
                .take(spec.precision.unwrap_or(usize::MAX))
                .collect::<String>()
        }
        (Some(code @ ('d' | 'x' | 'X' | 'b' | 'o')), Value::Int(integer)) => {
            let radix = match code {
                'x' | 'X' => 16,
                'b' => 2,
                'o' => 8,
                _ => 10,
            };
            let (negative, mut magnitude) = integer_magnitude(*integer, radix, code == 'X');
            if spec.grouping {
                magnitude = group_decimal_digits(&magnitude);
            }
            apply_numeric_sign(magnitude, negative, spec.sign)
        }
        (Some(code @ ('f' | 'e' | '%')), Value::Int(integer)) => {
            format_numeric_integer(*integer, code, &spec)
        }
        (Some(code @ ('f' | 'e' | '%')), Value::Float(number)) => {
            let mut number = if matches!(value_type, Type::Named(name, args) if name == "float32" && args.is_empty()) {
                *number as f32 as f64
            } else {
                *number
            };
            if code == '%' {
                number *= 100.0;
            }
            format_numeric_float(number, code, &spec)
        }
        (None, Value::Float(number)) => {
            let rendered = if matches!(value_type, Type::Named(name, args) if name == "float32" && args.is_empty()) {
                render_float32(*number as f32)
            } else {
                render_float(*number)
            };
            apply_numeric_sign(rendered.trim_start_matches('-').to_string(), number.is_sign_negative(), spec.sign)
        }
        (None, Value::Int(integer)) => {
            let (negative, magnitude) = integer_magnitude(*integer, 10, false);
            apply_numeric_sign(magnitude, negative, spec.sign)
        }
        (None, _) => value.render(),
        (Some(code), _) => {
            return Err(Diagnostic::coded(
                "AU4001",
                format!(
                    "internal format contract mismatch: runtime value does not support format code `{code}`"
                ),
            ))
        }
    };
    apply_format_width(rendered, &spec, is_numeric || is_integer)
}

fn decimal_round_significant(mut digits: String, significant: usize) -> (String, bool) {
    if digits.len() <= significant {
        digits.extend(std::iter::repeat_n('0', significant - digits.len()));
        return (digits, false);
    }
    let discarded = digits.as_bytes()[significant..].to_vec();
    digits.truncate(significant);
    let round_up = discarded[0] > b'5'
        || (discarded[0] == b'5'
            && (discarded[1..].iter().any(|digit| *digit != b'0')
                || digits
                    .as_bytes()
                    .last()
                    .is_some_and(|digit| (digit - b'0') % 2 == 1)));
    if !round_up {
        return (digits, false);
    }
    let mut bytes = digits.into_bytes();
    for digit in bytes.iter_mut().rev() {
        if *digit != b'9' {
            *digit += 1;
            return (
                String::from_utf8(bytes).expect("decimal digits are UTF-8"),
                false,
            );
        }
        *digit = b'0';
    }
    bytes[0] = b'1';
    (
        String::from_utf8(bytes).expect("decimal digits are UTF-8"),
        true,
    )
}

fn format_numeric_integer(integer: IntegerValue, code: char, spec: &ParsedFormatSpec) -> String {
    let (negative, mut digits) = integer_magnitude(integer, 10, false);
    let precision = spec.precision.unwrap_or(6);
    let mut rendered = if code == 'e' {
        if digits == "0" {
            let fraction = "0".repeat(precision);
            if precision == 0 {
                "0e+00".to_string()
            } else {
                format!("0.{fraction}e+00")
            }
        } else {
            let mut exponent = digits.len() - 1;
            let (rounded, carried) = decimal_round_significant(digits, precision + 1);
            if carried {
                exponent += 1;
            }
            let (first, rest) = rounded.split_at(1);
            if precision == 0 {
                format!("{first}e{exponent:+03}")
            } else {
                format!("{first}.{rest}e{exponent:+03}")
            }
        }
    } else {
        if code == '%' {
            digits.push_str("00");
        }
        if spec.grouping {
            digits = group_decimal_digits(&digits);
        }
        if precision == 0 {
            digits
        } else {
            format!("{digits}.{}", "0".repeat(precision))
        }
    };
    rendered = apply_numeric_sign(rendered, negative, spec.sign);
    if code == '%' {
        rendered.push('%');
    }
    rendered
}

fn format_numeric_float(number: f64, code: char, spec: &ParsedFormatSpec) -> String {
    let negative = number.is_sign_negative();
    let magnitude = number.abs();
    let precision = spec.precision.unwrap_or(6);
    let mut rendered = if magnitude.is_nan() {
        "nan".to_string()
    } else if magnitude.is_infinite() {
        "inf".to_string()
    } else if code == 'e' {
        normalize_scientific_exponent(format!("{magnitude:.precision$e}"))
    } else {
        format!("{magnitude:.precision$}")
    };
    if spec.grouping && magnitude.is_finite() {
        rendered = group_decimal_digits(&rendered);
    }
    rendered = apply_numeric_sign(rendered, negative, spec.sign);
    if code == '%' {
        rendered.push('%');
    }
    rendered
}

pub(crate) fn float_floor_divmod(left: f64, right: f64) -> (f64, f64) {
    let mut remainder = left % right;
    let mut quotient = (left - remainder) / right;

    if remainder != 0.0 {
        if remainder.is_sign_negative() != right.is_sign_negative() {
            remainder += right;
            quotient -= 1.0;
        }
    } else {
        remainder = 0.0_f64.copysign(right);
    }

    let quotient = if quotient != 0.0 {
        let mut floored = quotient.floor();
        if quotient - floored > 0.5 {
            floored += 1.0;
        }
        floored
    } else {
        0.0_f64.copysign(left / right)
    };

    (quotient, remainder)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FloatPowerWidth {
    Float32,
    Float64,
}

/// Implements the shared floating-power contract at the destination type's
/// width. Computing float32 power in f64 and narrowing afterwards can both
/// double-round finite results and hide destination-width overflow.
pub(crate) fn float_power(left: f64, right: f64, width: FloatPowerWidth) -> Result<f64> {
    if right == 0.0 || left == 1.0 {
        return Ok(1.0);
    }
    if left.is_nan() || right.is_nan() {
        return Ok(f64::NAN);
    }
    if left == 0.0 && right.is_finite() && right < 0.0 {
        return Err(Diagnostic::coded("AU4001", "floating power domain error"));
    }
    if left.is_finite() && left < 0.0 && right.is_finite() && right != right.trunc() {
        return Err(Diagnostic::coded("AU4001", "floating power domain error"));
    }

    let result = match width {
        FloatPowerWidth::Float32 => (left as f32).powf(right as f32) as f64,
        FloatPowerWidth::Float64 => left.powf(right),
    };
    if left.is_finite() && right.is_finite() && result.is_infinite() {
        return Err(Diagnostic::coded("AU4002", "floating power overflow"));
    }
    if result.is_nan() {
        return Err(Diagnostic::coded("AU4001", "floating power domain error"));
    }
    Ok(result)
}

/// Implements Aura's exact `round(value)` numeric contract for both
/// execution backends. Floating results use ties-to-even and must fit the
/// complete mathematical `int64` range; integer values retain their exact
/// runtime kind.
pub(crate) fn round_numeric_value(value: &Value) -> Result<Value> {
    match value {
        Value::Int(value) => Ok(Value::Int(*value)),
        Value::Float(value) => {
            let rounded = value.round_ties_even();
            const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;
            const I64_MAX_EXCLUSIVE_AS_F64: f64 = 9_223_372_036_854_775_808.0;
            if !rounded.is_finite()
                || !(I64_MIN_AS_F64..I64_MAX_EXCLUSIVE_AS_F64).contains(&rounded)
            {
                return Err(Diagnostic::coded(
                    "AU4002",
                    format!(
                        "`round(...)` cannot represent `{}` as `int64`",
                        render_float(*value)
                    ),
                ));
            }
            Ok(Value::Int(IntegerValue::from_i64(rounded as i64)))
        }
        other => Err(Diagnostic::coded(
            "AU4001",
            format!(
                "`round(...)` expects an integer, `float32`, or `float64`, found `{}`",
                other.render()
            ),
        )),
    }
}

/// Implements Aura's paired floor quotient/remainder contract once so MIR
/// and direct execution cannot drift. `operand_type` is the statically
/// checked exact type used for both tuple elements.
pub(crate) fn divmod_numeric_values(
    left: &Value,
    right: &Value,
    operand_type: &Type,
) -> Result<Value> {
    let integer_kind = match operand_type {
        Type::Named(name, args) if args.is_empty() => IntegerKind::from_runtime_type_name(name),
        _ => None,
    };
    let (quotient, remainder) = match (left, right) {
        (Value::Int(_), Value::Int(right)) if right.is_zero() => {
            return Err(Diagnostic::coded(
                "AU4004",
                "`divmod(...)` divisor cannot be zero",
            ))
        }
        (Value::Int(left), Value::Int(right)) => {
            let mut quotient = left.checked_floor_div(*right).ok_or_else(|| {
                Diagnostic::coded("AU4002", "`divmod(...)` integer quotient overflow")
            })?;
            let mut remainder = left.checked_floor_rem(*right).ok_or_else(|| {
                Diagnostic::coded("AU4002", "`divmod(...)` integer remainder overflow")
            })?;
            if let Some(kind) = integer_kind {
                quotient = quotient.with_runtime_kind(kind).ok_or_else(|| {
                    Diagnostic::coded("AU4002", "`divmod(...)` integer quotient overflow")
                })?;
                remainder = remainder.with_runtime_kind(kind).ok_or_else(|| {
                    Diagnostic::coded("AU4002", "`divmod(...)` integer remainder overflow")
                })?;
            }
            (Value::Int(quotient), Value::Int(remainder))
        }
        (Value::Float(_), Value::Float(right)) if *right == 0.0 => {
            return Err(Diagnostic::coded(
                "AU4004",
                "`divmod(...)` divisor cannot be zero",
            ))
        }
        (Value::Float(left), Value::Float(right)) => {
            let (quotient, remainder) = float_floor_divmod(*left, *right);
            (Value::Float(quotient), Value::Float(remainder))
        }
        _ => {
            return Err(Diagnostic::coded(
                "AU4001",
                format!(
                "`divmod(...)` expects two values of one exact numeric type, found `{}` and `{}`",
                left.render(),
                right.render()
            ),
            ))
        }
    };
    Ok(Value::Tuple(TupleValue {
        element_types: vec![operand_type.clone(), operand_type.clone()],
        elements: vec![quotient, remainder],
    }))
}

impl ChannelValue {
    fn has_pending_values(&self) -> bool {
        !lock_mutex(&self.inner.state).queue.is_empty()
    }

    pub(crate) fn new() -> Self {
        Self::with_optional_capacity(None)
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self::with_optional_capacity(Some(capacity))
    }

    fn with_optional_capacity(capacity: Option<usize>) -> Self {
        Self {
            inner: Arc::new(ChannelState {
                state: Mutex::new(ChannelInner {
                    queue: VecDeque::new(),
                    closed: false,
                    capacity,
                }),
                producer_tasks: Mutex::new(WeakTaskRegistry::new()),
                task_handles: Mutex::new(WeakTaskRegistry::new()),
                sender_tasks: Mutex::new(WeakTaskRegistry::new()),
                receiver_tasks: Mutex::new(WeakTaskRegistry::new()),
                recv_reactor_subscribers: Mutex::new(HashMap::new()),
                send_reactor_subscribers: Mutex::new(HashMap::new()),
                runtime_type_name: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn runtime_type_name(&self) -> Option<String> {
        lock_mutex(&self.inner.runtime_type_name).clone()
    }

    pub(crate) fn set_runtime_type_name(&self, runtime_type_name: String) {
        *lock_mutex(&self.inner.runtime_type_name) = Some(runtime_type_name);
    }
}

pub(crate) fn collect_queue_values(value: &Value, queues: &mut Vec<ChannelValue>) {
    match value {
        Value::Channel(channel) => queues.push(channel.clone()),
        Value::Tuple(tuple) => {
            for element in &tuple.elements {
                collect_queue_values(element, queues);
            }
        }
        Value::Vec(vector) => {
            for element in &vector.elements {
                collect_queue_values(element, queues);
            }
        }
        Value::Set(set) => {
            for element in &set.elements {
                collect_queue_values(element, queues);
            }
        }
        Value::Map(map) => {
            for (key, value) in &map.entries {
                collect_queue_values(key, queues);
                collect_queue_values(value, queues);
            }
        }
        Value::Instance(instance) => {
            for value in instance.fields.values() {
                collect_queue_values(value, queues);
            }
        }
        Value::EnumVariant(variant) => {
            for payload in &variant.payloads {
                collect_queue_values(payload, queues);
            }
        }
        _ => {}
    }
}

fn register_current_task_queue_handles(value: &Value) {
    let Some(task) = current_lightweight_task() else {
        return;
    };
    let mut queues = Vec::new();
    collect_queue_values(value, &mut queues);
    for queue in queues {
        queue.register_task_handle(&task);
    }
}

#[cfg(test)]
pub(crate) fn register_task_as_queue_producer_for_values<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    task: &TaskValue,
) {
    let mut queues = Vec::new();
    for value in values {
        collect_queue_values(value, &mut queues);
    }
    for queue in queues {
        queue.register_producer_task(task);
        queue.register_task_handle(task);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TryRecvResult {
    Value(Value),
    Closed,
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrySendResult {
    Sent,
    Closed(Value),
    Full(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SendValueError {
    Closed(Box<Value>),
    Cancelled(Box<Value>),
    TimedOut(Box<Value>),
    Full(Box<Value>),
}

impl SendValueError {
    fn into_value(self) -> Box<Value> {
        match self {
            Self::Closed(value)
            | Self::Cancelled(value)
            | Self::TimedOut(value)
            | Self::Full(value) => value,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecvValueResult {
    Value(Value),
    Closed,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) enum TaskWaitStatus {
    Ready(std::result::Result<Value, Diagnostic>),
    TimedOut,
    Cancelled,
}

impl ChannelValue {
    fn register_task(tasks: &Mutex<WeakTaskRegistry>, task: &TaskValue) {
        let mut registry = lock_mutex(tasks);
        let identity = Arc::as_ptr(&task.inner) as usize;
        registry.tasks.insert(identity, Arc::downgrade(&task.inner));
        if registry.tasks.len() >= registry.next_prune_at {
            registry.tasks.retain(|_, task| task.strong_count() > 0);
            registry.next_prune_at = registry.tasks.len().saturating_mul(2).max(256);
        }
    }

    fn registered_tasks(tasks: &Mutex<WeakTaskRegistry>) -> Vec<TaskValue> {
        let mut registry = lock_mutex(tasks);
        let mut live = Vec::new();
        registry.tasks.retain(|_, task| {
            if let Some(inner) = task.upgrade() {
                live.push(TaskValue { inner });
                true
            } else {
                false
            }
        });
        registry.next_prune_at = registry.tasks.len().saturating_mul(2).max(256);
        live
    }

    fn register_current_sender(&self) {
        if let Some(task) = current_lightweight_task() {
            Self::register_task(&self.inner.sender_tasks, &task);
        }
    }

    fn register_current_receiver(&self) {
        if let Some(task) = current_lightweight_task() {
            Self::register_task(&self.inner.receiver_tasks, &task);
        }
    }

    fn subscribe_reactor_recv(&self, subscription: &ReactorSubscription, ignore_closed: bool) {
        lock_mutex(&self.inner.recv_reactor_subscribers)
            .entry(subscription.identity())
            .or_insert_with(|| ReactorRecvSubscription {
                target: subscription.clone(),
                ignore_closed,
            });
    }

    fn unsubscribe_reactor_recv(&self, subscription: &ReactorSubscription) {
        lock_mutex(&self.inner.recv_reactor_subscribers).remove(&subscription.identity());
    }

    fn subscribe_reactor_send(&self, subscription: &ReactorSubscription) {
        subscribe_reactor_target(&self.inner.send_reactor_subscribers, subscription);
    }

    fn unsubscribe_reactor_send(&self, subscription: &ReactorSubscription) {
        unsubscribe_reactor_target(&self.inner.send_reactor_subscribers, subscription);
    }

    fn wake_reactor_receivers_for_value(&self) {
        let targets = std::mem::take(&mut *lock_mutex(&self.inner.recv_reactor_subscribers));
        let mut targets: Vec<_> = targets.into_values().collect();
        targets.sort_unstable_by_key(|target| target.target.identity());
        for target in targets {
            let _ = target.target.wake();
        }
    }

    fn wake_closed_reactor_receivers(&self) {
        let targets = {
            let mut subscribers = lock_mutex(&self.inner.recv_reactor_subscribers);
            let mut targets = Vec::new();
            subscribers.retain(|_, entry| {
                if entry.ignore_closed {
                    true
                } else {
                    targets.push(entry.target.clone());
                    false
                }
            });
            targets
        };
        let mut targets = targets;
        targets.sort_unstable_by_key(ReactorSubscription::identity);
        for target in targets {
            let _ = target.wake();
        }
    }

    fn is_ready_for_scheduler_recv(&self, ignore_closed: bool) -> bool {
        let state = lock_mutex(&self.inner.state);
        !state.queue.is_empty() || (!ignore_closed && state.closed)
    }

    pub(crate) fn is_ready_for_scheduler_send(&self) -> bool {
        let state = lock_mutex(&self.inner.state);
        state.closed
            || state
                .capacity
                .is_none_or(|capacity| state.queue.len() < capacity)
    }

    pub(crate) fn register_task_handle(&self, task: &TaskValue) {
        Self::register_task(&self.inner.task_handles, task);
    }

    pub(crate) fn register_producer_task(&self, task: &TaskValue) {
        Self::register_task(&self.inner.producer_tasks, task);
    }

    fn registered_producer_tasks(&self) -> Vec<TaskValue> {
        Self::registered_tasks(&self.inner.producer_tasks)
    }

    fn registered_task_handles(&self) -> Vec<TaskValue> {
        Self::registered_tasks(&self.inner.task_handles)
    }

    fn registered_sender_tasks(&self) -> Vec<TaskValue> {
        Self::registered_tasks(&self.inner.sender_tasks)
    }

    fn registered_receiver_tasks(&self) -> Vec<TaskValue> {
        Self::registered_tasks(&self.inner.receiver_tasks)
    }

    fn is_open(&self) -> bool {
        !lock_mutex(&self.inner.state).closed
    }

    #[cfg(test)]
    fn all_registered_producer_tasks_completed(&self) -> bool {
        self.registered_producer_tasks()
            .iter()
            .all(|task| task.completed_result().is_some())
    }

    pub(crate) fn try_recv(&self) -> TryRecvResult {
        self.register_current_receiver();
        let mut state = lock_mutex(&self.inner.state);
        let was_full = state
            .capacity
            .is_some_and(|capacity| state.queue.len() >= capacity);
        if let Some(value) = state.queue.pop_front() {
            drop(state);
            register_current_task_queue_handles(&value);
            if was_full {
                wake_reactor_subscribers(&self.inner.send_reactor_subscribers);
            }
            notify_runtime_scheduler_if_started();
            return TryRecvResult::Value(value);
        }
        if state.closed {
            return TryRecvResult::Closed;
        }
        TryRecvResult::Empty
    }

    pub(crate) fn try_send(&self, value: Value) -> TrySendResult {
        self.register_current_sender();
        let mut state = lock_mutex(&self.inner.state);
        if state.closed {
            return TrySendResult::Closed(value);
        }
        if state
            .capacity
            .is_some_and(|capacity| state.queue.len() >= capacity)
        {
            return TrySendResult::Full(value);
        }
        let was_empty = state.queue.is_empty();
        state.queue.push_back(value);
        drop(state);
        if was_empty {
            self.wake_reactor_receivers_for_value();
        }
        notify_runtime_scheduler_if_started();
        TrySendResult::Sent
    }

    pub(crate) fn send(&self, value: Value) -> std::result::Result<(), Value> {
        match self.send_with_cancellation(value, None) {
            Ok(()) => Ok(()),
            Err(error) => Err(*error.into_value()),
        }
    }

    pub(crate) fn send_with_cancellation(
        &self,
        value: Value,
        cancellation: Option<&CancellationContext>,
    ) -> std::result::Result<(), SendValueError> {
        self.send_with_deadline(value, None, cancellation, false)
    }

    pub(crate) fn try_send_result(&self, value: Value) -> std::result::Result<(), SendValueError> {
        match self.try_send(value) {
            TrySendResult::Sent => Ok(()),
            TrySendResult::Closed(value) => Err(SendValueError::Closed(Box::new(value))),
            TrySendResult::Full(value) => Err(SendValueError::Full(Box::new(value))),
        }
    }

    pub(crate) fn send_with_timeout(
        &self,
        value: Value,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<std::result::Result<(), SendValueError>> {
        let deadline = deadline_from_timeout_labeled(timeout, "queue timeout")?;
        Ok(self.send_with_deadline(value, deadline, cancellation, false))
    }

    fn send_with_deadline(
        &self,
        mut value: Value,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
        fail_on_full: bool,
    ) -> std::result::Result<(), SendValueError> {
        loop {
            value = match self.try_send(value) {
                TrySendResult::Sent => return Ok(()),
                TrySendResult::Closed(value) => {
                    return Err(SendValueError::Closed(Box::new(value)))
                }
                TrySendResult::Full(value) if fail_on_full => {
                    return Err(SendValueError::Full(Box::new(value)));
                }
                TrySendResult::Full(value) => value,
            };

            match wait_for_runtime_scheduler(
                Vec::new(),
                false,
                vec![self.clone()],
                Vec::new(),
                deadline,
                cancellation,
            ) {
                RuntimeSchedulerWakeReason::Ready => {}
                RuntimeSchedulerWakeReason::TimedOut => {
                    return Err(SendValueError::TimedOut(Box::new(value)));
                }
                RuntimeSchedulerWakeReason::Cancelled => {
                    return Err(SendValueError::Cancelled(Box::new(value)));
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn recv_with_cancellation(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Value>> {
        Ok(
            match self.recv_result_with_cancellation(timeout, cancellation)? {
                RecvValueResult::Value(value) => Some(value),
                RecvValueResult::Closed
                | RecvValueResult::TimedOut
                | RecvValueResult::Cancelled => None,
            },
        )
    }

    pub(crate) fn recv_result_with_cancellation(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<RecvValueResult> {
        let deadline = deadline_from_timeout_labeled(timeout, "queue timeout")?;
        Ok(self.recv_result_with_deadline(deadline, cancellation))
    }

    fn recv_result_with_deadline(
        &self,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> RecvValueResult {
        loop {
            match self.try_recv() {
                TryRecvResult::Value(value) => {
                    if self.has_pending_values() {
                        let _ = yield_now_current_lightweight_task();
                    }
                    return RecvValueResult::Value(value);
                }
                TryRecvResult::Closed => return RecvValueResult::Closed,
                TryRecvResult::Empty => {}
            }

            match wait_for_runtime_scheduler(
                vec![self.clone()],
                false,
                Vec::new(),
                Vec::new(),
                deadline,
                cancellation,
            ) {
                RuntimeSchedulerWakeReason::Ready => {}
                RuntimeSchedulerWakeReason::TimedOut => return RecvValueResult::TimedOut,
                RuntimeSchedulerWakeReason::Cancelled => return RecvValueResult::Cancelled,
            }
        }
    }

    pub(crate) fn close(&self) {
        let mut state = lock_mutex(&self.inner.state);
        state.closed = true;
        drop(state);
        self.wake_closed_reactor_receivers();
        wake_reactor_subscribers(&self.inner.send_reactor_subscribers);
        notify_runtime_scheduler_if_started();
    }
}

fn closed_resource_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "resource is closed")
}

fn cancelled_resource_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "operation cancelled")
}

fn timeout_resource_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "operation timed out")
}

fn validate_udp_datagram_limit(max_bytes: usize) -> io::Result<usize> {
    if max_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP reads require `max_bytes` to be greater than zero",
        ));
    }
    if max_bytes > MAX_UDP_DATAGRAM_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "UDP reads are limited to {} bytes per datagram",
                MAX_UDP_DATAGRAM_BYTES
            ),
        ));
    }
    Ok(max_bytes)
}

fn normalize_udp_send_error(error: io::Error) -> io::Error {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EMSGSIZE) {
        return io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP datagram exceeds the platform send limit",
        );
    }
    error
}

pub(crate) fn create_dir_once(path: impl AsRef<Path>) -> io::Result<()> {
    std::fs::create_dir(path)
}

pub(crate) fn remove_file_checked(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "path is a directory",
            ));
        }
        Ok(_) => {}
        Err(error) => return Err(error),
    }
    std::fs::remove_file(path)
}

fn deadline_from_timeout_with<F>(
    timeout: Option<StdDuration>,
    label: &str,
    now: Instant,
    checked_add: F,
) -> io::Result<Option<Instant>>
where
    F: FnOnce(Instant, StdDuration) -> Option<Instant>,
{
    match timeout {
        Some(duration) => checked_deadline_after_with(now, duration, label, checked_add).map(Some),
        None => Ok(None),
    }
}

fn deadline_from_timeout_labeled(
    timeout: Option<StdDuration>,
    label: &str,
) -> io::Result<Option<Instant>> {
    deadline_from_timeout_with(timeout, label, Instant::now(), |now, duration| {
        now.checked_add(duration)
    })
}

fn deadline_from_timeout(timeout: Option<StdDuration>) -> io::Result<Option<Instant>> {
    deadline_from_timeout_labeled(timeout, "timeout")
}

fn check_deadline_and_cancellation(
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()> {
    if cancellation.is_some_and(CancellationContext::is_cancelled) {
        return Err(cancelled_resource_error());
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(timeout_resource_error());
    }
    Ok(())
}

fn acquire_protocol_operation<'a>(
    busy: &'a AtomicBool,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<ProtocolOperationGuard<'a>> {
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        if busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(ProtocolOperationGuard { busy });
        }
        if yield_now_current_lightweight_task().is_none() {
            thread::yield_now();
        }
    }
}

fn tls_handshake_deadline_with<F>(
    deadline: Option<Instant>,
    now: Instant,
    checked_add: F,
) -> io::Result<Option<Instant>>
where
    F: FnOnce(Instant, StdDuration) -> Option<Instant>,
{
    let cap = checked_deadline_after_with(
        now,
        DEFAULT_TLS_HANDSHAKE_TIMEOUT,
        "TLS handshake timeout",
        checked_add,
    )?;
    Ok(Some(
        deadline.map_or(cap, |deadline| std::cmp::min(deadline, cap)),
    ))
}

fn tls_handshake_deadline(deadline: Option<Instant>) -> io::Result<Option<Instant>> {
    tls_handshake_deadline_with(deadline, Instant::now(), |now, duration| {
        now.checked_add(duration)
    })
}

fn duration_to_poll_timeout(duration: StdDuration) -> libc::c_int {
    if duration.is_zero() {
        return 0;
    }
    let millis = duration.as_millis();
    if millis > i32::MAX as u128 {
        i32::MAX
    } else {
        millis as libc::c_int
    }
}

fn next_wait_slice(
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<StdDuration>> {
    if cancellation.is_some_and(|context| context.is_cancelled()) {
        return Err(cancelled_resource_error());
    }

    let poll_slice = StdDuration::from_millis(50);
    match deadline {
        Some(deadline) => {
            let now = Instant::now();
            if now >= deadline {
                Err(timeout_resource_error())
            } else {
                Ok(Some(std::cmp::min(
                    deadline.saturating_duration_since(now),
                    poll_slice,
                )))
            }
        }
        None if cancellation.is_some() => Ok(Some(poll_slice)),
        None => Ok(None),
    }
}

fn is_retryable_network_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

#[cfg(unix)]
fn set_fd_nonblocking(fd: libc::c_int, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut updated = flags;
    if enabled {
        updated |= libc::O_NONBLOCK;
    } else {
        updated &= !libc::O_NONBLOCK;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, updated) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn set_process_pipe_nonblocking<P: AsRawFd>(pipe: &P, enabled: bool) -> io::Result<()> {
    set_fd_nonblocking(pipe.as_raw_fd(), enabled)
}

fn timeout_deadline(timeout: Option<StdDuration>) -> io::Result<Option<Instant>> {
    deadline_from_timeout(timeout)
}

fn ensure_rustls_crypto_provider() {
    static INSTALL_PROVIDER: Once = Once::new();
    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(not(unix))]
trait ReadTimeoutStream: Read {
    fn set_read_timeout_value(&mut self, timeout: Option<StdDuration>) -> io::Result<()>;
}

#[cfg(not(unix))]
impl ReadTimeoutStream for StdTcpStream {
    fn set_read_timeout_value(&mut self, timeout: Option<StdDuration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

#[cfg(not(unix))]
impl ReadTimeoutStream for rustls::StreamOwned<ClientConnection, StdTcpStream> {
    fn set_read_timeout_value(&mut self, timeout: Option<StdDuration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }
}

#[cfg(not(unix))]
impl ReadTimeoutStream for rustls::StreamOwned<ServerConnection, StdTcpStream> {
    fn set_read_timeout_value(&mut self, timeout: Option<StdDuration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }
}

#[cfg(unix)]
fn wait_for_fd_event(
    fd: i32,
    events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()> {
    if let Some(reason) = yield_current_lightweight_wait(TaskWaitRegistration {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline,
        cancellation: cancellation.cloned(),
        fd_wait: Some(FdWaitRegistration { fd, events }),
    }) {
        return match reason {
            RuntimeSchedulerWakeReason::Ready => Ok(()),
            RuntimeSchedulerWakeReason::TimedOut => Err(timeout_resource_error()),
            RuntimeSchedulerWakeReason::Cancelled => Err(cancelled_resource_error()),
        };
    }

    loop {
        let slice = next_wait_slice(deadline, cancellation)?;
        let timeout_ms = match slice {
            Some(slice) => duration_to_poll_timeout(slice),
            None => -1,
        };
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            if deadline.is_some() && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(timeout_resource_error());
            }
            if deadline.is_none() && cancellation.is_none() {
                return Err(timeout_resource_error());
            }
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

#[cfg(unix)]
fn wait_for_tls_listener_progress(
    listener_fd: i32,
    pending_empty: bool,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()> {
    if pending_empty {
        wait_for_fd_event(listener_fd, libc::POLLIN, deadline, cancellation)
    } else {
        let slice = next_wait_slice(deadline, cancellation)?
            .unwrap_or_else(|| StdDuration::from_millis(50));
        let wait_deadline = Instant::now().checked_add(slice);
        match wait_for_fd_event(listener_fd, libc::POLLIN, wait_deadline, cancellation) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg_attr(unix, allow(dead_code))]
fn non_unix_tls_listener_wait_timeout(
    pending_empty: bool,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<StdDuration>> {
    if pending_empty && cancellation.is_none() {
        match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    Err(timeout_resource_error())
                } else {
                    Ok(Some(deadline.saturating_duration_since(now)))
                }
            }
            None => Ok(None),
        }
    } else {
        next_wait_slice(deadline, cancellation)
    }
}

#[cfg(not(unix))]
fn poll_tls_listener_readable(
    listener: StdTcpListener,
    timeout: Option<StdDuration>,
) -> io::Result<bool> {
    let mut poll = mio::Poll::new()?;
    let mut events = mio::Events::with_capacity(1);
    let mut source = mio::net::TcpListener::from_std(listener);
    poll.registry()
        .register(&mut source, mio::Token(0), mio::Interest::READABLE)?;
    loop {
        match poll.poll(&mut events, timeout) {
            Ok(()) => return Ok(!events.is_empty()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(unix))]
fn wait_for_tls_listener_progress(
    listener: &StdTcpListener,
    pending_empty: bool,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()> {
    let wait_timeout = non_unix_tls_listener_wait_timeout(pending_empty, deadline, cancellation)?;
    let listener = listener.try_clone()?;
    let readable = run_blocking_io(
        move || poll_tls_listener_readable(listener, wait_timeout),
        cancellation,
    )?;
    if readable {
        return Ok(());
    }
    check_deadline_and_cancellation(deadline, cancellation)?;
    Ok(())
}

pub(crate) fn io_decode_utf8(bytes: &[u8]) -> io::Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("received non-UTF-8 data: {}", error),
        )
    })
}

fn trim_line_endings(text: String) -> String {
    text.trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string()
}

pub(crate) fn read_all_limit_error(label: &str, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} exceeded the supported read_all limit of {} bytes",
            label, limit
        ),
    )
}

fn requested_read_limit_error(label: &str, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} exceeds the supported read limit of {} bytes",
            label, limit
        ),
    )
}

fn validate_requested_read_size(label: &str, count: usize) -> io::Result<usize> {
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} requires a byte count greater than zero", label),
        ));
    }
    if count > MAX_STREAM_READ_BYTES {
        return Err(requested_read_limit_error(label, MAX_STREAM_READ_BYTES));
    }
    Ok(count)
}

fn validate_read_line_capacity(buffer_len: usize) -> io::Result<()> {
    if buffer_len >= MAX_STREAM_READ_BYTES {
        return Err(read_all_limit_error(
            "network read_line",
            MAX_STREAM_READ_BYTES,
        ));
    }
    Ok(())
}

fn push_limited_bytes_with_limit(
    contents: &mut Vec<u8>,
    chunk: &[u8],
    label: &str,
    limit: usize,
) -> io::Result<()> {
    if contents.len().saturating_add(chunk.len()) > limit {
        return Err(read_all_limit_error(label, limit));
    }
    contents.extend_from_slice(chunk);
    Ok(())
}

fn push_limited_bytes(contents: &mut Vec<u8>, chunk: &[u8], label: &str) -> io::Result<()> {
    push_limited_bytes_with_limit(contents, chunk, label, MAX_STREAM_READ_BYTES)
}

fn read_all_from_reader_with_limit<R: Read>(
    reader: &mut R,
    label: &str,
    limit: usize,
) -> io::Result<Vec<u8>> {
    let mut limited = reader.take((limit as u64).saturating_add(1));
    let mut contents = Vec::new();
    limited.read_to_end(&mut contents)?;
    if contents.len() > limit {
        return Err(read_all_limit_error(label, limit));
    }
    Ok(contents)
}

#[cfg(not(unix))]
pub(crate) fn read_all_from_reader<R: Read>(reader: &mut R, label: &str) -> io::Result<Vec<u8>> {
    read_all_from_reader_with_limit(reader, label, MAX_STREAM_READ_BYTES)
}

fn validate_regular_file_remaining_size(
    file: &mut StdFile,
    label: &str,
    limit: usize,
) -> io::Result<()> {
    let Some(metadata) = file.metadata().ok().filter(|metadata| metadata.is_file()) else {
        return Ok(());
    };
    let Ok(position) = file.stream_position() else {
        return Ok(());
    };
    if metadata.len().saturating_sub(position) > limit as u64 {
        return Err(read_all_limit_error(label, limit));
    }
    Ok(())
}

fn read_std_file_with_limit(file: &mut StdFile, label: &str, limit: usize) -> io::Result<Vec<u8>> {
    validate_regular_file_remaining_size(file, label, limit)?;
    read_all_from_reader_with_limit(file, label, limit)
}

pub(crate) fn read_file_limited(path: &str, label: &str) -> io::Result<Vec<u8>> {
    let mut file = StdFile::open(path)?;
    read_std_file_with_limit(&mut file, label, MAX_FILESYSTEM_READ_BYTES)
}

fn read_tls_config_file(path: &str, label: &str) -> io::Result<Vec<u8>> {
    let mut file = StdFile::open(path)?;
    read_std_file_with_limit(&mut file, label, MAX_TLS_CONFIG_BYTES)
}

fn validate_http_header_name(name: &str) -> io::Result<()> {
    fn is_tchar(byte: u8) -> bool {
        matches!(
            byte,
            b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_'
                | b'`' | b'|' | b'~' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'
        )
    }

    if name.is_empty() || name.bytes().any(|byte| !is_tchar(byte)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid HTTP header name `{}`", name),
        ));
    }
    Ok(())
}

fn validate_http_header_value(value: &str) -> io::Result<()> {
    if value.bytes().any(|byte| {
        matches!(byte, b'\r' | b'\n')
            || (byte < 0x20 && byte != b'\t')
            || byte == 0x7f
            || byte >= 0x80
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTP header values may not contain control characters",
        ));
    }
    Ok(())
}

fn validate_http_headers(headers: &[(String, String)]) -> io::Result<()> {
    for (name, value) in headers {
        validate_http_header_name(name)?;
        validate_http_header_value(value)?;
    }
    Ok(())
}

#[cfg(unix)]
fn read_line_with_fd_deadline<R>(
    reader: &mut R,
    fd: i32,
    readiness_events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<String>>
where
    R: Read,
{
    let mut buffer = Vec::new();
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        validate_read_line_capacity(buffer.len())?;
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) if buffer.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(_) => {
                buffer.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if is_retryable_network_error(&error) => {
                wait_for_fd_event(fd, readiness_events, deadline, cancellation)?;
            }
            Err(error) => return Err(error),
        }
    }

    Ok(Some(trim_line_endings(io_decode_utf8(&buffer)?)))
}

#[cfg(unix)]
fn read_line_with_deadline<R>(
    reader: &mut R,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<String>>
where
    R: Read + AsRawFd,
{
    read_line_with_fd_deadline(
        reader,
        reader.as_raw_fd(),
        libc::POLLIN,
        deadline,
        cancellation,
    )
}

#[cfg(not(unix))]
fn read_line_with_deadline<R>(
    reader: &mut R,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<String>>
where
    R: ReadTimeoutStream,
{
    let mut buffer = Vec::new();
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        validate_read_line_capacity(buffer.len())?;
        reader.set_read_timeout_value(next_wait_slice(deadline, cancellation)?)?;
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) if buffer.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(_) => {
                buffer.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if is_retryable_network_error(&error) => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(Some(trim_line_endings(io_decode_utf8(&buffer)?)))
}

#[cfg(unix)]
fn read_exact_with_fd_deadline<R>(
    reader: &mut R,
    fd: i32,
    count: usize,
    readiness_events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<u8>>
where
    R: Read,
{
    validate_requested_read_size("read_exact(...)", count)?;
    let mut buffer = vec![0u8; count];
    let mut offset = 0;
    while offset < count {
        check_deadline_and_cancellation(deadline, cancellation)?;
        match reader.read(&mut buffer[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before reading the requested number of bytes",
                ))
            }
            Ok(bytes) => offset += bytes,
            Err(error) if is_retryable_network_error(&error) => {
                wait_for_fd_event(fd, readiness_events, deadline, cancellation)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(buffer)
}

#[cfg(unix)]
fn read_exact_with_deadline<R>(
    reader: &mut R,
    count: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<u8>>
where
    R: Read + AsRawFd,
{
    read_exact_with_fd_deadline(
        reader,
        reader.as_raw_fd(),
        count,
        libc::POLLIN,
        deadline,
        cancellation,
    )
}

#[cfg(not(unix))]
fn read_exact_with_deadline<R>(
    reader: &mut R,
    count: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<u8>>
where
    R: ReadTimeoutStream,
{
    validate_requested_read_size("read_exact(...)", count)?;
    let mut buffer = vec![0u8; count];
    let mut offset = 0;
    while offset < count {
        check_deadline_and_cancellation(deadline, cancellation)?;
        reader.set_read_timeout_value(next_wait_slice(deadline, cancellation)?)?;
        match reader.read(&mut buffer[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before reading the requested number of bytes",
                ))
            }
            Ok(bytes) => offset += bytes,
            Err(error) if is_retryable_network_error(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(buffer)
}

#[cfg(unix)]
fn read_some_with_fd_deadline<R>(
    reader: &mut R,
    fd: i32,
    max_bytes: usize,
    readiness_events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<Vec<u8>>>
where
    R: Read,
{
    let max_bytes = validate_requested_read_size("read_bytes(...)", max_bytes)?;
    let mut buffer = vec![0u8; max_bytes.max(1)];
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(bytes) => {
                buffer.truncate(bytes);
                return Ok(Some(buffer));
            }
            Err(error) if is_retryable_network_error(&error) => {
                wait_for_fd_event(fd, readiness_events, deadline, cancellation)?;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn read_some_with_deadline<R>(
    reader: &mut R,
    max_bytes: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<Vec<u8>>>
where
    R: Read + AsRawFd,
{
    read_some_with_fd_deadline(
        reader,
        reader.as_raw_fd(),
        max_bytes,
        libc::POLLIN,
        deadline,
        cancellation,
    )
}

#[cfg(not(unix))]
fn read_some_with_deadline<R>(
    reader: &mut R,
    max_bytes: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<Vec<u8>>>
where
    R: ReadTimeoutStream,
{
    let max_bytes = validate_requested_read_size("read_bytes(...)", max_bytes)?;
    let mut buffer = vec![0u8; max_bytes.max(1)];
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        reader.set_read_timeout_value(next_wait_slice(deadline, cancellation)?)?;
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(bytes) => {
                buffer.truncate(bytes);
                return Ok(Some(buffer));
            }
            Err(error) if is_retryable_network_error(&error) => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn read_all_with_fd_deadline<R>(
    reader: &mut R,
    fd: i32,
    readiness_events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<u8>>
where
    R: Read,
{
    let mut contents = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(bytes) => push_limited_bytes(&mut contents, &chunk[..bytes], "network read_all")?,
            Err(error) if is_retryable_network_error(&error) => {
                wait_for_fd_event(fd, readiness_events, deadline, cancellation)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(contents)
}

#[cfg(unix)]
fn read_all_with_deadline<R>(
    reader: &mut R,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<u8>>
where
    R: Read + AsRawFd,
{
    read_all_with_fd_deadline(
        reader,
        reader.as_raw_fd(),
        libc::POLLIN,
        deadline,
        cancellation,
    )
}

#[cfg(unix)]
fn write_all_with_fd_deadline<W>(
    writer: &mut W,
    fd: i32,
    bytes: &[u8],
    readiness_events: i16,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()>
where
    W: Write,
{
    let mut written = 0usize;
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "stream ended before enough bytes were written",
                ));
            }
            Ok(count) => {
                written += count;
            }
            Err(error) if is_retryable_network_error(&error) => {
                wait_for_fd_event(fd, readiness_events, deadline, cancellation)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_with_deadline<W>(
    writer: &mut W,
    bytes: &[u8],
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()>
where
    W: Write + AsRawFd,
{
    write_all_with_fd_deadline(
        writer,
        writer.as_raw_fd(),
        bytes,
        libc::POLLOUT,
        deadline,
        cancellation,
    )
}

#[cfg(test)]
fn load_tls_server_config(
    cert_pem_path: &str,
    key_pem_path: &str,
) -> io::Result<Arc<ServerConfig>> {
    let cert_pem = read_tls_config_file(cert_pem_path, "TLS certificate PEM")?;
    let key_pem = read_tls_config_file(key_pem_path, "TLS private key PEM")?;
    build_tls_server_config(cert_pem, key_pem)
}

fn build_tls_server_config(cert_pem: Vec<u8>, key_pem: Vec<u8>) -> io::Result<Arc<ServerConfig>> {
    ensure_rustls_crypto_provider();
    let cert_chain = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<std::result::Result<Vec<CertificateDer<'static>>, _>>()
        .map_err(io::Error::other)?;
    let Some(private_key) = PrivateKeyDer::pem_slice_iter(&key_pem).next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TLS private key PEM did not contain a key",
        ));
    };
    let private_key = private_key.map_err(io::Error::other)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .map_err(io::Error::other)?;
    Ok(Arc::new(config))
}

#[cfg(test)]
fn load_tls_root_store(ca_pem_path: Option<&str>) -> io::Result<RootCertStore> {
    let pem = ca_pem_path
        .filter(|path| !path.is_empty())
        .map(|path| read_tls_config_file(path, "TLS CA PEM"))
        .transpose()?;
    build_tls_root_store(pem)
}

fn build_tls_root_store(ca_pem: Option<Vec<u8>>) -> io::Result<RootCertStore> {
    ensure_rustls_crypto_provider();
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = ca_pem {
        for certificate in CertificateDer::pem_slice_iter(&pem) {
            let certificate = certificate.map_err(io::Error::other)?;
            roots.add(certificate).map_err(io::Error::other)?;
        }
    }
    Ok(roots)
}

fn parse_http_response(
    status: i32,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> HttpResponseValue {
    HttpResponseValue {
        status,
        reason,
        headers,
        body,
    }
}

const MAX_HTTP_HEADERS: usize = 64;
const MAX_HTTP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const HTTP_MESSAGE_TOO_LARGE_PREFIX: &str = "HTTP message exceeds the supported size limit";
const HTTP_HEADERS_TOO_LARGE_PREFIX: &str = "HTTP request exceeded the supported header count";

fn http_message_too_large_error_with_limit(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} of {} bytes", HTTP_MESSAGE_TOO_LARGE_PREFIX, limit),
    )
}

#[cfg(test)]
fn http_message_too_large_error() -> io::Error {
    http_message_too_large_error_with_limit(MAX_HTTP_MESSAGE_BYTES)
}

fn is_http_message_too_large_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidData
        && error.to_string().starts_with(HTTP_MESSAGE_TOO_LARGE_PREFIX)
}

fn http_headers_too_large_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} of {} headers",
            HTTP_HEADERS_TOO_LARGE_PREFIX, MAX_HTTP_HEADERS
        ),
    )
}

fn is_http_headers_too_large_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidData
        && error.to_string().starts_with(HTTP_HEADERS_TOO_LARGE_PREFIX)
}

fn is_http_bad_request_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
    ) && !is_http_message_too_large_error(error)
        && !is_http_headers_too_large_error(error)
}

fn http_reason_phrase(status: i32) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn http_header_name_eq(name: &str, expected: &str) -> bool {
    name.eq_ignore_ascii_case(expected)
}

fn parse_http_headers(headers: &[httparse::Header<'_>]) -> io::Result<Vec<(String, String)>> {
    headers
        .iter()
        .map(|header| {
            let value = std::str::from_utf8(header.value).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("received non-UTF-8 HTTP header value: {}", error),
                )
            })?;
            Ok((header.name.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
fn parse_http_content_length(headers: &[(String, String)]) -> io::Result<Option<usize>> {
    let mut content_length = None;
    for (name, value) in headers {
        if http_header_name_eq(name, "Transfer-Encoding") && !value.eq_ignore_ascii_case("identity")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Aura HTTP currently does not support transfer-encoding other than identity",
            ));
        }
        if http_header_name_eq(name, "Content-Length") {
            let parsed = value.parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid HTTP content length `{}`: {}", value, error),
                )
            })?;
            if let Some(existing) = content_length {
                if existing != parsed {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "conflicting HTTP content-length headers",
                    ));
                }
            } else {
                content_length = Some(parsed);
            }
        }
    }
    Ok(content_length)
}

fn parse_http_body_framing(
    headers: &[(String, String)],
    default: HttpBodyFraming,
) -> io::Result<HttpBodyFraming> {
    let content_length = headers
        .iter()
        .filter(|(name, _)| http_header_name_eq(name, "Content-Length"))
        .try_fold(None, |current, (_, value)| {
            let parsed = value.parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid HTTP content length `{value}`: {error}"),
                )
            })?;
            match current {
                Some(existing) if existing != parsed => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conflicting HTTP content-length headers",
                )),
                Some(existing) => Ok(Some(existing)),
                None => Ok(Some(parsed)),
            }
        })?;
    let transfer_codings = headers
        .iter()
        .filter(|(name, _)| http_header_name_eq(name, "Transfer-Encoding"))
        .flat_map(|(_, value)| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity")
        .collect::<Vec<_>>();
    if transfer_codings.is_empty() {
        return Ok(content_length
            .map(HttpBodyFraming::ContentLength)
            .unwrap_or(default));
    }
    if transfer_codings.as_slice() == ["chunked"] {
        if content_length.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP message cannot combine transfer-encoding chunked with content-length",
            ));
        }
        return Ok(HttpBodyFraming::Chunked);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "unsupported HTTP transfer-encoding `{}`",
            transfer_codings.join(", ")
        ),
    ))
}

fn push_http_chunk_with_limit(buffer: &mut Vec<u8>, chunk: &[u8], limit: usize) -> io::Result<()> {
    if buffer.len().saturating_add(chunk.len()) > limit {
        return Err(http_message_too_large_error_with_limit(limit));
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

fn find_http_crlf(bytes: &[u8], start: usize) -> Option<usize> {
    bytes
        .get(start..)?
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|offset| start + offset)
}

fn try_decode_chunked_http_body_with_limit(
    buffer: &[u8],
    start: usize,
    limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut cursor = start;
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find_http_crlf(buffer, cursor) else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&buffer[cursor..line_end]).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid chunk-size line: {error}"),
            )
        })?;
        let size_text = line.split_once(';').map_or(line, |(size, _)| size).trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid HTTP chunk size `{size_text}`: {error}"),
            )
        })?;
        cursor = line_end + 2;
        if size == 0 {
            if buffer.get(cursor..cursor + 2) == Some(b"\r\n") {
                return Ok(Some(body));
            }
            let Some(trailer_end) = buffer
                .get(cursor..)
                .and_then(|tail| tail.windows(4).position(|window| window == b"\r\n\r\n"))
            else {
                return Ok(None);
            };
            let trailer_bytes = &buffer[cursor..cursor + trailer_end];
            if trailer_bytes.len() > limit {
                return Err(http_message_too_large_error_with_limit(limit));
            }
            return Ok(Some(body));
        }
        if body.len().saturating_add(size) > limit {
            return Err(http_message_too_large_error_with_limit(limit));
        }
        let Some(chunk_end) = cursor.checked_add(size) else {
            return Err(http_message_too_large_error_with_limit(limit));
        };
        if buffer.len() < chunk_end.saturating_add(2) {
            return Ok(None);
        }
        if buffer.get(chunk_end..chunk_end + 2) != Some(b"\r\n") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP chunk data was not followed by CRLF",
            ));
        }
        body.extend_from_slice(&buffer[cursor..chunk_end]);
        cursor = chunk_end + 2;
    }
}

#[cfg(test)]
fn try_decode_chunked_http_body(buffer: &[u8], start: usize) -> io::Result<Option<Vec<u8>>> {
    try_decode_chunked_http_body_with_limit(buffer, start, MAX_HTTP_MESSAGE_BYTES)
}

fn parse_http_request_head(buffer: &[u8]) -> io::Result<Option<HttpRequestHead>> {
    let mut raw_headers = [httparse::EMPTY_HEADER; MAX_HTTP_HEADERS];
    let mut request = HttpParseRequest::new(&mut raw_headers);
    match request.parse(buffer).map_err(|error| match error {
        httparse::Error::TooManyHeaders => http_headers_too_large_error(),
        other => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid HTTP request: {}", other),
        ),
    })? {
        HttpParseStatus::Complete(header_len) => {
            let method = request
                .method
                .ok_or(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP request missing method",
                ))?
                .to_string();
            let path = request
                .path
                .ok_or(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP request missing path",
                ))?
                .to_string();
            let headers = parse_http_headers(request.headers)?;
            let framing = parse_http_body_framing(&headers, HttpBodyFraming::ContentLength(0))?;
            Ok(Some((header_len, method, path, headers, framing)))
        }
        HttpParseStatus::Partial => Ok(None),
    }
}

fn parse_http_response_head(buffer: &[u8]) -> io::Result<Option<HttpResponseHead>> {
    let mut raw_headers = [httparse::EMPTY_HEADER; MAX_HTTP_HEADERS];
    let mut response = HttpParseResponse::new(&mut raw_headers);
    match response.parse(buffer).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid HTTP response: {}", error),
        )
    })? {
        HttpParseStatus::Complete(header_len) => {
            let status = i32::from(response.code.ok_or(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response missing status code",
            ))?);
            let reason = response
                .reason
                .unwrap_or(http_reason_phrase(status))
                .to_string();
            let headers = parse_http_headers(response.headers)?;
            let framing = parse_http_body_framing(&headers, HttpBodyFraming::UntilClose)?;
            Ok(Some((header_len, status, reason, headers, framing)))
        }
        HttpParseStatus::Partial => Ok(None),
    }
}

fn parse_http_request_head_step(
    buffer: &[u8],
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<HttpRequestHead>> {
    let owned = buffer.to_vec();
    run_protocol_step_before(
        move || parse_http_request_head(&owned),
        deadline,
        cancellation,
    )
}

fn parse_http_response_head_step(
    buffer: &[u8],
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<HttpResponseHead>> {
    let owned = buffer.to_vec();
    run_protocol_step_before(
        move || parse_http_response_head(&owned),
        deadline,
        cancellation,
    )
}

fn decode_chunked_http_body_step(
    buffer: &[u8],
    start: usize,
    limit: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Option<Vec<u8>>> {
    let owned = buffer.to_vec();
    run_protocol_step_before(
        move || try_decode_chunked_http_body_with_limit(&owned, start, limit),
        deadline,
        cancellation,
    )
}

fn read_http_request_from_stream_with_limit(
    stream: &mut StdTcpStream,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
    message_limit: usize,
) -> io::Result<HttpRequestParts> {
    let mut buffer = Vec::new();
    let (header_len, method, path, headers, framing) = loop {
        let parsed = parse_http_request_head_step(&buffer, deadline, cancellation)?;
        check_deadline_and_cancellation(deadline, cancellation)?;
        if let Some(parsed) = parsed {
            break parsed;
        }
        let Some(chunk) = read_some_with_deadline(stream, 4096, deadline, cancellation)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream closed before a complete HTTP request was received",
            ));
        };
        push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
    };

    let body = match framing {
        HttpBodyFraming::ContentLength(content_length) => {
            if header_len.saturating_add(content_length) > message_limit {
                return Err(http_message_too_large_error_with_limit(message_limit));
            }
            while buffer.len() < header_len.saturating_add(content_length) {
                let Some(chunk) = read_some_with_deadline(stream, 4096, deadline, cancellation)?
                else {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream closed before the HTTP request body was fully received",
                    ));
                };
                push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
            }
            buffer[header_len..header_len.saturating_add(content_length)].to_vec()
        }
        HttpBodyFraming::Chunked => loop {
            let decoded = decode_chunked_http_body_step(
                &buffer,
                header_len,
                message_limit,
                deadline,
                cancellation,
            )?;
            check_deadline_and_cancellation(deadline, cancellation)?;
            if let Some(body) = decoded {
                break body;
            }
            let Some(chunk) = read_some_with_deadline(stream, 4096, deadline, cancellation)? else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before the chunked HTTP request body was fully received",
                ));
            };
            push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
        },
        HttpBodyFraming::UntilClose => unreachable!("requests always have explicit body framing"),
    };
    Ok((method, path, headers, body))
}

fn read_http_request_from_stream(
    stream: &mut StdTcpStream,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<HttpRequestParts> {
    read_http_request_from_stream_with_limit(stream, deadline, cancellation, MAX_HTTP_MESSAGE_BYTES)
}

trait HttpDeadlineReader: Read {
    fn read_http_some(
        &mut self,
        max_bytes: usize,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>>;
}

impl HttpDeadlineReader for StdTcpStream {
    fn read_http_some(
        &mut self,
        max_bytes: usize,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>> {
        read_some_with_deadline(self, max_bytes, deadline, cancellation)
    }
}

#[cfg(unix)]
impl HttpDeadlineReader for rustls::StreamOwned<ClientConnection, StdTcpStream> {
    fn read_http_some(
        &mut self,
        max_bytes: usize,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>> {
        read_some_with_fd_deadline(
            self,
            self.sock.as_raw_fd(),
            max_bytes,
            libc::POLLIN | libc::POLLOUT,
            deadline,
            cancellation,
        )
    }
}

fn read_http_response_from_stream_with_limit<R: HttpDeadlineReader>(
    stream: &mut R,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
    message_limit: usize,
) -> io::Result<HttpResponseValue> {
    let mut buffer = Vec::new();
    let (header_len, status, reason, headers, framing) = loop {
        let parsed = parse_http_response_head_step(&buffer, deadline, cancellation)?;
        check_deadline_and_cancellation(deadline, cancellation)?;
        if let Some(parsed) = parsed {
            break parsed;
        }
        let Some(chunk) = stream.read_http_some(4096, deadline, cancellation)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream closed before a complete HTTP response was received",
            ));
        };
        push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
    };

    let body = match framing {
        HttpBodyFraming::ContentLength(content_length) => {
            if header_len.saturating_add(content_length) > message_limit {
                return Err(http_message_too_large_error_with_limit(message_limit));
            }
            while buffer.len() < header_len.saturating_add(content_length) {
                let Some(chunk) = stream.read_http_some(4096, deadline, cancellation)? else {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream closed before the HTTP response body was fully received",
                    ));
                };
                push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
            }
            buffer[header_len..header_len.saturating_add(content_length)].to_vec()
        }
        HttpBodyFraming::Chunked => loop {
            let decoded = decode_chunked_http_body_step(
                &buffer,
                header_len,
                message_limit,
                deadline,
                cancellation,
            )?;
            check_deadline_and_cancellation(deadline, cancellation)?;
            if let Some(body) = decoded {
                break body;
            }
            let Some(chunk) = stream.read_http_some(4096, deadline, cancellation)? else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before the chunked HTTP response body was fully received",
                ));
            };
            push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
        },
        HttpBodyFraming::UntilClose => {
            while let Some(chunk) = stream.read_http_some(4096, deadline, cancellation)? {
                push_http_chunk_with_limit(&mut buffer, &chunk, message_limit)?;
            }
            buffer[header_len..].to_vec()
        }
    };

    Ok(parse_http_response(status, reason, headers, body))
}

fn read_http_response_from_stream<R: HttpDeadlineReader>(
    stream: &mut R,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<HttpResponseValue> {
    read_http_response_from_stream_with_limit(
        stream,
        deadline,
        cancellation,
        MAX_HTTP_MESSAGE_BYTES,
    )
}

fn build_http_request_bytes(
    method: &str,
    url: &Url,
    body: &[u8],
    headers: Vec<(String, String)>,
) -> io::Result<Vec<u8>> {
    validate_http_headers(&headers)?;
    let mut path = if url.path().is_empty() {
        "/".to_string()
    } else {
        url.path().to_string()
    };
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }

    let default_port = match url.scheme() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    };
    let host = match (url.host(), url.port()) {
        (Some(url::Host::Ipv6(host)), Some(port)) if Some(port) != default_port => {
            format!("[{}]:{}", host, port)
        }
        (Some(url::Host::Ipv6(host)), _) => format!("[{}]", host),
        (Some(url::Host::Ipv4(host)), Some(port)) if Some(port) != default_port => {
            format!("{}:{}", host, port)
        }
        (Some(url::Host::Ipv4(host)), _) => host.to_string(),
        (Some(url::Host::Domain(host)), Some(port)) if Some(port) != default_port => {
            format!("{}:{}", host, port)
        }
        (Some(url::Host::Domain(host)), _) => host.to_string(),
        (None, _) => String::new(),
    };

    let mut rendered = format!("{} {} HTTP/1.1\r\n", method, path).into_bytes();
    let mut saw_host = false;
    let mut saw_content_length = false;
    let mut saw_connection = false;
    for (name, value) in headers {
        if http_header_name_eq(&name, "Host") {
            saw_host = true;
        } else if http_header_name_eq(&name, "Content-Length") {
            saw_content_length = true;
        } else if http_header_name_eq(&name, "Connection") {
            saw_connection = true;
        }
        rendered.extend_from_slice(name.as_bytes());
        rendered.extend_from_slice(b": ");
        rendered.extend_from_slice(value.as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }
    if !saw_host {
        rendered.extend_from_slice(b"Host: ");
        rendered.extend_from_slice(host.as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }
    if !saw_content_length {
        rendered.extend_from_slice(b"Content-Length: ");
        rendered.extend_from_slice(body.len().to_string().as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }
    if !saw_connection {
        rendered.extend_from_slice(b"Connection: close\r\n");
    }
    rendered.extend_from_slice(b"\r\n");
    rendered.extend_from_slice(body);
    Ok(rendered)
}

fn write_http_response_to_stream(
    stream: &mut StdTcpStream,
    status: i32,
    headers: Vec<(String, String)>,
    body: &[u8],
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<()> {
    let body = body.to_vec();
    let rendered = run_protocol_step_before(
        move || build_http_response_bytes(status, headers, &body),
        deadline,
        cancellation,
    )?;
    write_all_with_deadline(stream, &rendered, deadline, cancellation)
}

fn build_http_response_bytes(
    status: i32,
    headers: Vec<(String, String)>,
    body: &[u8],
) -> io::Result<Vec<u8>> {
    validate_http_headers(&headers)?;
    let mut rendered =
        format!("HTTP/1.1 {} {}\r\n", status, http_reason_phrase(status)).into_bytes();
    let mut saw_content_length = false;
    let mut saw_connection = false;
    for (name, value) in headers {
        if http_header_name_eq(&name, "Content-Length") {
            saw_content_length = true;
        } else if http_header_name_eq(&name, "Connection") {
            saw_connection = true;
        }
        rendered.extend_from_slice(name.as_bytes());
        rendered.extend_from_slice(b": ");
        rendered.extend_from_slice(value.as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }
    if !saw_content_length {
        rendered.extend_from_slice(b"Content-Length: ");
        rendered.extend_from_slice(body.len().to_string().as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }
    if !saw_connection {
        rendered.extend_from_slice(b"Connection: close\r\n");
    }
    rendered.extend_from_slice(b"\r\n");
    rendered.extend_from_slice(body);
    Ok(rendered)
}

#[cfg(unix)]
fn unsupported_websocket_transport_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "unsupported websocket transport",
    )
}

#[cfg(unix)]
fn websocket_raw_fd(socket: &WebSocketStateKind) -> io::Result<i32> {
    match socket {
        WebSocketStateKind::Plain(socket) => Ok(socket.get_ref().as_raw_fd()),
        WebSocketStateKind::MaybeTls(socket) => maybe_tls_stream_raw_fd(socket.get_ref()),
    }
}

#[cfg(unix)]
fn maybe_tls_stream_raw_fd(stream: &MaybeTlsStream<StdTcpStream>) -> io::Result<i32> {
    match stream {
        MaybeTlsStream::Plain(stream) => Ok(stream.as_raw_fd()),
        MaybeTlsStream::Rustls(stream) => Ok(stream.get_ref().as_raw_fd()),
        _ => Err(unsupported_websocket_transport_error()),
    }
}

fn websocket_error_to_io(error: tungstenite::Error) -> io::Error {
    match error {
        tungstenite::Error::Io(error) => error,
        other => io::Error::other(other),
    }
}

#[cfg(unix)]
trait WebSocketHandshakeStream {
    fn raw_fd(&self) -> io::Result<i32>;
}

#[cfg(unix)]
impl WebSocketHandshakeStream for StdTcpStream {
    fn raw_fd(&self) -> io::Result<i32> {
        Ok(self.as_raw_fd())
    }
}

#[cfg(unix)]
impl WebSocketHandshakeStream for MaybeTlsStream<StdTcpStream> {
    fn raw_fd(&self) -> io::Result<i32> {
        maybe_tls_stream_raw_fd(self)
    }
}

#[cfg(unix)]
fn finish_websocket_handshake<Role>(
    mut mid: tungstenite::handshake::MidHandshake<Role>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Role::FinalResult>
where
    Role: tungstenite::handshake::HandshakeRole + Send + 'static,
    Role::InternalStream: Read + Write + WebSocketHandshakeStream + Send + 'static,
    Role::FinalResult: Send + 'static,
{
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        match run_protocol_step_before(move || Ok(mid.handshake()), deadline, cancellation)? {
            Ok(result) => return Ok(result),
            Err(tungstenite::handshake::HandshakeError::Interrupted(next_mid)) => {
                wait_for_fd_event(
                    next_mid.get_ref().get_ref().raw_fd()?,
                    libc::POLLIN | libc::POLLOUT,
                    deadline,
                    cancellation,
                )?;
                mid = next_mid;
            }
            Err(tungstenite::handshake::HandshakeError::Failure(error)) => {
                return Err(websocket_error_to_io(error));
            }
        }
    }
}

#[cfg(unix)]
fn accept_websocket_stream(
    stream: StdTcpStream,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<WebSocketStateKind> {
    check_deadline_and_cancellation(deadline, cancellation)?;
    stream.set_nonblocking(true)?;
    let socket = match run_protocol_step_before(
        move || {
            Ok(websocket_accept_with_config(
                stream,
                Some(websocket_config()),
            ))
        },
        deadline,
        cancellation,
    )? {
        Ok(socket) => socket,
        Err(tungstenite::handshake::HandshakeError::Interrupted(mid)) => {
            finish_websocket_handshake(mid, deadline, cancellation)?
        }
        Err(tungstenite::handshake::HandshakeError::Failure(error)) => {
            return Err(websocket_error_to_io(error));
        }
    };
    Ok(WebSocketStateKind::Plain(Box::new(socket)))
}

#[cfg(unix)]
fn connect_websocket_stream(
    stream: StdTcpStream,
    request: tungstenite::http::Request<()>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<WebSocketStateKind> {
    check_deadline_and_cancellation(deadline, cancellation)?;
    stream.set_nonblocking(true)?;
    stream.set_nodelay(true)?;
    let (socket, _) = match run_protocol_step_before(
        move || {
            Ok(client_tls_with_config(
                request,
                stream,
                Some(websocket_config()),
                None,
            ))
        },
        deadline,
        cancellation,
    )? {
        Ok(result) => result,
        Err(tungstenite::handshake::HandshakeError::Interrupted(mid)) => {
            finish_websocket_handshake(mid, deadline, cancellation)?
        }
        Err(tungstenite::handshake::HandshakeError::Failure(error)) => {
            return Err(websocket_error_to_io(error));
        }
    };
    Ok(WebSocketStateKind::MaybeTls(Box::new(socket)))
}

#[cfg(unix)]
fn websocket_set_nonblocking(socket: &mut WebSocketStateKind, enabled: bool) -> io::Result<()> {
    match socket {
        WebSocketStateKind::Plain(socket) => socket.get_mut().set_nonblocking(enabled),
        WebSocketStateKind::MaybeTls(socket) => match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_nonblocking(enabled),
            MaybeTlsStream::Rustls(stream) => stream.get_mut().set_nonblocking(enabled),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported websocket transport",
            )),
        },
    }
}

fn websocket_config() -> WebSocketConfig {
    #[allow(deprecated)]
    WebSocketConfig {
        max_send_queue: None,
        write_buffer_size: 128 * 1024,
        max_write_buffer_size: MAX_WEBSOCKET_WRITE_BUFFER_BYTES,
        max_message_size: Some(MAX_WEBSOCKET_MESSAGE_BYTES),
        max_frame_size: Some(MAX_WEBSOCKET_FRAME_BYTES),
        accept_unmasked_frames: false,
    }
}

fn websocket_client_key() -> io::Result<String> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(io::Error::other)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(nonce))
}

fn websocket_host_header(parsed: &Url) -> io::Result<String> {
    let host = parsed.host_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "websocket URL is missing a host",
        )
    })?;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn websocket_client_request(parsed: &Url) -> io::Result<tungstenite::http::Request<()>> {
    tungstenite::http::Request::builder()
        .method("GET")
        .uri(parsed.as_str())
        .header("Host", websocket_host_header(parsed)?)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", websocket_client_key()?)
        .body(())
        .map_err(io::Error::other)
}

impl FileValue {
    fn from_std(file: StdFile) -> Self {
        Self {
            inner: Arc::new(FileState {
                file: Mutex::new(Some(file)),
            }),
        }
    }

    pub(crate) fn open(path: &str) -> io::Result<Self> {
        let path = path.to_string();
        run_blocking_io(
            move || StdFile::open(path).map(Self::from_std),
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn create(path: &str) -> io::Result<Self> {
        let path = path.to_string();
        run_blocking_io(
            move || StdFile::create(path).map(Self::from_std),
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn append(path: &str) -> io::Result<Self> {
        let path = path.to_string();
        run_blocking_io(
            move || {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map(Self::from_std)
            },
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn read_all(&self) -> io::Result<String> {
        let state = self.inner.clone();
        run_blocking_io(
            move || {
                let mut file = lock_mutex(&state.file);
                let Some(file) = file.as_mut() else {
                    return Err(closed_resource_error());
                };
                io_decode_utf8(&read_std_file_with_limit(
                    file,
                    "file read_all",
                    MAX_FILESYSTEM_READ_BYTES,
                )?)
            },
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn read_bytes(&self) -> io::Result<Vec<u8>> {
        let state = self.inner.clone();
        run_blocking_io(
            move || {
                let mut file = lock_mutex(&state.file);
                let Some(file) = file.as_mut() else {
                    return Err(closed_resource_error());
                };
                read_std_file_with_limit(file, "file read_bytes", MAX_FILESYSTEM_READ_BYTES)
            },
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn write_all(&self, text: &str) -> io::Result<()> {
        self.write_bytes(text.as_bytes())
    }

    pub(crate) fn write_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        let state = self.inner.clone();
        let bytes = bytes.to_vec();
        run_blocking_io(
            move || {
                let mut file = lock_mutex(&state.file);
                let Some(file) = file.as_mut() else {
                    return Err(closed_resource_error());
                };
                file.write_all(&bytes)
            },
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn flush(&self) -> io::Result<()> {
        let state = self.inner.clone();
        run_blocking_io(
            move || {
                let mut file = lock_mutex(&state.file);
                let Some(file) = file.as_mut() else {
                    return Err(closed_resource_error());
                };
                file.flush()
            },
            current_lightweight_task_cancellation().as_ref(),
        )
    }

    pub(crate) fn close(&self) {
        let mut file = lock_mutex(&self.inner.file);
        *file = None;
    }
}

impl ProcessPipeValue {
    #[cfg(unix)]
    fn from_stdin(stdin: StdChildStdin) -> io::Result<Self> {
        set_process_pipe_nonblocking(&stdin, true)?;
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stdin(stdin))),
            }),
        })
    }

    #[cfg(not(unix))]
    fn from_stdin(stdin: StdChildStdin) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stdin(stdin))),
            }),
        })
    }

    #[cfg(unix)]
    fn from_stdout(stdout: StdChildStdout) -> io::Result<Self> {
        set_process_pipe_nonblocking(&stdout, true)?;
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stdout(stdout))),
            }),
        })
    }

    #[cfg(not(unix))]
    fn from_stdout(stdout: StdChildStdout) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stdout(stdout))),
            }),
        })
    }

    #[cfg(unix)]
    fn from_stderr(stderr: StdChildStderr) -> io::Result<Self> {
        set_process_pipe_nonblocking(&stderr, true)?;
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stderr(stderr))),
            }),
        })
    }

    #[cfg(not(unix))]
    fn from_stderr(stderr: StdChildStderr) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(ProcessPipeState {
                pipe: Mutex::new(Some(ProcessPipeKind::Stderr(stderr))),
            }),
        })
    }

    pub(crate) fn read_all(
        &self,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<String> {
        let bytes = self.read_all_bytes(cancellation)?;
        io_decode_utf8(&bytes)
    }

    pub(crate) fn read_all_bytes(
        &self,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Vec<u8>> {
        let mut pipe = lock_mutex(&self.inner.pipe);
        let Some(pipe) = pipe.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        let bytes = match pipe {
            ProcessPipeKind::Stdout(stdout) => read_all_with_deadline(stdout, None, cancellation)?,
            ProcessPipeKind::Stderr(stderr) => read_all_with_deadline(stderr, None, cancellation)?,
            ProcessPipeKind::Stdin(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot read from a process stdin pipe",
                ))
            }
        };
        #[cfg(not(unix))]
        let bytes = match pipe {
            ProcessPipeKind::Stdout(stdout) => {
                read_all_from_reader(stdout, "process pipe read_all")?
            }
            ProcessPipeKind::Stderr(stderr) => {
                read_all_from_reader(stderr, "process pipe read_all")?
            }
            ProcessPipeKind::Stdin(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot read from a process stdin pipe",
                ))
            }
        };
        Ok(bytes)
    }

    pub(crate) fn read_line(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<String>> {
        let deadline = timeout_deadline(timeout)?;
        let mut pipe = lock_mutex(&self.inner.pipe);
        let Some(pipe) = pipe.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            match pipe {
                ProcessPipeKind::Stdout(stdout) => {
                    read_line_with_deadline(stdout, deadline, cancellation)
                }
                ProcessPipeKind::Stderr(stderr) => {
                    read_line_with_deadline(stderr, deadline, cancellation)
                }
                ProcessPipeKind::Stdin(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot read from a process stdin pipe",
                )),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = deadline;
            let _ = cancellation;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process pipe line reads are only supported on unix in the bootstrap runtime",
            ))
        }
    }

    pub(crate) fn read_bytes(
        &self,
        max_bytes: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>> {
        let deadline = timeout_deadline(timeout)?;
        let mut pipe = lock_mutex(&self.inner.pipe);
        let Some(pipe) = pipe.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            match pipe {
                ProcessPipeKind::Stdout(stdout) => {
                    read_some_with_deadline(stdout, max_bytes, deadline, cancellation)
                }
                ProcessPipeKind::Stderr(stderr) => {
                    read_some_with_deadline(stderr, max_bytes, deadline, cancellation)
                }
                ProcessPipeKind::Stdin(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot read from a process stdin pipe",
                )),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = max_bytes;
            let _ = deadline;
            let _ = cancellation;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process pipe byte reads are only supported on unix in the bootstrap runtime",
            ))
        }
    }

    pub(crate) fn write_all(
        &self,
        text: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        self.write_bytes(text.as_bytes(), timeout, cancellation)
    }

    pub(crate) fn write_bytes(
        &self,
        bytes: &[u8],
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        let deadline = timeout_deadline(timeout)?;
        let mut pipe = lock_mutex(&self.inner.pipe);
        let Some(pipe) = pipe.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            match pipe {
                ProcessPipeKind::Stdin(stdin) => {
                    write_all_with_deadline(stdin, bytes, deadline, cancellation)
                }
                ProcessPipeKind::Stdout(_) | ProcessPipeKind::Stderr(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot write to a process output pipe",
                )),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = bytes;
            let _ = deadline;
            let _ = cancellation;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process pipe writes are only supported on unix in the bootstrap runtime",
            ))
        }
    }

    pub(crate) fn flush(&self) -> io::Result<()> {
        let mut pipe = lock_mutex(&self.inner.pipe);
        let Some(pipe) = pipe.as_mut() else {
            return Err(closed_resource_error());
        };
        match pipe {
            ProcessPipeKind::Stdin(stdin) => stdin.flush(),
            ProcessPipeKind::Stdout(_) | ProcessPipeKind::Stderr(_) => Ok(()),
        }
    }

    pub(crate) fn close(&self) {
        let mut pipe = lock_mutex(&self.inner.pipe);
        *pipe = None;
    }
}

impl ProcessCompletedValue {
    pub(crate) fn new(status: Value, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(ProcessCompletedState {
                status,
                stdout,
                stderr,
            }),
        }
    }

    pub(crate) fn status(&self) -> Value {
        self.inner.status.clone()
    }

    pub(crate) fn stdout(&self) -> io::Result<String> {
        io_decode_utf8(&self.inner.stdout)
    }

    pub(crate) fn stderr(&self) -> io::Result<String> {
        io_decode_utf8(&self.inner.stderr)
    }

    pub(crate) fn stdout_bytes(&self) -> Vec<u8> {
        self.inner.stdout.clone()
    }

    pub(crate) fn stderr_bytes(&self) -> Vec<u8> {
        self.inner.stderr.clone()
    }

    pub(crate) fn success(&self) -> bool {
        matches!(
            &self.inner.status,
            Value::EnumVariant(EnumVariantValue {
                enum_name,
                variant_name,
                payloads,
            }) if matches!(enum_name.as_str(), "ExitStatus" | "process.ExitStatus")
                && variant_name == "Exited"
                && matches!(payloads.as_slice(), [Value::Int(code)] if code.as_i128() == Some(0))
        )
    }

    pub(crate) fn check(&self) -> std::result::Result<(), Value> {
        if self.success() {
            Ok(())
        } else {
            Err(process_error_other(format!(
                "process exited with {}",
                self.status().render()
            )))
        }
    }
}

fn supervisor_restart_schedule_with<F>(
    name: &str,
    restart_count: i32,
    now: Instant,
    backoff: StdDuration,
    checked_add: F,
) -> SupervisorRestartSchedule
where
    F: FnOnce(Instant, StdDuration) -> Option<Instant>,
{
    match checked_deadline_after_with(now, backoff, "supervisor restart backoff", checked_add) {
        Ok(deadline) => SupervisorRestartSchedule::Deadline(deadline),
        Err(error) => SupervisorRestartSchedule::Failed(process_supervisor_event_failed(
            name.to_string(),
            process_error_io(error),
            IntegerValue::from_signed(i128::from(restart_count)),
        )),
    }
}

fn supervisor_restart_schedule(
    name: &str,
    restart_count: i32,
    now: Instant,
    backoff: StdDuration,
) -> SupervisorRestartSchedule {
    supervisor_restart_schedule_with(name, restart_count, now, backoff, |now, duration| {
        now.checked_add(duration)
    })
}

impl ProcessSupervisorValue {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ProcessSupervisorState {
                services: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &self,
        name: String,
        command: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        stdin: ProcessStdioConfig,
        stdout: ProcessStdioConfig,
        stderr: ProcessStdioConfig,
        restart: ProcessRestartPolicy,
        backoff: StdDuration,
        max_restarts: Option<i32>,
        group: bool,
    ) -> std::result::Result<(), Value> {
        if command.is_empty() {
            return Err(process_error_no_command());
        }
        if !matches!(restart, ProcessRestartPolicy::Never)
            && backoff < MIN_SUPERVISOR_RESTART_BACKOFF
        {
            return Err(process_error_io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "supervisor restart backoff must be at least {:?} when restart is enabled",
                    MIN_SUPERVISOR_RESTART_BACKOFF
                ),
            )));
        }

        {
            let services = lock_mutex(&self.inner.services);
            if services.contains_key(&name) {
                return Err(process_error_other(format!(
                    "supervisor already manages a child named `{}`",
                    name
                )));
            }
        }

        let child = ProcessChildValue::spawn(
            command.clone(),
            cwd.clone(),
            env.clone(),
            stdin,
            stdout,
            stderr,
            group,
        )
        .map_err(|error| process_error_spawn(error.to_string()))?;

        let mut services = lock_mutex(&self.inner.services);
        if services.contains_key(&name) {
            child.close();
            return Err(process_error_other(format!(
                "supervisor already manages a child named `{}`",
                name
            )));
        }
        services.insert(
            name,
            ProcessSupervisorEntry {
                spec: ProcessSupervisorSpec {
                    command,
                    cwd,
                    env,
                    stdin,
                    stdout,
                    stderr,
                    restart,
                    backoff,
                    max_restarts,
                    group,
                },
                child: Some(child),
                restart_count: 0,
                pending_restart_status: None,
                next_restart_at: None,
            },
        );
        Ok(())
    }

    pub(crate) fn wait(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> ProcessSupervisorWaitStatus {
        self.wait_result(timeout, cancellation)
            .unwrap_or_else(|error| {
                ProcessSupervisorWaitStatus::Event(process_supervisor_event_failed(
                    "<supervisor>".to_string(),
                    error,
                    IntegerValue::from_signed(0),
                ))
            })
    }

    fn wait_result(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> std::result::Result<ProcessSupervisorWaitStatus, Value> {
        let deadline = timeout_deadline(timeout).map_err(process_error_io)?;
        loop {
            match self.try_collect_event() {
                Ok(Some(event)) => return Ok(ProcessSupervisorWaitStatus::Event(event)),
                Ok(None) => {}
                Err(error) => return Err(error),
            }

            if self.is_empty() {
                return Ok(ProcessSupervisorWaitStatus::TimedOut);
            }
            if cancellation.is_some_and(CancellationContext::is_cancelled) {
                return Ok(ProcessSupervisorWaitStatus::Cancelled);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(ProcessSupervisorWaitStatus::TimedOut);
            }

            if let Err(error) =
                sleep_with_runtime_scheduler(StdDuration::from_millis(5), cancellation)
            {
                return Err(process_error_io(error));
            }
        }
    }

    pub(crate) fn wait_or_none(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> std::result::Result<Option<Value>, Value> {
        match self.wait_result(timeout, cancellation)? {
            ProcessSupervisorWaitStatus::Event(event) => Ok(Some(event)),
            ProcessSupervisorWaitStatus::TimedOut => Ok(None),
            ProcessSupervisorWaitStatus::Cancelled => Err(process_error_cancelled()),
        }
    }

    pub(crate) fn stop(&self) -> std::result::Result<(), Value> {
        let drained: Vec<ProcessChildValue> = {
            let mut services = lock_mutex(&self.inner.services);
            std::mem::take(&mut *services)
                .into_values()
                .filter_map(|entry| entry.child)
                .collect::<Vec<_>>()
        };
        for child in drained {
            child.close();
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        lock_mutex(&self.inner.services).is_empty()
    }

    pub(crate) fn close(&self) {
        let _ = self.stop();
    }

    fn try_collect_event(&self) -> std::result::Result<Option<Value>, Value> {
        let now = Instant::now();
        let names = {
            let services = lock_mutex(&self.inner.services);
            services.keys().cloned().collect::<Vec<_>>()
        };

        for name in names {
            enum Action {
                None,
                Emit(Value),
                RemoveAndEmit(Value),
            }

            let action = {
                let mut services = lock_mutex(&self.inner.services);
                let Some(entry) = services.get_mut(&name) else {
                    continue;
                };

                if let Some(child) = entry.child.clone() {
                    match child.try_wait_once() {
                        Ok(Some(status)) => {
                            if should_restart_process(
                                entry.spec.restart,
                                &status,
                                entry.restart_count,
                                entry.spec.max_restarts,
                            ) {
                                entry.restart_count += 1;
                                match supervisor_restart_schedule(
                                    &name,
                                    entry.restart_count,
                                    now,
                                    entry.spec.backoff,
                                ) {
                                    SupervisorRestartSchedule::Deadline(deadline) => {
                                        entry.child = None;
                                        entry.pending_restart_status = Some(status);
                                        entry.next_restart_at = Some(deadline);
                                        Action::None
                                    }
                                    SupervisorRestartSchedule::Failed(event) => {
                                        Action::RemoveAndEmit(event)
                                    }
                                }
                            } else {
                                Action::RemoveAndEmit(process_supervisor_event_exited(
                                    name.clone(),
                                    status,
                                    IntegerValue::from_signed(entry.restart_count as i128),
                                ))
                            }
                        }
                        Ok(None) => Action::None,
                        Err(error) => Action::RemoveAndEmit(process_supervisor_event_failed(
                            name.clone(),
                            process_error_io(error),
                            IntegerValue::from_signed(entry.restart_count as i128),
                        )),
                    }
                } else if let (Some(status), Some(next_restart_at)) =
                    (entry.pending_restart_status, entry.next_restart_at)
                {
                    if next_restart_at <= now {
                        match ProcessChildValue::spawn(
                            entry.spec.command.clone(),
                            entry.spec.cwd.clone(),
                            entry.spec.env.clone(),
                            entry.spec.stdin,
                            entry.spec.stdout,
                            entry.spec.stderr,
                            entry.spec.group,
                        ) {
                            Ok(restarted_child) => {
                                entry.child = Some(restarted_child);
                                entry.pending_restart_status = None;
                                entry.next_restart_at = None;
                                Action::Emit(process_supervisor_event_restarted(
                                    name.clone(),
                                    status,
                                    IntegerValue::from_signed(entry.restart_count as i128),
                                ))
                            }
                            Err(error) => Action::RemoveAndEmit(process_supervisor_event_failed(
                                name.clone(),
                                process_error_spawn(error.to_string()),
                                IntegerValue::from_signed(entry.restart_count as i128),
                            )),
                        }
                    } else {
                        Action::None
                    }
                } else {
                    Action::None
                }
            };

            match action {
                Action::None => {}
                Action::Emit(event) => return Ok(Some(event)),
                Action::RemoveAndEmit(event) => {
                    lock_mutex(&self.inner.services).remove(&name);
                    return Ok(Some(event));
                }
            }
        }

        Ok(None)
    }
}

impl ProcessChildValue {
    pub(crate) fn spawn(
        command: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        stdin: ProcessStdioConfig,
        stdout: ProcessStdioConfig,
        stderr: ProcessStdioConfig,
        group: bool,
    ) -> io::Result<Self> {
        let Some(program) = command.first().cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process.start(...) requires at least one command element",
            ));
        };
        let mut builder = StdCommand::new(program);
        builder.args(command.iter().skip(1));
        if let Some(cwd) = cwd {
            builder.current_dir(cwd);
        }
        for (key, value) in env {
            builder.env(key, value);
        }
        builder.stdin(stdin.as_stdio());
        builder.stdout(stdout.as_stdio());
        builder.stderr(stderr.as_stdio());
        #[cfg(unix)]
        if group {
            // SAFETY: This runs in the child just before exec. It only calls the
            // async-signal-safe `setpgid(0, 0)` to place the child in its own
            // process group.
            unsafe {
                builder.pre_exec(|| {
                    if libc::setpgid(0, 0) < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        #[cfg(not(unix))]
        if group {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process groups are only supported on Unix hosts",
            ));
        }
        let mut child = builder.spawn()?;
        let process_group_id = if group { Some(child.id() as i32) } else { None };
        let stdin = child
            .stdin
            .take()
            .map(ProcessPipeValue::from_stdin)
            .transpose()?;
        let stdout = child
            .stdout
            .take()
            .map(ProcessPipeValue::from_stdout)
            .transpose()?;
        let stderr = child
            .stderr
            .take()
            .map(ProcessPipeValue::from_stderr)
            .transpose()?;
        Ok(Self {
            inner: Arc::new(ProcessChildState {
                child: Mutex::new(Some(child)),
                waited: Mutex::new(None),
                process_group_id,
                stdin,
                stdout,
                stderr,
            }),
        })
    }

    pub(crate) fn stdin(&self) -> Option<ProcessPipeValue> {
        self.inner.stdin.clone()
    }

    pub(crate) fn stdout(&self) -> Option<ProcessPipeValue> {
        self.inner.stdout.clone()
    }

    pub(crate) fn stderr(&self) -> Option<ProcessPipeValue> {
        self.inner.stderr.clone()
    }

    pub(crate) fn wait(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> ProcessChildWaitStatus {
        if let Some(status) = *lock_mutex(&self.inner.waited) {
            return ProcessChildWaitStatus::Exited(status);
        }
        let deadline = match timeout_deadline(timeout) {
            Ok(deadline) => deadline,
            Err(error) => return ProcessChildWaitStatus::Failed(error),
        };
        loop {
            match self.try_wait_once() {
                Ok(Some(status)) => return ProcessChildWaitStatus::Exited(status),
                Ok(None) => {}
                Err(error) => return ProcessChildWaitStatus::Failed(error),
            }
            if cancellation.is_some_and(CancellationContext::is_cancelled) {
                return ProcessChildWaitStatus::Cancelled;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return ProcessChildWaitStatus::TimedOut;
            }
            if let Err(error) =
                sleep_with_runtime_scheduler(StdDuration::from_millis(5), cancellation)
            {
                return ProcessChildWaitStatus::Failed(error);
            }
        }
    }

    pub(crate) fn wait_or_none(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> std::result::Result<Option<StdExitStatus>, Value> {
        match self.wait(timeout, cancellation) {
            ProcessChildWaitStatus::Exited(status) => Ok(Some(status)),
            ProcessChildWaitStatus::TimedOut => Ok(None),
            ProcessChildWaitStatus::Cancelled => Err(process_error_cancelled()),
            ProcessChildWaitStatus::Failed(error) => Err(process_error_io(error)),
        }
    }

    pub(crate) fn wait_ok(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> std::result::Result<StdExitStatus, Value> {
        match self.wait(timeout, cancellation) {
            ProcessChildWaitStatus::Exited(status) if status.success() => Ok(status),
            ProcessChildWaitStatus::Exited(status) => Err(process_error_other(format!(
                "process exited with {}",
                process_exit_status(status).render()
            ))),
            ProcessChildWaitStatus::TimedOut => Err(process_error_timed_out()),
            ProcessChildWaitStatus::Cancelled => Err(process_error_cancelled()),
            ProcessChildWaitStatus::Failed(error) => Err(process_error_io(error)),
        }
    }

    fn try_wait_once(&self) -> io::Result<Option<StdExitStatus>> {
        if let Some(status) = *lock_mutex(&self.inner.waited) {
            return Ok(Some(status));
        }
        let mut child_slot = lock_mutex(&self.inner.child);
        let Some(child) = child_slot.as_mut() else {
            return Ok(*lock_mutex(&self.inner.waited));
        };
        match child.try_wait()? {
            Some(status) => {
                *lock_mutex(&self.inner.waited) = Some(status);
                *child_slot = None;
                Ok(Some(status))
            }
            None => Ok(None),
        }
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        if let Some(process_group_id) = self.inner.process_group_id {
            return signal_process_group(process_group_id, libc::SIGKILL);
        }
        let mut child = lock_mutex(&self.inner.child);
        let Some(child) = child.as_mut() else {
            return Ok(());
        };
        child.kill()
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        if let Some(process_group_id) = self.inner.process_group_id {
            return signal_process_group(process_group_id, libc::SIGTERM);
        }
        let mut child = lock_mutex(&self.inner.child);
        let Some(child) = child.as_mut() else {
            return Ok(());
        };
        #[cfg(unix)]
        {
            if unsafe { libc::kill(child.id() as i32, libc::SIGTERM) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            child.kill()
        }
    }

    pub(crate) fn close(&self) {
        let _ = self.terminate();
        let wait_timed_out = matches!(
            self.wait(
                Some(StdDuration::from_millis(100)),
                current_lightweight_task_cancellation().as_ref(),
            ),
            ProcessChildWaitStatus::TimedOut
        );
        let group_wait_timed_out = if wait_timed_out {
            true
        } else {
            !self.wait_for_process_group_exit(
                Some(StdDuration::from_millis(100)),
                current_lightweight_task_cancellation().as_ref(),
            )
        };
        if wait_timed_out || group_wait_timed_out {
            let _ = self.kill();
            let _ = self.wait(
                Some(StdDuration::from_millis(100)),
                current_lightweight_task_cancellation().as_ref(),
            );
            let _ = self.wait_for_process_group_exit(
                Some(StdDuration::from_millis(100)),
                current_lightweight_task_cancellation().as_ref(),
            );
        }
        if let Some(stdin) = &self.inner.stdin {
            stdin.close();
        }
        if let Some(stdout) = &self.inner.stdout {
            stdout.close();
        }
        if let Some(stderr) = &self.inner.stderr {
            stderr.close();
        }
    }

    fn wait_for_process_group_exit(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> bool {
        let Some(process_group_id) = self.inner.process_group_id else {
            return true;
        };
        let deadline = match deadline_from_timeout_labeled(timeout, "process group cleanup timeout")
        {
            Ok(deadline) => deadline,
            Err(_) => return false,
        };
        loop {
            if !process_group_alive(process_group_id) {
                return true;
            }
            if cancellation.is_some_and(CancellationContext::is_cancelled) {
                return false;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return false;
            }
            if sleep_with_runtime_scheduler(StdDuration::from_millis(5), cancellation).is_err() {
                return false;
            }
        }
    }
}

#[cfg(unix)]
fn signal_process_group(process_group_id: i32, signal: libc::c_int) -> io::Result<()> {
    if unsafe { libc::kill(-process_group_id, signal) } < 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(()),
            _ => Err(error),
        }
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn signal_process_group(_process_group_id: i32, _signal: libc::c_int) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn process_group_alive(process_group_id: i32) -> bool {
    if unsafe { libc::kill(-process_group_id, 0) } == 0 {
        true
    } else {
        let error = io::Error::last_os_error();
        matches!(error.raw_os_error(), Some(libc::EPERM))
    }
}

#[cfg(not(unix))]
fn process_group_alive(_process_group_id: i32) -> bool {
    false
}

fn should_restart_process(
    policy: ProcessRestartPolicy,
    status: &StdExitStatus,
    restart_count: i32,
    max_restarts: Option<i32>,
) -> bool {
    let allowed_by_policy = match policy {
        ProcessRestartPolicy::Never => false,
        ProcessRestartPolicy::OnFailure => !status.success(),
        ProcessRestartPolicy::Always => true,
    };
    if !allowed_by_policy {
        return false;
    }
    max_restarts.is_none_or(|max_restarts| restart_count < max_restarts)
}

impl TcpListenerValue {
    fn from_std(listener: StdTcpListener) -> io::Result<Self> {
        #[cfg(unix)]
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(TcpListenerState {
                listener: Mutex::new(Some(listener)),
            }),
        })
    }

    pub(crate) fn bind(address: &str) -> io::Result<Self> {
        let address = address.to_string();
        let listener = run_blocking_io_with_deadline(
            move || StdTcpListener::bind(address),
            None,
            current_lightweight_task_cancellation().as_ref(),
        )?;
        Self::from_std(listener)
    }

    pub(crate) fn accept(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<TcpStreamValue> {
        let mut listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_mut() else {
            return Err(closed_resource_error());
        };
        let deadline = deadline_from_timeout(timeout)?;
        loop {
            match listener.accept() {
                Ok((stream, _)) => return TcpStreamValue::from_std(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    wait_for_fd_event(listener.as_raw_fd(), libc::POLLIN, deadline, cancellation)?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let mut listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_mut() else {
            return Err(closed_resource_error());
        };
        Ok(listener.local_addr()?.to_string())
    }

    pub(crate) fn close(&self) {
        let mut listener = lock_mutex(&self.inner.listener);
        *listener = None;
    }
}

fn resolve_socket_addresses_before(
    address: &str,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<Vec<SocketAddr>> {
    let pool = match blocking_io_pool() {
        Ok(pool) => pool,
        Err(error) => raise_blocking_io_pool_startup_failure(error),
    };
    resolve_socket_addresses_before_on_pool_with(address, deadline, cancellation, pool, |address| {
        address
            .to_socket_addrs()
            .map(|addresses| addresses.collect())
    })
}

fn resolve_socket_addresses_before_on_pool_with<F>(
    address: &str,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
    pool: &Arc<BlockingIoPool>,
    resolver: F,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let address = address.to_string();
    run_blocking_io_with_deadline_on_pool(move || resolver(address), deadline, cancellation, pool)
}

fn connect_resolved_tcp_candidates_with_clock<T, N, C>(
    address: &str,
    addresses: Vec<SocketAddr>,
    deadline: Option<Instant>,
    mut now: N,
    mut connect: C,
) -> io::Result<T>
where
    N: FnMut() -> Instant,
    C: FnMut(SocketAddr, Option<StdDuration>) -> io::Result<T>,
{
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{address}` did not resolve to any socket addresses"),
        ));
    }

    let mut last_error = None;
    for candidate in addresses {
        let candidate_timeout = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(now());
                if remaining.is_zero() {
                    return Err(timeout_resource_error());
                }
                Some(remaining)
            }
            None => None,
        };
        match connect(candidate, candidate_timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
        if deadline.is_some_and(|deadline| now() >= deadline) {
            return Err(timeout_resource_error());
        }
    }

    Err(last_error.unwrap_or_else(|| io::Error::other("failed to connect to any socket address")))
}

impl TcpStreamValue {
    fn from_std(stream: StdTcpStream) -> io::Result<Self> {
        #[cfg(unix)]
        stream.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(TcpStreamState {
                stream: Mutex::new(Some(stream)),
            }),
        })
    }

    pub(crate) fn connect(
        address: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::connect_before(address, deadline_from_timeout(timeout)?, cancellation)
    }

    fn connect_before(
        address: &str,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::connect_with_deadline_and_operations(
            address,
            deadline,
            cancellation,
            |address| {
                address
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect())
            },
            |candidate, timeout| match timeout {
                Some(timeout) => StdTcpStream::connect_timeout(&candidate, timeout),
                None => StdTcpStream::connect(candidate),
            },
        )
    }

    #[cfg(test)]
    fn connect_with_operations<R, C>(
        address: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
        resolve: R,
        connect: C,
    ) -> io::Result<Self>
    where
        R: FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
        C: FnMut(SocketAddr, Option<StdDuration>) -> io::Result<StdTcpStream> + Send + 'static,
    {
        Self::connect_with_deadline_and_operations(
            address,
            deadline_from_timeout(timeout)?,
            cancellation,
            resolve,
            connect,
        )
    }

    #[cfg(test)]
    fn connect_with_operations_on_pool<R, C>(
        address: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
        pool: &Arc<BlockingIoPool>,
        resolve: R,
        connect: C,
    ) -> io::Result<Self>
    where
        R: FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
        C: FnMut(SocketAddr, Option<StdDuration>) -> io::Result<StdTcpStream> + Send + 'static,
    {
        Self::connect_with_deadline_and_operations_on_pool(
            address,
            deadline_from_timeout(timeout)?,
            cancellation,
            pool,
            resolve,
            connect,
        )
    }

    fn connect_with_deadline_and_operations<R, C>(
        address: &str,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
        resolve: R,
        connect: C,
    ) -> io::Result<Self>
    where
        R: FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
        C: FnMut(SocketAddr, Option<StdDuration>) -> io::Result<StdTcpStream> + Send + 'static,
    {
        let pool = match blocking_io_pool() {
            Ok(pool) => pool,
            Err(error) => raise_blocking_io_pool_startup_failure(error),
        };
        Self::connect_with_deadline_and_operations_on_pool(
            address,
            deadline,
            cancellation,
            pool,
            resolve,
            connect,
        )
    }

    fn connect_with_deadline_and_operations_on_pool<R, C>(
        address: &str,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
        pool: &Arc<BlockingIoPool>,
        resolve: R,
        connect: C,
    ) -> io::Result<Self>
    where
        R: FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
        C: FnMut(SocketAddr, Option<StdDuration>) -> io::Result<StdTcpStream> + Send + 'static,
    {
        let address = address.to_string();
        let stream = run_blocking_io_with_deadline_on_pool(
            move || {
                let addresses = resolve(address.clone())?;
                connect_resolved_tcp_candidates_with_clock(
                    &address,
                    addresses,
                    deadline,
                    Instant::now,
                    connect,
                )
            },
            deadline,
            cancellation,
            pool,
        )?;
        Self::from_std(stream)
    }

    pub(crate) fn read_all(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<String> {
        io_decode_utf8(&self.read_bytes_all(timeout, cancellation)?)
    }

    pub(crate) fn read_bytes_all(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Vec<u8>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            read_all_with_deadline(stream, deadline_from_timeout(timeout)?, cancellation)
        }
        #[cfg(not(unix))]
        {
            let mut contents = Vec::new();
            let deadline = deadline_from_timeout(timeout)?;
            loop {
                stream.set_read_timeout(next_wait_slice(deadline, cancellation)?)?;
                let mut chunk = [0u8; 4096];
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(bytes) => {
                        push_limited_bytes(&mut contents, &chunk[..bytes], "network read_all")?
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        if let Some(deadline) = deadline {
                            if Instant::now() >= deadline {
                                return Err(timeout_resource_error());
                            }
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(contents)
        }
    }

    pub(crate) fn read_line(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<String>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        read_line_with_deadline(stream, deadline_from_timeout(timeout)?, cancellation)
    }

    pub(crate) fn read_bytes(
        &self,
        max_bytes: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        read_some_with_deadline(
            stream,
            max_bytes,
            deadline_from_timeout(timeout)?,
            cancellation,
        )
    }

    pub(crate) fn read_exact(
        &self,
        count: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Vec<u8>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        read_exact_with_deadline(stream, count, deadline_from_timeout(timeout)?, cancellation)
    }

    pub(crate) fn write_all(
        &self,
        text: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        self.write_bytes(text.as_bytes(), timeout, cancellation)
    }

    pub(crate) fn write_bytes(
        &self,
        bytes: &[u8],
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            write_all_with_deadline(stream, bytes, deadline_from_timeout(timeout)?, cancellation)
        }
        #[cfg(not(unix))]
        {
            stream.set_write_timeout(next_wait_slice(
                deadline_from_timeout(timeout)?,
                cancellation,
            )?)?;
            stream.write_all(bytes)
        }
    }

    pub(crate) fn flush(&self) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        stream.flush()
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        Ok(stream.local_addr()?.to_string())
    }

    pub(crate) fn peer_addr(&self) -> io::Result<String> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        Ok(stream.peer_addr()?.to_string())
    }

    pub(crate) fn close(&self) {
        let mut stream = lock_mutex(&self.inner.stream);
        if let Some(stream) = stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn shutdown_read(&self) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        stream.shutdown(Shutdown::Read)
    }

    pub(crate) fn shutdown_write(&self) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        stream.shutdown(Shutdown::Write)
    }

    pub(crate) fn shutdown_both(&self) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        stream.shutdown(Shutdown::Both)
    }
}

impl UdpDatagramValue {
    pub(crate) fn address(&self) -> String {
        self.address.clone()
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    pub(crate) fn text(&self) -> io::Result<String> {
        io_decode_utf8(&self.data)
    }
}

impl UdpSocketValue {
    fn from_std(socket: StdUdpSocket) -> io::Result<Self> {
        #[cfg(unix)]
        socket.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(UdpSocketState {
                socket: Mutex::new(Some(socket)),
            }),
        })
    }

    pub(crate) fn bind(address: &str) -> io::Result<Self> {
        let address = address.to_string();
        let socket = run_blocking_io_with_deadline(
            move || StdUdpSocket::bind(address),
            None,
            current_lightweight_task_cancellation().as_ref(),
        )?;
        Self::from_std(socket)
    }

    pub(crate) fn send_to_text(
        &self,
        address: &str,
        text: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        self.send_to_bytes(address, text.as_bytes(), timeout, cancellation)
    }

    pub(crate) fn send_to_bytes(
        &self,
        address: &str,
        bytes: &[u8],
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        let deadline = deadline_from_timeout(timeout)?;
        let addresses = resolve_socket_addresses_before(address, deadline, cancellation)?;
        let target = addresses.into_iter().next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{address}` did not resolve to any socket addresses"),
            )
        })?;
        let mut socket = lock_mutex(&self.inner.socket);
        let Some(socket) = socket.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            loop {
                match socket.send_to(bytes, target) {
                    Ok(_) => return Ok(()),
                    Err(error) if is_retryable_network_error(&error) => {
                        wait_for_fd_event(
                            socket.as_raw_fd(),
                            libc::POLLOUT,
                            deadline,
                            cancellation,
                        )?;
                    }
                    Err(error) => return Err(normalize_udp_send_error(error)),
                }
            }
        }
        #[cfg(not(unix))]
        {
            socket.set_write_timeout(next_wait_slice(deadline, cancellation)?)?;
            socket
                .send_to(bytes, target)
                .map_err(normalize_udp_send_error)?;
            Ok(())
        }
    }

    pub(crate) fn recv(
        &self,
        max_bytes: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut socket = lock_mutex(&self.inner.socket);
        let Some(socket) = socket.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            let deadline = deadline_from_timeout(timeout)?;
            let mut buffer = vec![0u8; validate_udp_datagram_limit(max_bytes)?];
            loop {
                match socket.recv(&mut buffer) {
                    Ok(0) => return Ok(None),
                    Ok(bytes) => {
                        buffer.truncate(bytes);
                        return Ok(Some(buffer));
                    }
                    Err(error) if is_retryable_network_error(&error) => {
                        match wait_for_fd_event(
                            socket.as_raw_fd(),
                            libc::POLLIN,
                            deadline,
                            cancellation,
                        ) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                                return Ok(None);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        #[cfg(not(unix))]
        {
            socket.set_read_timeout(next_wait_slice(
                deadline_from_timeout(timeout)?,
                cancellation,
            )?)?;
            let mut buffer = vec![0u8; validate_udp_datagram_limit(max_bytes)?];
            match socket.recv(&mut buffer) {
                Ok(0) => Ok(None),
                Ok(bytes) => {
                    buffer.truncate(bytes);
                    Ok(Some(buffer))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }
    }

    pub(crate) fn recv_from(
        &self,
        max_bytes: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<UdpDatagramValue>> {
        let mut socket = lock_mutex(&self.inner.socket);
        let Some(socket) = socket.as_mut() else {
            return Err(closed_resource_error());
        };
        #[cfg(unix)]
        {
            let deadline = deadline_from_timeout(timeout)?;
            let mut buffer = vec![0u8; validate_udp_datagram_limit(max_bytes)?];
            loop {
                match socket.recv_from(&mut buffer) {
                    Ok((bytes, address)) => {
                        buffer.truncate(bytes);
                        return Ok(Some(UdpDatagramValue {
                            address: address.to_string(),
                            data: buffer,
                        }));
                    }
                    Err(error) if is_retryable_network_error(&error) => {
                        match wait_for_fd_event(
                            socket.as_raw_fd(),
                            libc::POLLIN,
                            deadline,
                            cancellation,
                        ) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                                return Ok(None);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        #[cfg(not(unix))]
        {
            socket.set_read_timeout(next_wait_slice(
                deadline_from_timeout(timeout)?,
                cancellation,
            )?)?;
            let mut buffer = vec![0u8; validate_udp_datagram_limit(max_bytes)?];
            match socket.recv_from(&mut buffer) {
                Ok((bytes, address)) => {
                    buffer.truncate(bytes);
                    Ok(Some(UdpDatagramValue {
                        address: address.to_string(),
                        data: buffer,
                    }))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let socket = lock_mutex(&self.inner.socket);
        let Some(socket) = socket.as_ref() else {
            return Err(closed_resource_error());
        };
        Ok(socket.local_addr()?.to_string())
    }

    pub(crate) fn peer_addr(&self) -> io::Result<String> {
        let socket = lock_mutex(&self.inner.socket);
        let Some(socket) = socket.as_ref() else {
            return Err(closed_resource_error());
        };
        Ok(socket.peer_addr()?.to_string())
    }

    pub(crate) fn close(&self) {
        let mut socket = lock_mutex(&self.inner.socket);
        *socket = None;
    }
}

#[cfg(unix)]
impl UnixListenerValue {
    fn from_std(listener: StdUnixListener) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(UnixListenerState {
                listener: Mutex::new(Some(listener)),
            }),
        })
    }

    pub(crate) fn bind(path: &str) -> io::Result<Self> {
        let path = path.to_string();
        let listener = run_blocking_io_with_deadline(
            move || {
                match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "unix listener path already exists",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                StdUnixListener::bind(path)
            },
            None,
            current_lightweight_task_cancellation().as_ref(),
        )?;
        Self::from_std(listener)
    }

    pub(crate) fn accept(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<UnixStreamValue> {
        let mut listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_mut() else {
            return Err(closed_resource_error());
        };
        let deadline = deadline_from_timeout(timeout)?;
        loop {
            match listener.accept() {
                Ok((stream, _)) => return UnixStreamValue::from_std(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    wait_for_fd_event(listener.as_raw_fd(), libc::POLLIN, deadline, cancellation)?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn close(&self) {
        let mut listener = lock_mutex(&self.inner.listener);
        *listener = None;
    }
}

#[cfg(not(unix))]
impl UnixListenerValue {
    pub(crate) fn bind(_path: &str) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unix domain sockets are not supported on this platform",
        ))
    }
}

#[cfg(unix)]
impl UnixStreamValue {
    fn from_std(stream: StdUnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(UnixStreamState {
                stream: Mutex::new(Some(stream)),
            }),
        })
    }

    pub(crate) fn connect(
        path: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::connect_with_deadline_and_operation(
            path,
            deadline_from_timeout(timeout)?,
            cancellation,
            StdUnixStream::connect,
        )
    }

    #[cfg(test)]
    fn connect_with_operation<C>(
        path: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
        connect: C,
    ) -> io::Result<Self>
    where
        C: FnOnce(String) -> io::Result<StdUnixStream> + Send + 'static,
    {
        Self::connect_with_deadline_and_operation(
            path,
            deadline_from_timeout(timeout)?,
            cancellation,
            connect,
        )
    }

    fn connect_with_deadline_and_operation<C>(
        path: &str,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
        connect: C,
    ) -> io::Result<Self>
    where
        C: FnOnce(String) -> io::Result<StdUnixStream> + Send + 'static,
    {
        let path = path.to_string();
        let stream = run_blocking_io_with_deadline(move || connect(path), deadline, cancellation)?;
        Self::from_std(stream)
    }

    pub(crate) fn read_line(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<String>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        read_line_with_deadline(stream, deadline_from_timeout(timeout)?, cancellation)
    }

    pub(crate) fn read_exact(
        &self,
        count: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Vec<u8>> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        read_exact_with_deadline(stream, count, deadline_from_timeout(timeout)?, cancellation)
    }

    pub(crate) fn write_all(
        &self,
        text: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        let mut stream = lock_mutex(&self.inner.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(closed_resource_error());
        };
        write_all_with_deadline(
            stream,
            text.as_bytes(),
            deadline_from_timeout(timeout)?,
            cancellation,
        )
    }

    pub(crate) fn close(&self) {
        let mut stream = lock_mutex(&self.inner.stream);
        *stream = None;
    }
}

#[cfg(not(unix))]
impl UnixStreamValue {
    pub(crate) fn connect(
        _path: &str,
        _timeout: Option<StdDuration>,
        _cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unix domain sockets are not supported on this platform",
        ))
    }
}

impl TlsListenerValue {
    pub(crate) fn bind(address: &str, cert_pem_path: &str, key_pem_path: &str) -> io::Result<Self> {
        let address = address.to_string();
        let cert_pem_path = cert_pem_path.to_string();
        let key_pem_path = key_pem_path.to_string();
        let cancellation = current_lightweight_task_cancellation();
        let (listener, cert_pem, key_pem) = run_blocking_io_with_deadline(
            move || {
                Ok((
                    StdTcpListener::bind(address)?,
                    read_tls_config_file(&cert_pem_path, "TLS certificate PEM")?,
                    read_tls_config_file(&key_pem_path, "TLS private key PEM")?,
                ))
            },
            None,
            cancellation.as_ref(),
        )?;
        let config = run_protocol_step_before(
            move || build_tls_server_config(cert_pem, key_pem),
            None,
            cancellation.as_ref(),
        )?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(TlsListenerState {
                listener: Mutex::new(Some(listener)),
                config,
            }),
        })
    }

    pub(crate) fn accept(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<TlsStreamValue> {
        let deadline = deadline_from_timeout(timeout)?;
        let mut pending = VecDeque::new();
        loop {
            #[cfg(unix)]
            let (listener_fd, accepted) = {
                let mut listener = lock_mutex(&self.inner.listener);
                let Some(listener) = listener.as_mut() else {
                    return Err(closed_resource_error());
                };
                let listener_fd = listener.as_raw_fd();
                let mut accepted = Vec::new();
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => accepted.push(stream),
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                (listener_fd, accepted)
            };
            #[cfg(unix)]
            for stream in accepted {
                let config = self.inner.config.clone();
                let handshake_deadline = tls_handshake_deadline(deadline)?;
                pending.push_back(run_protocol_step_before(
                    move || {
                        stream.set_nonblocking(true)?;
                        let connection = ServerConnection::new(config).map_err(io::Error::other)?;
                        Ok(PendingTlsServerHandshake {
                            stream: rustls::StreamOwned::new(connection, stream),
                            deadline: handshake_deadline,
                        })
                    },
                    deadline,
                    cancellation,
                )?);
            }
            #[cfg(not(unix))]
            let (wait_listener, accepted) = {
                let mut listener = lock_mutex(&self.inner.listener);
                let Some(listener) = listener.as_mut() else {
                    return Err(closed_resource_error());
                };
                let wait_listener = listener.try_clone()?;
                let mut accepted = Vec::new();
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => accepted.push(stream),
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                (wait_listener, accepted)
            };
            #[cfg(not(unix))]
            for stream in accepted {
                let config = self.inner.config.clone();
                let handshake_deadline = tls_handshake_deadline(deadline)?;
                pending.push_back(run_protocol_step_before(
                    move || {
                        stream.set_nonblocking(true)?;
                        let connection = ServerConnection::new(config).map_err(io::Error::other)?;
                        Ok(PendingTlsServerHandshake {
                            stream: rustls::StreamOwned::new(connection, stream),
                            deadline: handshake_deadline,
                        })
                    },
                    deadline,
                    cancellation,
                )?);
            }

            let pending_count = pending.len();
            for _ in 0..pending_count {
                let Some(mut handshake) = pending.pop_front() else {
                    break;
                };
                match advance_tls_server_handshake(
                    handshake.stream,
                    handshake.deadline,
                    cancellation,
                ) {
                    Ok((mut stream, true)) => {
                        finalize_tls_server_stream_for_runtime(&mut stream)?;
                        return Ok(TlsStreamValue {
                            inner: Arc::new(TlsStreamState {
                                stream: Mutex::new(Some(TlsStreamKind::Server(stream))),
                                busy: AtomicBool::new(false),
                                closed: AtomicBool::new(false),
                            }),
                        });
                    }
                    Ok((stream, false)) => {
                        handshake.stream = stream;
                        pending.push_back(handshake);
                    }
                    Err(error) => {
                        if cancellation.is_some_and(CancellationContext::is_cancelled) {
                            return Err(error);
                        }
                    }
                }
            }

            #[cfg(unix)]
            wait_for_tls_listener_progress(
                listener_fd,
                pending.is_empty(),
                deadline,
                cancellation,
            )?;
            #[cfg(not(unix))]
            wait_for_tls_listener_progress(
                &wait_listener,
                pending.is_empty(),
                deadline,
                cancellation,
            )?;
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_ref() else {
            return Err(closed_resource_error());
        };
        Ok(listener.local_addr()?.to_string())
    }

    pub(crate) fn close(&self) {
        let mut listener = lock_mutex(&self.inner.listener);
        *listener = None;
    }
}

fn complete_tls_client_handshake(
    mut stream: rustls::StreamOwned<ClientConnection, StdTcpStream>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<rustls::StreamOwned<ClientConnection, StdTcpStream>> {
    while stream.conn.is_handshaking() {
        check_deadline_and_cancellation(deadline, cancellation)?;
        let (returned, result) = run_protocol_state_step(
            stream,
            |stream| stream.conn.complete_io(&mut stream.sock),
            deadline,
            cancellation,
        )?;
        stream = returned;
        let result = match result {
            Ok(result) => result,
            Err(error) => Err(error),
        };
        check_deadline_and_cancellation(deadline, cancellation)?;
        match result {
            Ok(_) => {}
            Err(error) if is_retryable_network_error(&error) => {
                #[cfg(unix)]
                wait_for_fd_event(
                    stream.sock.as_raw_fd(),
                    libc::POLLIN | libc::POLLOUT,
                    deadline,
                    cancellation,
                )?;
                #[cfg(not(unix))]
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(stream)
}

fn advance_tls_server_handshake(
    mut stream: rustls::StreamOwned<ServerConnection, StdTcpStream>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(rustls::StreamOwned<ServerConnection, StdTcpStream>, bool)> {
    while stream.conn.is_handshaking() {
        check_deadline_and_cancellation(deadline, cancellation)?;
        let (returned, result) = run_protocol_state_step(
            stream,
            |stream| stream.conn.complete_io(&mut stream.sock),
            deadline,
            cancellation,
        )?;
        stream = returned;
        let result = match result {
            Ok(result) => result,
            Err(error) => Err(error),
        };
        check_deadline_and_cancellation(deadline, cancellation)?;
        match result {
            Ok(_) => {}
            Err(error) if is_retryable_network_error(&error) => return Ok((stream, false)),
            Err(error) => return Err(error),
        }
    }
    Ok((stream, true))
}

fn finalize_tls_server_stream_for_runtime(
    _stream: &mut rustls::StreamOwned<ServerConnection, StdTcpStream>,
) -> io::Result<()> {
    #[cfg(not(unix))]
    _stream.sock.set_nonblocking(false)?;
    Ok(())
}

fn tls_stream_raw_fd(stream: &TlsStreamKind) -> i32 {
    match stream {
        TlsStreamKind::Client(stream) => stream.sock.as_raw_fd(),
        TlsStreamKind::Server(stream) => stream.sock.as_raw_fd(),
    }
}

fn tls_read_step(
    stream: TlsStreamKind,
    max_bytes: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(TlsStreamKind, io::Result<Vec<u8>>)> {
    let (stream, result) = run_protocol_state_step(
        stream,
        move |stream| {
            let mut bytes = vec![0u8; max_bytes];
            let count = match stream {
                TlsStreamKind::Client(stream) => stream.read(&mut bytes),
                TlsStreamKind::Server(stream) => stream.read(&mut bytes),
            }?;
            bytes.truncate(count);
            Ok(bytes)
        },
        deadline,
        cancellation,
    )?;
    Ok((
        stream,
        match result {
            Ok(result) => result,
            Err(error) => Err(error),
        },
    ))
}

fn tls_write_step(
    stream: TlsStreamKind,
    bytes: Vec<u8>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(TlsStreamKind, io::Result<usize>)> {
    let (stream, result) = run_protocol_state_step(
        stream,
        move |stream| match stream {
            TlsStreamKind::Client(stream) => stream.write(&bytes),
            TlsStreamKind::Server(stream) => stream.write(&bytes),
        },
        deadline,
        cancellation,
    )?;
    Ok((
        stream,
        match result {
            Ok(result) => result,
            Err(error) => Err(error),
        },
    ))
}

fn tls_client_read_some_owned(
    mut stream: TlsClientStream,
    max_bytes: usize,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(TlsClientStream, Option<Vec<u8>>)> {
    loop {
        check_deadline_and_cancellation(deadline, cancellation)?;
        let (returned, result) = tls_read_step(
            TlsStreamKind::Client(stream),
            max_bytes,
            deadline,
            cancellation,
        )?;
        let TlsStreamKind::Client(returned) = returned else {
            unreachable!("TLS client read step returned a server stream")
        };
        stream = returned;
        check_deadline_and_cancellation(deadline, cancellation)?;
        match result {
            Ok(bytes) if bytes.is_empty() => return Ok((stream, None)),
            Ok(bytes) => return Ok((stream, Some(bytes))),
            Err(error) if is_retryable_network_error(&error) => wait_for_fd_event(
                stream.sock.as_raw_fd(),
                libc::POLLIN | libc::POLLOUT,
                deadline,
                cancellation,
            )?,
            Err(error) => return Err(error),
        }
    }
}

fn write_all_tls_client_owned(
    mut stream: rustls::StreamOwned<ClientConnection, StdTcpStream>,
    bytes: &[u8],
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<rustls::StreamOwned<ClientConnection, StdTcpStream>> {
    let mut written = 0usize;
    while written < bytes.len() {
        check_deadline_and_cancellation(deadline, cancellation)?;
        let pending = bytes[written..].to_vec();
        let (returned, result) = tls_write_step(
            TlsStreamKind::Client(stream),
            pending,
            deadline,
            cancellation,
        )?;
        let TlsStreamKind::Client(returned) = returned else {
            unreachable!("TLS client write step returned a server stream")
        };
        stream = returned;
        check_deadline_and_cancellation(deadline, cancellation)?;
        match result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "stream ended before enough bytes were written",
                ));
            }
            Ok(count) => written += count,
            Err(error) if is_retryable_network_error(&error) => wait_for_fd_event(
                stream.sock.as_raw_fd(),
                libc::POLLIN | libc::POLLOUT,
                deadline,
                cancellation,
            )?,
            Err(error) => return Err(error),
        }
    }
    Ok(stream)
}

fn read_http_response_tls_client_owned(
    mut stream: rustls::StreamOwned<ClientConnection, StdTcpStream>,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationContext>,
) -> io::Result<(
    rustls::StreamOwned<ClientConnection, StdTcpStream>,
    HttpResponseValue,
)> {
    let mut buffer = Vec::new();
    let (header_len, status, reason, headers, framing) = loop {
        let parsed = parse_http_response_head_step(&buffer, deadline, cancellation)?;
        check_deadline_and_cancellation(deadline, cancellation)?;
        if let Some(parsed) = parsed {
            break parsed;
        }
        let (returned, chunk) = tls_client_read_some_owned(stream, 4096, deadline, cancellation)?;
        stream = returned;
        let Some(chunk) = chunk else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream closed before a complete HTTP response was received",
            ));
        };
        push_http_chunk_with_limit(&mut buffer, &chunk, MAX_HTTP_MESSAGE_BYTES)?;
    };

    let body = match framing {
        HttpBodyFraming::ContentLength(content_length) => {
            if header_len.saturating_add(content_length) > MAX_HTTP_MESSAGE_BYTES {
                return Err(http_message_too_large_error_with_limit(
                    MAX_HTTP_MESSAGE_BYTES,
                ));
            }
            while buffer.len() < header_len.saturating_add(content_length) {
                let (returned, chunk) =
                    tls_client_read_some_owned(stream, 4096, deadline, cancellation)?;
                stream = returned;
                let Some(chunk) = chunk else {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream closed before the HTTP response body was fully received",
                    ));
                };
                push_http_chunk_with_limit(&mut buffer, &chunk, MAX_HTTP_MESSAGE_BYTES)?;
            }
            buffer[header_len..header_len.saturating_add(content_length)].to_vec()
        }
        HttpBodyFraming::Chunked => loop {
            let decoded = decode_chunked_http_body_step(
                &buffer,
                header_len,
                MAX_HTTP_MESSAGE_BYTES,
                deadline,
                cancellation,
            )?;
            check_deadline_and_cancellation(deadline, cancellation)?;
            if let Some(body) = decoded {
                break body;
            }
            let (returned, chunk) =
                tls_client_read_some_owned(stream, 4096, deadline, cancellation)?;
            stream = returned;
            let Some(chunk) = chunk else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before the chunked HTTP response body was fully received",
                ));
            };
            push_http_chunk_with_limit(&mut buffer, &chunk, MAX_HTTP_MESSAGE_BYTES)?;
        },
        HttpBodyFraming::UntilClose => loop {
            let (returned, chunk) =
                tls_client_read_some_owned(stream, 4096, deadline, cancellation)?;
            stream = returned;
            let Some(chunk) = chunk else {
                break buffer[header_len..].to_vec();
            };
            push_http_chunk_with_limit(&mut buffer, &chunk, MAX_HTTP_MESSAGE_BYTES)?;
        },
    };

    Ok((stream, parse_http_response(status, reason, headers, body)))
}

impl TlsStreamValue {
    fn take_stream_for_step(&self) -> io::Result<TlsStreamKind> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(closed_resource_error());
        }
        lock_mutex(&self.inner.stream)
            .take()
            .ok_or_else(|| io::Error::other("TLS stream state is temporarily unavailable"))
    }

    fn restore_stream_after_step(&self, stream: TlsStreamKind) {
        *lock_mutex(&self.inner.stream) = Some(stream);
    }

    pub(crate) fn connect(
        address: &str,
        server_name: &str,
        ca_pem_path: Option<&str>,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::connect_before(
            address,
            server_name,
            ca_pem_path,
            deadline_from_timeout(timeout)?,
            cancellation,
        )
    }

    fn connect_before(
        address: &str,
        server_name: &str,
        ca_pem_path: Option<&str>,
        deadline: Option<Instant>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        let ca_pem = ca_pem_path
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = path.to_string();
                run_blocking_io_with_deadline(
                    move || read_tls_config_file(&path, "TLS CA PEM"),
                    deadline,
                    cancellation,
                )
            })
            .transpose()?;
        let tcp = TcpStreamValue::connect_before(address, deadline, cancellation)?;
        let mut guard = lock_mutex(&tcp.inner.stream);
        let Some(stream) = guard.take() else {
            return Err(closed_resource_error());
        };
        drop(guard);
        let server_name = server_name.to_string();
        let stream = run_protocol_step_before(
            move || {
                ensure_rustls_crypto_provider();
                let config = ClientConfig::builder()
                    .with_root_certificates(build_tls_root_store(ca_pem)?)
                    .with_no_client_auth();
                let server_name = ServerName::try_from(server_name).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name")
                })?;
                let connection = ClientConnection::new(Arc::new(config), server_name)
                    .map_err(io::Error::other)?;
                Ok(rustls::StreamOwned::new(connection, stream))
            },
            deadline,
            cancellation,
        )?;
        let stream =
            complete_tls_client_handshake(stream, tls_handshake_deadline(deadline)?, cancellation)?;
        Ok(Self {
            inner: Arc::new(TlsStreamState {
                stream: Mutex::new(Some(TlsStreamKind::Client(stream))),
                busy: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn read_line(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Option<String>> {
        let deadline = deadline_from_timeout(timeout)?;
        let _operation = acquire_protocol_operation(&self.inner.busy, deadline, cancellation)?;
        let mut buffer = Vec::new();
        loop {
            check_deadline_and_cancellation(deadline, cancellation)?;
            let stream = self.take_stream_for_step()?;
            let (stream, result) = tls_read_step(stream, 1, deadline, cancellation)?;
            let fd = tls_stream_raw_fd(&stream);
            self.restore_stream_after_step(stream);
            check_deadline_and_cancellation(deadline, cancellation)?;
            match result {
                Ok(bytes) if bytes.is_empty() && buffer.is_empty() => return Ok(None),
                Ok(bytes) if bytes.is_empty() => {
                    return String::from_utf8(buffer)
                        .map(trim_line_endings)
                        .map(Some)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
                }
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    if buffer.last() == Some(&b'\n') {
                        return String::from_utf8(buffer)
                            .map(trim_line_endings)
                            .map(Some)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
                    }
                }
                Err(error) if is_retryable_network_error(&error) => {
                    wait_for_fd_event(fd, libc::POLLIN | libc::POLLOUT, deadline, cancellation)?
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn read_exact(
        &self,
        count: usize,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Vec<u8>> {
        let deadline = deadline_from_timeout(timeout)?;
        let _operation = acquire_protocol_operation(&self.inner.busy, deadline, cancellation)?;
        let mut output = Vec::with_capacity(count);
        while output.len() < count {
            check_deadline_and_cancellation(deadline, cancellation)?;
            let stream = self.take_stream_for_step()?;
            let (stream, result) =
                tls_read_step(stream, count - output.len(), deadline, cancellation)?;
            let fd = tls_stream_raw_fd(&stream);
            self.restore_stream_after_step(stream);
            check_deadline_and_cancellation(deadline, cancellation)?;
            match result {
                Ok(bytes) if bytes.is_empty() => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream ended before enough bytes were read",
                    ));
                }
                Ok(bytes) => output.extend_from_slice(&bytes),
                Err(error) if is_retryable_network_error(&error) => {
                    wait_for_fd_event(fd, libc::POLLIN | libc::POLLOUT, deadline, cancellation)?
                }
                Err(error) => return Err(error),
            }
        }
        Ok(output)
    }

    pub(crate) fn write_all(
        &self,
        text: &str,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<()> {
        let deadline = deadline_from_timeout(timeout)?;
        let _operation = acquire_protocol_operation(&self.inner.busy, deadline, cancellation)?;
        let bytes = text.as_bytes();
        let mut written = 0usize;
        while written < bytes.len() {
            check_deadline_and_cancellation(deadline, cancellation)?;
            let stream = self.take_stream_for_step()?;
            let pending = bytes[written..].to_vec();
            let (stream, result) = tls_write_step(stream, pending, deadline, cancellation)?;
            let fd = tls_stream_raw_fd(&stream);
            self.restore_stream_after_step(stream);
            check_deadline_and_cancellation(deadline, cancellation)?;
            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "stream ended before enough bytes were written",
                    ));
                }
                Ok(count) => written += count,
                Err(error) if is_retryable_network_error(&error) => {
                    wait_for_fd_event(fd, libc::POLLIN | libc::POLLOUT, deadline, cancellation)?
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        let Ok(_operation) = acquire_protocol_operation(&self.inner.busy, None, None) else {
            return;
        };
        let Ok(stream) = self.take_stream_for_step() else {
            return;
        };
        self.inner.closed.store(true, Ordering::Release);
        let _ = run_protocol_step(move || {
            match stream {
                TlsStreamKind::Client(mut stream) => {
                    stream.conn.send_close_notify();
                    let _ = stream.conn.complete_io(&mut stream.sock);
                    let _ = stream.sock.shutdown(Shutdown::Both);
                }
                TlsStreamKind::Server(mut stream) => {
                    stream.conn.send_close_notify();
                    let _ = stream.conn.complete_io(&mut stream.sock);
                    let _ = stream.sock.shutdown(Shutdown::Both);
                }
            }
            Ok(())
        });
    }
}

impl HttpListenerValue {
    pub(crate) fn bind(address: &str) -> io::Result<Self> {
        let address = address.to_string();
        let listener = run_blocking_io_with_deadline(
            move || StdTcpListener::bind(address),
            None,
            current_lightweight_task_cancellation().as_ref(),
        )?;
        #[cfg(unix)]
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(HttpListenerState {
                listener: Mutex::new(Some(listener)),
            }),
        })
    }

    pub(crate) fn accept(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<HttpExchangeValue> {
        let deadline = deadline_from_timeout(timeout)?;
        loop {
            #[cfg(unix)]
            let stream = {
                let mut listener = lock_mutex(&self.inner.listener);
                let Some(listener) = listener.as_mut() else {
                    return Err(closed_resource_error());
                };
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => break TcpStreamValue::from_std(stream)?,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            wait_for_fd_event(
                                listener.as_raw_fd(),
                                libc::POLLIN,
                                deadline,
                                cancellation,
                            )?;
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            #[cfg(not(unix))]
            let stream = {
                let mut listener = lock_mutex(&self.inner.listener);
                let Some(listener) = listener.as_mut() else {
                    return Err(closed_resource_error());
                };
                TcpStreamValue::from_std(listener.accept()?.0)?
            };
            let mut raw_stream_guard = lock_mutex(&stream.inner.stream);
            let Some(mut raw_stream) = raw_stream_guard.take() else {
                return Err(closed_resource_error());
            };
            drop(raw_stream_guard);
            let request = read_http_request_from_stream(&mut raw_stream, deadline, cancellation);
            *lock_mutex(&stream.inner.stream) = Some(raw_stream);
            let (method, path, headers, body) = match request {
                Ok(request) => request,
                Err(error)
                    if is_http_message_too_large_error(&error)
                        || is_http_headers_too_large_error(&error) =>
                {
                    let status = if is_http_message_too_large_error(&error) {
                        413
                    } else {
                        431
                    };
                    let raw_stream = lock_mutex(&stream.inner.stream).take();
                    if let Some(mut raw_stream) = raw_stream {
                        let _ = write_http_response_to_stream(
                            &mut raw_stream,
                            status,
                            Vec::new(),
                            b"",
                            deadline,
                            cancellation,
                        );
                    }
                    stream.close();
                    continue;
                }
                Err(error) if is_http_bad_request_error(&error) => {
                    let raw_stream = lock_mutex(&stream.inner.stream).take();
                    if let Some(mut raw_stream) = raw_stream {
                        let _ = write_http_response_to_stream(
                            &mut raw_stream,
                            400,
                            Vec::new(),
                            b"",
                            deadline,
                            cancellation,
                        );
                    }
                    stream.close();
                    continue;
                }
                Err(error) => return Err(error),
            };
            return Ok(HttpExchangeValue {
                inner: Arc::new(HttpExchangeState {
                    stream: Mutex::new(Some(stream)),
                    method,
                    path,
                    headers,
                    body,
                }),
            });
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_ref() else {
            return Err(closed_resource_error());
        };
        Ok(listener.local_addr()?.to_string())
    }

    pub(crate) fn close(&self) {
        let mut listener = lock_mutex(&self.inner.listener);
        *listener = None;
    }
}

impl HttpExchangeValue {
    pub(crate) fn method(&self) -> String {
        self.inner.method.clone()
    }

    pub(crate) fn path(&self) -> String {
        self.inner.path.clone()
    }

    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        self.inner.headers.clone()
    }

    pub(crate) fn body_text(&self) -> io::Result<String> {
        io_decode_utf8(&self.inner.body)
    }

    pub(crate) fn body_bytes(&self) -> Vec<u8> {
        self.inner.body.clone()
    }

    pub(crate) fn respond_text(
        &self,
        status: i32,
        body: &str,
        headers: Vec<(String, String)>,
    ) -> io::Result<()> {
        self.respond_bytes(status, body.as_bytes(), headers)
    }

    pub(crate) fn respond_bytes(
        &self,
        status: i32,
        body: &[u8],
        headers: Vec<(String, String)>,
    ) -> io::Result<()> {
        let mut exchange_stream = lock_mutex(&self.inner.stream);
        let Some(stream) = exchange_stream.take() else {
            return Err(closed_resource_error());
        };
        drop(exchange_stream);
        let mut raw_stream_guard = lock_mutex(&stream.inner.stream);
        let Some(mut raw_stream) = raw_stream_guard.take() else {
            return Err(closed_resource_error());
        };
        drop(raw_stream_guard);
        let result =
            write_http_response_to_stream(&mut raw_stream, status, headers, body, None, None);
        stream.close();
        result
    }
}

impl HttpResponseValue {
    pub(crate) fn request_text(
        method: &str,
        url: &str,
        body: &str,
        headers: Vec<(String, String)>,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::request_bytes(method, url, body.as_bytes(), headers, timeout, cancellation)
    }

    #[cfg(test)]
    pub(crate) fn request_text_with_ca(
        method: &str,
        url: &str,
        body: &str,
        headers: Vec<(String, String)>,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
        ca_pem_path: &str,
    ) -> io::Result<Self> {
        Self::request_bytes_with_ca(
            method,
            url,
            body.as_bytes(),
            headers,
            timeout,
            cancellation,
            Some(ca_pem_path),
        )
    }

    pub(crate) fn request_bytes(
        method: &str,
        url: &str,
        body: &[u8],
        headers: Vec<(String, String)>,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<Self> {
        Self::request_bytes_with_ca(method, url, body, headers, timeout, cancellation, None)
    }

    fn request_bytes_with_ca(
        method: &str,
        url: &str,
        body: &[u8],
        headers: Vec<(String, String)>,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
        ca_pem_path: Option<&str>,
    ) -> io::Result<Self> {
        let deadline = deadline_from_timeout(timeout)?;
        let method = method.to_string();
        let url_text = url.to_string();
        let body = body.to_vec();
        let (url, host, request) = run_protocol_step_before(
            move || {
                let url = Url::parse(&url_text).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid URL `{url_text}`: {error}"),
                    )
                })?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                        "Aura HTTP requests require `http://` or `https://` URLs, found `{url}`"
                    ),
                    ));
                }
                let host = match url.host() {
                    Some(url::Host::Ipv6(host)) => {
                        format!("[{}]:{}", host, url.port_or_known_default().unwrap_or(80))
                    }
                    Some(url::Host::Ipv4(host)) => {
                        format!("{}:{}", host, url.port_or_known_default().unwrap_or(80))
                    }
                    Some(url::Host::Domain(host)) => {
                        format!("{}:{}", host, url.port_or_known_default().unwrap_or(80))
                    }
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid URL `{url}`: missing host"),
                        ))
                    }
                };
                let request = build_http_request_bytes(&method, &url, &body, headers)?;
                Ok((url, host, request))
            },
            deadline,
            cancellation,
        )?;
        if url.scheme() == "http" {
            let stream = TcpStreamValue::connect_before(&host, deadline, cancellation)?;
            let mut guard = lock_mutex(&stream.inner.stream);
            let Some(mut raw_stream) = guard.take() else {
                return Err(closed_resource_error());
            };
            drop(guard);
            let response = (|| {
                write_all_with_deadline(&mut raw_stream, &request, deadline, cancellation)?;
                read_http_response_from_stream(&mut raw_stream, deadline, cancellation)
            })();
            *lock_mutex(&stream.inner.stream) = Some(raw_stream);
            stream.close();
            return response;
        }

        #[cfg(unix)]
        {
            let server_name = url.host_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "HTTPS URL is missing a host")
            })?;
            let stream = TlsStreamValue::connect_before(
                &host,
                server_name,
                ca_pem_path,
                deadline,
                cancellation,
            )?;
            let mut guard = lock_mutex(&stream.inner.stream);
            let Some(TlsStreamKind::Client(raw_stream)) = guard.take() else {
                return Err(closed_resource_error());
            };
            drop(guard);
            let raw_stream =
                write_all_tls_client_owned(raw_stream, &request, deadline, cancellation)?;
            let (raw_stream, response) =
                read_http_response_tls_client_owned(raw_stream, deadline, cancellation)?;
            stream.restore_stream_after_step(TlsStreamKind::Client(raw_stream));
            stream.close();
            Ok(response)
        }
        #[cfg(not(unix))]
        {
            let _ = (ca_pem_path, deadline);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "HTTPS requests are supported on Aura's Unix preview platforms",
            ))
        }
    }

    pub(crate) fn status(&self) -> i32 {
        self.status
    }

    pub(crate) fn reason(&self) -> String {
        self.reason.clone()
    }

    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        self.headers.clone()
    }

    pub(crate) fn text(&self) -> io::Result<String> {
        io_decode_utf8(&self.body)
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.body.clone()
    }
}

impl WebSocketListenerValue {
    pub(crate) fn bind(address: &str) -> io::Result<Self> {
        let address = address.to_string();
        let listener = run_blocking_io_with_deadline(
            move || StdTcpListener::bind(address),
            None,
            current_lightweight_task_cancellation().as_ref(),
        )?;
        #[cfg(unix)]
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(WebSocketListenerState {
                listener: Mutex::new(Some(listener)),
            }),
        })
    }

    pub(crate) fn accept(&self, timeout: Option<StdDuration>) -> io::Result<WebSocketValue> {
        let listener = {
            let listener = lock_mutex(&self.inner.listener);
            let Some(listener) = listener.as_ref() else {
                return Err(closed_resource_error());
            };
            listener.try_clone()?
        };
        let deadline = deadline_from_timeout(timeout)?;
        let cancellation = current_lightweight_task_cancellation();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    #[cfg(unix)]
                    let mut state =
                        accept_websocket_stream(stream, deadline, cancellation.as_ref())?;
                    #[cfg(not(unix))]
                    let mut state = {
                        let socket =
                            match websocket_accept_with_config(stream, Some(websocket_config())) {
                                Ok(socket) => socket,
                                Err(tungstenite::handshake::HandshakeError::Interrupted(_)) => {
                                    return Err(io::Error::new(
                                        io::ErrorKind::WouldBlock,
                                        "websocket handshake unexpectedly blocked",
                                    ));
                                }
                                Err(tungstenite::handshake::HandshakeError::Failure(error)) => {
                                    return Err(websocket_error_to_io(error));
                                }
                            };
                        WebSocketStateKind::Plain(Box::new(socket))
                    };
                    #[cfg(unix)]
                    websocket_set_nonblocking(&mut state, true)?;
                    return Ok(WebSocketValue {
                        inner: Arc::new(WebSocketState {
                            socket: Mutex::new(Some(state)),
                            busy: AtomicBool::new(false),
                            closed: AtomicBool::new(false),
                        }),
                    });
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    wait_for_fd_event(
                        listener.as_raw_fd(),
                        libc::POLLIN,
                        deadline,
                        cancellation.as_ref(),
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<String> {
        let listener = lock_mutex(&self.inner.listener);
        let Some(listener) = listener.as_ref() else {
            return Err(closed_resource_error());
        };
        Ok(listener.local_addr()?.to_string())
    }
}

impl WebSocketValue {
    fn take_socket_for_step(&self) -> io::Result<WebSocketStateKind> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(closed_resource_error());
        }
        lock_mutex(&self.inner.socket)
            .take()
            .ok_or_else(|| io::Error::other("websocket state is temporarily unavailable"))
    }

    fn restore_socket_after_step(&self, socket: WebSocketStateKind) {
        *lock_mutex(&self.inner.socket) = Some(socket);
    }

    pub(crate) fn connect(url: &str, timeout: Option<StdDuration>) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let deadline = deadline_from_timeout(timeout)?;
            let cancellation = current_lightweight_task_cancellation();
            let url = url.to_string();
            let (request, address) = run_protocol_step_before(
                move || {
                    let parsed = url::Url::parse(&url).map_err(io::Error::other)?;
                    let request = websocket_client_request(&parsed)?;
                    let host = parsed.host_str().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "websocket URL is missing a host",
                        )
                    })?;
                    let port = parsed.port_or_known_default().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "websocket URL is missing a known port",
                        )
                    })?;
                    let address = if host.contains(':') && !host.starts_with('[') {
                        format!("[{host}]:{port}")
                    } else {
                        format!("{host}:{port}")
                    };
                    Ok((request, address))
                },
                deadline,
                cancellation.as_ref(),
            )?;
            let tcp = TcpStreamValue::connect_before(&address, deadline, cancellation.as_ref())?;
            let mut guard = lock_mutex(&tcp.inner.stream);
            let Some(stream) = guard.take() else {
                return Err(closed_resource_error());
            };
            drop(guard);
            let mut state =
                connect_websocket_stream(stream, request, deadline, cancellation.as_ref())?;
            websocket_set_nonblocking(&mut state, true)?;
            Ok(Self {
                inner: Arc::new(WebSocketState {
                    socket: Mutex::new(Some(state)),
                    busy: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                }),
            })
        }

        #[cfg(not(unix))]
        {
            let url = url.to_string();
            let deadline = deadline_from_timeout(timeout)?;
            let cancellation = current_lightweight_task_cancellation();
            let (socket, _) = run_blocking_io_with_deadline(
                move || tungstenite::connect(url).map_err(websocket_error_to_io),
                deadline,
                cancellation.as_ref(),
            )?;
            let state = WebSocketStateKind::MaybeTls(Box::new(socket));
            Ok(Self {
                inner: Arc::new(WebSocketState {
                    socket: Mutex::new(Some(state)),
                    busy: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                }),
            })
        }
    }

    pub(crate) fn send_text(&self, text: &str, timeout: Option<StdDuration>) -> io::Result<()> {
        self.send_message(Message::Text(text.to_string()), timeout)
    }

    pub(crate) fn send_bytes(&self, bytes: &[u8], timeout: Option<StdDuration>) -> io::Result<()> {
        self.send_message(Message::Binary(bytes.to_vec()), timeout)
    }

    #[allow(clippy::result_large_err)]
    fn send_message(&self, mut message: Message, timeout: Option<StdDuration>) -> io::Result<()> {
        let deadline = deadline_from_timeout(timeout)?;
        let cancellation = current_lightweight_task_cancellation();
        let _operation =
            acquire_protocol_operation(&self.inner.busy, deadline, cancellation.as_ref())?;
        loop {
            check_deadline_and_cancellation(deadline, cancellation.as_ref())?;
            let socket = self.take_socket_for_step()?;
            let ((returned_socket, returned_message), result) = run_protocol_state_step(
                (socket, message),
                |(socket, message)| match socket {
                    WebSocketStateKind::Plain(socket) => socket.send(message.clone()),
                    WebSocketStateKind::MaybeTls(socket) => socket.send(message.clone()),
                },
                deadline,
                cancellation.as_ref(),
            )?;
            message = returned_message;
            #[cfg(unix)]
            let fd = websocket_raw_fd(&returned_socket);
            self.restore_socket_after_step(returned_socket);
            check_deadline_and_cancellation(deadline, cancellation.as_ref())?;
            #[cfg(unix)]
            let fd = fd?;
            let result = result?;
            match result {
                Ok(()) => return Ok(()),
                Err(tungstenite::Error::WriteBufferFull(returned)) => {
                    message = returned;
                    #[cfg(unix)]
                    wait_for_fd_event(fd, libc::POLLOUT, deadline, cancellation.as_ref())?;
                    #[cfg(not(unix))]
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "websocket write buffer is full",
                    ));
                }
                Err(tungstenite::Error::Io(error)) if is_retryable_network_error(&error) => {
                    #[cfg(unix)]
                    wait_for_fd_event(fd, libc::POLLOUT, deadline, cancellation.as_ref())?;
                    #[cfg(not(unix))]
                    return Err(error);
                }
                Err(error) => return Err(websocket_error_to_io(error)),
            }
        }
    }

    pub(crate) fn recv_text(&self, timeout: Option<StdDuration>) -> io::Result<Option<String>> {
        let message = match self.recv_message(timeout)? {
            Some(message) => message,
            None => return Ok(None),
        };
        match message {
            Message::Text(text) => Ok(Some(text.to_string())),
            Message::Binary(bytes) => Ok(Some(io_decode_utf8(bytes.as_ref())?)),
            Message::Close(_) => Ok(None),
            _ => Ok(None),
        }
    }

    pub(crate) fn recv_bytes(&self, timeout: Option<StdDuration>) -> io::Result<Option<Vec<u8>>> {
        let message = match self.recv_message(timeout)? {
            Some(message) => message,
            None => return Ok(None),
        };
        match message {
            Message::Text(text) => Ok(Some(text.as_bytes().to_vec())),
            Message::Binary(bytes) => Ok(Some(bytes.to_vec())),
            Message::Close(_) => Ok(None),
            _ => Ok(None),
        }
    }

    #[allow(clippy::result_large_err)]
    fn recv_message(&self, timeout: Option<StdDuration>) -> io::Result<Option<Message>> {
        let deadline = deadline_from_timeout(timeout)?;
        let cancellation = current_lightweight_task_cancellation();
        let _operation =
            acquire_protocol_operation(&self.inner.busy, deadline, cancellation.as_ref())?;
        loop {
            check_deadline_and_cancellation(deadline, cancellation.as_ref())?;
            let socket = self.take_socket_for_step()?;
            let (socket, result) = run_protocol_state_step(
                socket,
                |socket| match socket {
                    WebSocketStateKind::Plain(socket) => socket.read(),
                    WebSocketStateKind::MaybeTls(socket) => socket.read(),
                },
                deadline,
                cancellation.as_ref(),
            )?;
            #[cfg(unix)]
            let fd = websocket_raw_fd(&socket);
            self.restore_socket_after_step(socket);
            check_deadline_and_cancellation(deadline, cancellation.as_ref())?;
            #[cfg(unix)]
            let fd = fd?;
            let result = result?;
            match result {
                Ok(Message::Close(_)) => return Ok(None),
                Ok(message) => return Ok(Some(message)),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(None);
                }
                Err(tungstenite::Error::Io(error)) if is_retryable_network_error(&error) => {
                    #[cfg(unix)]
                    wait_for_fd_event(
                        fd,
                        libc::POLLIN | libc::POLLOUT,
                        deadline,
                        cancellation.as_ref(),
                    )?;
                    #[cfg(not(unix))]
                    continue;
                }
                Err(error) => return Err(websocket_error_to_io(error)),
            }
        }
    }

    pub(crate) fn close(&self) -> io::Result<()> {
        let _operation = acquire_protocol_operation(&self.inner.busy, None, None)?;
        let socket = self.take_socket_for_step()?;
        self.inner.closed.store(true, Ordering::Release);
        let (_socket, result) = run_protocol_state_step(
            socket,
            |socket| match socket {
                WebSocketStateKind::Plain(socket) => {
                    socket.close(None).map_err(websocket_error_to_io)
                }
                WebSocketStateKind::MaybeTls(socket) => {
                    socket.close(None).map_err(websocket_error_to_io)
                }
            },
            None,
            None,
        )?;
        match result {
            Ok(result) => result,
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn io_read_line() -> io::Result<Option<String>> {
    let mut stdin = io::stdin().lock();
    let mut buffer = String::new();
    let bytes = stdin.read_line(&mut buffer)?;
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(trim_line_endings(buffer)))
}

impl CancellationContext {
    pub(crate) fn merged(&self, other: &CancellationContext) -> CancellationContext {
        let mut flags = self.flags.clone();
        flags.extend(other.flags.iter().cloned());
        CancellationContext { flags }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.flags.iter().any(|flag| flag.load(Ordering::SeqCst))
    }

    fn subscribe_reactor(&self, subscription: &ReactorSubscription) {
        for flag in &self.flags {
            flag.subscribe(subscription);
        }
    }

    fn unsubscribe_reactor(&self, subscription: &ReactorSubscription) {
        for flag in &self.flags {
            flag.unsubscribe(subscription);
        }
    }
}

pub(crate) fn poll_cancellation(cancellation: &CancellationContext) -> bool {
    if cancellation.is_cancelled() {
        return true;
    }
    let _ = yield_now_current_lightweight_task();
    cancellation.is_cancelled()
}

impl TaskGroupValue {
    pub(crate) fn new(parent: &CancellationContext) -> Self {
        Self {
            inner: Arc::new(TaskGroupState {
                tasks: Mutex::new(Vec::new()),
                cancel_flag: Arc::new(RuntimeWakeSignal::new(false)),
                failure_wake_flag: Arc::new(RuntimeWakeSignal::new(false)),
                completion_wake_flag: Arc::new(RuntimeWakeSignal::new(false)),
                parent_flags: parent.flags.clone(),
            }),
        }
    }

    pub(crate) fn child_cancellation(&self) -> CancellationContext {
        let mut flags = self.inner.parent_flags.clone();
        flags.push(self.inner.cancel_flag.clone());
        CancellationContext { flags }
    }

    pub(crate) fn queue_iteration_signal(&self) -> CancellationContext {
        CancellationContext {
            flags: vec![
                self.inner.cancel_flag.clone(),
                self.inner.failure_wake_flag.clone(),
                self.inner.completion_wake_flag.clone(),
            ],
        }
    }

    // Task-start adapters invoke this during prepared-task publication, before
    // the prepared task enters any worker inbox. A later drain therefore sees
    // the complete task set even when the selected worker is already idle.
    pub(crate) fn register_task(&self, task: TaskValue) {
        task.register_group_failure_wake_flag(self.inner.failure_wake_flag.clone());
        task.register_group_completion_wake_flag(self.inner.completion_wake_flag.clone());
        lock_mutex(&self.inner.tasks).push(task);
    }

    pub(crate) fn cancel(&self) {
        self.inner.cancel_flag.store(true, Ordering::SeqCst);
        notify_runtime_scheduler_if_started();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancel_flag.load(Ordering::SeqCst)
    }

    pub(crate) fn has_unobserved_error(&self) -> bool {
        lock_mutex(&self.inner.tasks)
            .iter()
            .any(|task| task.unobserved_error().is_some())
    }

    pub(crate) fn clear_failure_wake_if_no_unobserved_error(&self) {
        if !self.has_unobserved_error() {
            self.inner.failure_wake_flag.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn all_registered_tasks_completed(&self) -> bool {
        let tasks = lock_mutex(&self.inner.tasks);
        tasks.iter().all(|task| task.completed_result().is_some())
    }

    pub(crate) fn clear_completion_wake_if_tasks_still_running(&self) {
        if !self.all_registered_tasks_completed() {
            self.inner
                .completion_wake_flag
                .store(false, Ordering::SeqCst);
        }
    }

    // Invariant: callers drain only after they have finished registering tasks for the group.
    pub(crate) fn drain_tasks(&self) -> Vec<TaskValue> {
        let mut tasks = lock_mutex(&self.inner.tasks);
        std::mem::take(&mut *tasks)
    }
}

#[derive(Default)]
struct TaskReachabilitySearch {
    task_visiting: HashSet<usize>,
    task_memo: HashMap<usize, bool>,
    recv_channel_visiting: HashSet<(usize, bool)>,
    recv_channel_memo: HashMap<(usize, bool), bool>,
    send_channel_visiting: HashSet<usize>,
    send_channel_memo: HashMap<usize, bool>,
    channel_expansions: usize,
}

fn reachable_channel_candidate(
    tasks: impl IntoIterator<Item = TaskValue>,
    search: &mut TaskReachabilitySearch,
    seen: &mut HashSet<usize>,
) -> bool {
    tasks.into_iter().any(|task| {
        let identity = Arc::as_ptr(&task.inner) as usize;
        seen.insert(identity)
            && task.completed_result().is_none()
            && task.can_make_reachable_progress(search)
    })
}

fn recv_channel_has_reachable_waker(
    channel: &ChannelValue,
    ignore_closed: bool,
    search: &mut TaskReachabilitySearch,
) -> bool {
    let identity = Arc::as_ptr(&channel.inner) as usize;
    let key = (identity, ignore_closed);
    if let Some(reachable) = search.recv_channel_memo.get(&key) {
        return *reachable;
    }
    if !search.recv_channel_visiting.insert(key) {
        return false;
    }
    search.channel_expansions += 1;

    let mut reachable = channel.is_ready_for_scheduler_recv(ignore_closed);
    if !reachable {
        let mut seen = HashSet::new();
        reachable =
            reachable_channel_candidate(channel.registered_sender_tasks(), search, &mut seen);
        if !reachable && channel.is_open() {
            reachable =
                reachable_channel_candidate(channel.registered_task_handles(), search, &mut seen);
        }
        if !reachable {
            // A completing sender may have made the queue ready between the
            // initial state read and the weak-owner traversal.
            reachable = channel.is_ready_for_scheduler_recv(ignore_closed);
        }
    }

    search.recv_channel_visiting.remove(&key);
    search.recv_channel_memo.insert(key, reachable);
    reachable
}

fn send_channel_has_reachable_waker(
    channel: &ChannelValue,
    search: &mut TaskReachabilitySearch,
) -> bool {
    let identity = Arc::as_ptr(&channel.inner) as usize;
    if let Some(reachable) = search.send_channel_memo.get(&identity) {
        return *reachable;
    }
    if !search.send_channel_visiting.insert(identity) {
        return false;
    }
    search.channel_expansions += 1;

    let mut reachable = channel.is_ready_for_scheduler_send();
    if !reachable {
        #[cfg(test)]
        AFTER_TASK_GROUP_SEND_REACHABILITY_INITIAL_CHECK_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
        let mut seen = HashSet::new();
        reachable =
            reachable_channel_candidate(channel.registered_receiver_tasks(), search, &mut seen)
                || reachable_channel_candidate(
                    channel.registered_task_handles(),
                    search,
                    &mut seen,
                );
        if !reachable {
            // A completing receiver may have freed capacity between the
            // initial state read and the weak-owner traversal.
            reachable = channel.is_ready_for_scheduler_send();
        }
    }

    search.send_channel_visiting.remove(&identity);
    search.send_channel_memo.insert(identity, reachable);
    reachable
}

impl TaskValue {
    fn subscribe_reactor_completion(&self, subscription: &ReactorSubscription) {
        subscribe_reactor_target(&self.inner.completion_reactor_subscribers, subscription);
    }

    fn unsubscribe_reactor_completion(&self, subscription: &ReactorSubscription) {
        unsubscribe_reactor_target(&self.inner.completion_reactor_subscribers, subscription);
    }

    pub(crate) fn runtime_type_name(&self) -> Option<String> {
        lock_mutex(&self.inner.runtime_type_name).clone()
    }

    pub(crate) fn set_runtime_type_name(&self, runtime_type_name: String) {
        *lock_mutex(&self.inner.runtime_type_name) = Some(runtime_type_name);
    }

    fn register_group_failure_wake_flag(&self, flag: Arc<RuntimeWakeSignal>) {
        let mut flags = lock_mutex(&self.inner.group_failure_wake_flags);
        if !flags.iter().any(|existing| Arc::ptr_eq(existing, &flag)) {
            flags.push(flag.clone());
        }
        drop(flags);
        if self.unobserved_error().is_some() {
            flag.store(true, Ordering::SeqCst);
            notify_runtime_scheduler_if_started();
        }
    }

    fn register_group_completion_wake_flag(&self, flag: Arc<RuntimeWakeSignal>) {
        let mut flags = lock_mutex(&self.inner.group_completion_wake_flags);
        if !flags.iter().any(|existing| Arc::ptr_eq(existing, &flag)) {
            flags.push(flag.clone());
        }
        drop(flags);
        if self.completed_result().is_some() {
            flag.store(true, Ordering::SeqCst);
            notify_runtime_scheduler_if_started();
        }
    }

    fn observe_result(&self, result: &TaskExecutionResult) {
        if matches!(result, TaskExecutionResult::Ready(Err(_))) {
            self.inner.observed_failure.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn claim_result_observation(&self) -> Result<()> {
        if self.inner.result_is_repeatable {
            return Ok(());
        }
        self.inner
            .result_observation_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| {
                Diagnostic::coded(
                    "AU4001",
                    "task result has already been observed; non-repeatable task results allow exactly one observing attempt",
                )
            })
    }

    pub(crate) fn completed_result(&self) -> Option<TaskExecutionResult> {
        let state = lock_mutex(&self.inner.handle);
        match &*state {
            TaskHandle::Completed(result) => Some(result.clone()),
            TaskHandle::Running => None,
        }
    }

    pub(crate) fn completed_result_observed(&self) -> Option<TaskExecutionResult> {
        let result = self.completed_result()?;
        self.observe_result(&result);
        if let TaskExecutionResult::Ready(Ok(value)) = &result {
            register_current_task_queue_handles(value);
        }
        Some(result)
    }

    #[cfg(test)]
    pub(crate) fn from_handle(
        handle: thread::JoinHandle<std::result::Result<Value, Diagnostic>>,
    ) -> Self {
        Self::from_handle_with_result_repeatability(handle, true)
    }

    #[cfg(test)]
    pub(crate) fn from_handle_with_result_repeatability(
        handle: thread::JoinHandle<std::result::Result<Value, Diagnostic>>,
        result_is_repeatable: bool,
    ) -> Self {
        let inner = Arc::new(TaskState {
            handle: Mutex::new(TaskHandle::Running),
            ready: Condvar::new(),
            lightweight: false,
            result_is_repeatable,
            result_observation_claimed: AtomicBool::new(false),
            waits_without_deadline: AtomicBool::new(false),
            current_wait: Mutex::new(None),
            join_dependencies: Mutex::new(None),
            observed_failure: AtomicBool::new(false),
            completion_reactor_subscribers: Mutex::new(HashMap::new()),
            group_failure_wake_flags: Mutex::new(Vec::new()),
            group_completion_wake_flags: Mutex::new(Vec::new()),
            runtime_type_name: Mutex::new(None),
        });
        let state = inner.clone();
        thread::spawn(move || {
            let result = match handle.join() {
                Ok(result) => TaskExecutionResult::Ready(result),
                Err(_) => TaskExecutionResult::Ready(Err(Diagnostic::new("spawned task panicked"))),
            };
            let mut task_state = lock_mutex(&state.handle);
            *task_state = TaskHandle::Completed(result.clone());
            drop(task_state);
            wake_reactor_subscribers(&state.completion_reactor_subscribers);
            notify_group_failure_wake_flags(&state, &result);
            notify_group_completion_wake_flags(&state);
            state.ready.notify_all();
            notify_runtime_scheduler_if_started();
        });
        Self { inner }
    }

    pub(crate) fn wait_result_with_cancellation(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<TaskWaitStatus> {
        let deadline = deadline_from_timeout_labeled(timeout, "task result timeout")?;
        loop {
            if let Some(result) = self.completed_result() {
                return Ok(match result {
                    TaskExecutionResult::Ready(result) => TaskWaitStatus::Ready(result),
                    TaskExecutionResult::Cancelled => TaskWaitStatus::Cancelled,
                });
            }

            match wait_for_runtime_scheduler(
                Vec::new(),
                false,
                Vec::new(),
                vec![self.clone()],
                deadline,
                cancellation,
            ) {
                RuntimeSchedulerWakeReason::Ready => {}
                RuntimeSchedulerWakeReason::TimedOut => return Ok(TaskWaitStatus::TimedOut),
                RuntimeSchedulerWakeReason::Cancelled => return Ok(TaskWaitStatus::Cancelled),
            }
        }
    }

    pub(crate) fn wait_result_with_cancellation_observed(
        &self,
        timeout: Option<StdDuration>,
        cancellation: Option<&CancellationContext>,
    ) -> io::Result<TaskWaitStatus> {
        let status = self.wait_result_with_cancellation(timeout, cancellation)?;
        if let TaskWaitStatus::Ready(result) = &status {
            self.observe_result(&TaskExecutionResult::Ready(result.clone()));
            if let Ok(value) = result {
                register_current_task_queue_handles(value);
            }
        }
        Ok(status)
    }

    pub(crate) fn unobserved_error(&self) -> Option<Diagnostic> {
        if self.inner.observed_failure.load(Ordering::SeqCst) {
            return None;
        }
        match self.completed_result() {
            Some(TaskExecutionResult::Ready(Err(error))) => Some(error),
            _ => None,
        }
    }

    pub(crate) fn waits_without_deadline(&self) -> bool {
        if self.completed_result().is_some() || !self.inner.lightweight {
            return false;
        }
        self.inner.waits_without_deadline.load(Ordering::SeqCst)
    }

    fn unbounded_wait_has_reachable_waker(&self) -> bool {
        self.unbounded_wait_reachability().0
    }

    fn unbounded_wait_reachability(&self) -> (bool, usize) {
        let mut search = TaskReachabilitySearch::default();
        if let Some(task) = current_lightweight_task() {
            search
                .task_visiting
                .insert(Arc::as_ptr(&task.inner) as usize);
        }
        let reachable = self.can_make_reachable_progress(&mut search);
        (reachable, search.channel_expansions)
    }

    fn can_make_reachable_progress(&self, search: &mut TaskReachabilitySearch) -> bool {
        if self.completed_result().is_some() {
            return true;
        }

        let identity = Arc::as_ptr(&self.inner) as usize;
        if let Some(reachable) = search.task_memo.get(&identity) {
            return *reachable;
        }
        if !search.task_visiting.insert(identity) {
            return false;
        }

        let join_dependencies = lock_mutex(&self.inner.join_dependencies).clone();
        let reachable = if let Some(dependencies) = join_dependencies {
            dependencies.iter().any(|task| {
                task.upgrade()
                    .map(|inner| TaskValue { inner })
                    .is_some_and(|task| task.can_make_reachable_progress(search))
            })
        } else {
            let wait = lock_mutex(&self.inner.current_wait).clone();
            match wait {
                None => {
                    // A runnable task, including one which has not yet been
                    // admitted, can still execute code that wakes a peer.
                    true
                }
                Some(wait)
                    if wait.deadline.is_some()
                        || wait
                            .cancellation
                            .as_ref()
                            .is_some_and(CancellationContext::is_cancelled) =>
                {
                    true
                }
                Some(wait) => {
                    let queue_waker_is_reachable = wait.recv_channels.iter().any(|channel| {
                        channel
                            .upgrade()
                            .map(|inner| ChannelValue { inner })
                            .is_some_and(|channel| {
                                recv_channel_has_reachable_waker(
                                    &channel,
                                    wait.ignore_closed_recv_channels,
                                    search,
                                )
                            })
                    }) || wait.send_channels.iter().any(|channel| {
                        channel
                            .upgrade()
                            .map(|inner| ChannelValue { inner })
                            .is_some_and(|channel| {
                                send_channel_has_reachable_waker(&channel, search)
                            })
                    });
                    let task_waker_is_reachable = wait.task_waits.iter().any(|task| {
                        task.upgrade()
                            .map(|inner| TaskValue { inner })
                            .is_some_and(|task| task.can_make_reachable_progress(search))
                    });
                    queue_waker_is_reachable || task_waker_is_reachable
                }
            }
        };

        search.task_visiting.remove(&identity);
        search.task_memo.insert(identity, reachable);
        reachable
    }
}

pub(crate) fn claim_task_result_observations(tasks: &[TaskValue]) -> Result<()> {
    for task in tasks {
        task.claim_result_observation()?;
    }
    Ok(())
}

pub(crate) fn option_some(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Option".to_string(),
        variant_name: "Some".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn recv_for_task_group_iteration(
    channel: &ChannelValue,
    cancellation: &CancellationContext,
    group: &TaskGroupValue,
) -> RecvValueResult {
    loop {
        match channel.try_recv() {
            TryRecvResult::Value(value) => {
                if channel.has_pending_values() {
                    let _ = yield_now_current_lightweight_task();
                }
                return RecvValueResult::Value(value);
            }
            TryRecvResult::Closed => return RecvValueResult::Closed,
            TryRecvResult::Empty => {}
        }
        // Cancellation is an observable abnormal exit from receive-based task-group
        // iteration.  It must win over the vacuous "all tasks completed" state of
        // an empty group; otherwise cancelling an empty group is misreported as
        // ordinary iteration exhaustion.
        if cancellation.is_cancelled() || group.is_cancelled() || group.has_unobserved_error() {
            return RecvValueResult::Cancelled;
        }
        if group.all_registered_tasks_completed() {
            return RecvValueResult::Closed;
        }
        let wait_cancellation = cancellation.merged(&group.queue_iteration_signal());
        match wait_for_runtime_scheduler(
            vec![channel.clone()],
            false,
            Vec::new(),
            Vec::new(),
            None,
            Some(&wait_cancellation),
        ) {
            RuntimeSchedulerWakeReason::Ready => {}
            RuntimeSchedulerWakeReason::Cancelled => {
                if cancellation.is_cancelled()
                    || group.is_cancelled()
                    || group.has_unobserved_error()
                {
                    return RecvValueResult::Cancelled;
                }
                group.clear_failure_wake_if_no_unobserved_error();
                group.clear_completion_wake_if_tasks_still_running();
            }
            RuntimeSchedulerWakeReason::TimedOut => return RecvValueResult::TimedOut,
        }
    }
}

pub(crate) fn recv_for_registered_producers_iteration(
    channel: &ChannelValue,
    cancellation: &CancellationContext,
) -> RecvValueResult {
    loop {
        match channel.try_recv() {
            TryRecvResult::Value(value) => {
                if channel.has_pending_values() {
                    let _ = yield_now_current_lightweight_task();
                }
                return RecvValueResult::Value(value);
            }
            TryRecvResult::Closed => return RecvValueResult::Closed,
            TryRecvResult::Empty => {}
        }
        let running_producers = channel
            .registered_producer_tasks()
            .into_iter()
            .filter(|task| task.completed_result().is_none())
            .collect::<Vec<_>>();
        if running_producers.is_empty() {
            return RecvValueResult::Closed;
        }
        match wait_for_runtime_scheduler(
            vec![channel.clone()],
            false,
            Vec::new(),
            running_producers,
            None,
            Some(cancellation),
        ) {
            RuntimeSchedulerWakeReason::Ready => {}
            RuntimeSchedulerWakeReason::Cancelled => return RecvValueResult::Cancelled,
            RuntimeSchedulerWakeReason::TimedOut => return RecvValueResult::TimedOut,
        }
    }
}

struct TaskGroupJoinReachabilityGuard {
    state: Option<Arc<TaskState>>,
    previous: Option<Vec<Weak<TaskState>>>,
}

impl TaskGroupJoinReachabilityGuard {
    fn enter(tasks: &[TaskValue]) -> Self {
        let state = (!tasks.is_empty())
            .then(current_lightweight_task)
            .flatten()
            .map(|task| task.inner);
        let previous = state.as_ref().and_then(|state| {
            lock_mutex(&state.join_dependencies).replace(
                tasks
                    .iter()
                    .map(|task| Arc::downgrade(&task.inner))
                    .collect(),
            )
        });
        Self { state, previous }
    }
}

impl Drop for TaskGroupJoinReachabilityGuard {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            *lock_mutex(&state.join_dependencies) = self.previous.take();
        }
    }
}

pub(crate) fn task_group_cleanup_should_cancel(
    tasks: &[TaskValue],
    cancellation: &CancellationContext,
) -> bool {
    let _join_reachability = TaskGroupJoinReachabilityGuard::enter(tasks);
    loop {
        let mut saw_incomplete_task = false;
        for task in tasks {
            match task.wait_result_with_cancellation(
                Some(TASK_GROUP_CLEANUP_PROBE_TIMEOUT),
                Some(cancellation),
            ) {
                Ok(TaskWaitStatus::Ready(_) | TaskWaitStatus::Cancelled) => {}
                Ok(TaskWaitStatus::TimedOut) => {
                    if task.waits_without_deadline() && !task.unbounded_wait_has_reachable_waker() {
                        return true;
                    }
                    if task.completed_result().is_none() {
                        saw_incomplete_task = true;
                    }
                }
                Err(_) => return true,
            }
        }

        if !saw_incomplete_task {
            return false;
        }
    }
}

pub(crate) fn option_none() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Option".to_string(),
        variant_name: "None".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn result_ok(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Result".to_string(),
        variant_name: "Ok".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn result_err(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Result".to_string(),
        variant_name: "Err".to_string(),
        payloads: vec![value],
    })
}

#[cfg(test)]
std::thread_local! {
    static BYTES_RUNTIME_ALLOCATION_BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
    static BYTES_RUNTIME_ENCODED_INPUT_LEN_OVERRIDE: Cell<Option<usize>> = const { Cell::new(None) };
}

#[cfg(test)]
struct BytesRuntimeAllocationBudgetGuard(Option<usize>);

#[cfg(test)]
impl Drop for BytesRuntimeAllocationBudgetGuard {
    fn drop(&mut self) {
        BYTES_RUNTIME_ALLOCATION_BUDGET.with(|budget| budget.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn with_bytes_runtime_allocation_budget<T>(
    successful_allocations: usize,
    operation: impl FnOnce() -> T,
) -> T {
    let previous =
        BYTES_RUNTIME_ALLOCATION_BUDGET.with(|budget| budget.replace(Some(successful_allocations)));
    let _guard = BytesRuntimeAllocationBudgetGuard(previous);
    operation()
}

#[cfg(test)]
struct BytesRuntimeEncodedInputLenGuard(Option<usize>);

#[cfg(test)]
impl Drop for BytesRuntimeEncodedInputLenGuard {
    fn drop(&mut self) {
        BYTES_RUNTIME_ENCODED_INPUT_LEN_OVERRIDE.with(|length| length.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn with_bytes_runtime_encoded_input_len_for_test<T>(
    input_len: usize,
    operation: impl FnOnce() -> T,
) -> T {
    let previous =
        BYTES_RUNTIME_ENCODED_INPUT_LEN_OVERRIDE.with(|length| length.replace(Some(input_len)));
    let _guard = BytesRuntimeEncodedInputLenGuard(previous);
    operation()
}

fn bytes_runtime_allocation_error() -> Diagnostic {
    Diagnostic::coded(
        "AU4005",
        "memory allocation failed while materializing byte data",
    )
}

fn bytes_runtime_allocation_checkpoint() -> Result<()> {
    #[cfg(test)]
    {
        let injected_failure = BYTES_RUNTIME_ALLOCATION_BUDGET.with(|budget| match budget.get() {
            Some(0) => true,
            Some(remaining) => {
                budget.set(Some(remaining - 1));
                false
            }
            None => false,
        });
        if injected_failure {
            return Err(bytes_runtime_allocation_error());
        }
    }
    Ok(())
}

fn bytes_runtime_try_reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<()> {
    if additional == 0 {
        return Ok(());
    }
    bytes_runtime_allocation_checkpoint()?;
    values
        .try_reserve(additional)
        .map_err(|_| bytes_runtime_allocation_error())
}

fn bytes_runtime_owned_string(value: &str) -> Result<String> {
    let mut owned = String::new();
    if !value.is_empty() {
        bytes_runtime_allocation_checkpoint()?;
        owned
            .try_reserve(value.len())
            .map_err(|_| bytes_runtime_allocation_error())?;
        owned.push_str(value);
    }
    Ok(owned)
}

fn exact_runtime_uint8(value: &Value) -> Option<u8> {
    let Value::Int(value) = value else {
        return None;
    };
    if value.runtime_kind() != Some(IntegerKind::Uint8) {
        return None;
    }
    value.as_i128().and_then(|value| u8::try_from(value).ok())
}

fn runtime_uint8_elements<'a>(value: &'a Value, call: &str) -> Result<&'a [Value]> {
    let Value::Vec(value) = value else {
        return Err(Diagnostic::coded(
            "AU4001",
            format!("`{call}` expects a runtime `list[uint8]` value"),
        ));
    };
    if !matches!(
        &value.element_type,
        Type::Named(name, arguments) if name == "uint8" && arguments.is_empty()
    ) || value
        .elements
        .iter()
        .any(|value| exact_runtime_uint8(value).is_none())
    {
        return Err(Diagnostic::coded(
            "AU4001",
            format!("`{call}` expects an exact runtime `list[uint8]` value"),
        ));
    }
    Ok(&value.elements)
}

fn host_bytes_from_runtime_elements(elements: &[Value]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes_runtime_try_reserve(&mut bytes, elements.len())?;
    bytes.extend(
        elements
            .iter()
            .map(|value| exact_runtime_uint8(value).expect("runtime bytes were validated")),
    );
    Ok(bytes)
}

pub(crate) fn host_bytes_from_runtime(value: &Value, call: &str) -> Result<Vec<u8>> {
    host_bytes_from_runtime_elements(runtime_uint8_elements(value, call)?)
}

fn runtime_utf8_error_index(bytes: impl IntoIterator<Item = u8>) -> Option<usize> {
    let mut bytes = bytes.into_iter();
    let mut index = 0;

    while let Some(first) = bytes.next() {
        let sequence_start = index;
        index += 1;
        let (sequence_len, second_min, second_max) = match first {
            0x00..=0x7f => continue,
            0xc2..=0xdf => (2, 0x80, 0xbf),
            0xe0 => (3, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => (3, 0x80, 0xbf),
            0xed => (3, 0x80, 0x9f),
            0xf0 => (4, 0x90, 0xbf),
            0xf1..=0xf3 => (4, 0x80, 0xbf),
            0xf4 => (4, 0x80, 0x8f),
            _ => return Some(sequence_start),
        };

        let Some(second) = bytes.next() else {
            return Some(sequence_start);
        };
        index += 1;
        if !(second_min..=second_max).contains(&second) {
            return Some(sequence_start);
        }
        for _ in 2..sequence_len {
            let Some(continuation) = bytes.next() else {
                return Some(sequence_start);
            };
            index += 1;
            if !(0x80..=0xbf).contains(&continuation) {
                return Some(sequence_start);
            }
        }
    }
    None
}

fn bytes_runtime_encoded_input_len(actual: usize) -> usize {
    #[cfg(test)]
    if let Some(overridden) = BYTES_RUNTIME_ENCODED_INPUT_LEN_OVERRIDE.with(|length| length.get()) {
        return overridden;
    }
    actual
}

fn preflight_runtime_bytes_encoder<'a>(
    value: &'a Value,
    call: &str,
    encoded_len: fn(usize) -> std::result::Result<usize, BytesResourceError>,
) -> Result<&'a [Value]> {
    let elements = runtime_uint8_elements(value, call)?;
    encoded_len(bytes_runtime_encoded_input_len(elements.len()))
        .map_err(bytes_resource_error_to_diagnostic)?;
    Ok(elements)
}

pub(crate) fn bytes_resource_error_to_diagnostic(error: BytesResourceError) -> Diagnostic {
    Diagnostic::coded("AU4005", error.to_string())
}

pub(crate) fn runtime_bytes_from_host(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > bytes_codec::MAX_CODEC_OUTPUT_LEN {
        return Err(bytes_resource_error_to_diagnostic(
            BytesResourceError::OutputTooLarge {
                maximum: bytes_codec::MAX_CODEC_OUTPUT_LEN,
            },
        ));
    }

    let mut elements = Vec::new();
    bytes_runtime_try_reserve(&mut elements, bytes.len())?;
    elements.extend(bytes.iter().copied().map(|byte| {
        Value::Int(
            IntegerValue::from_typed_unsigned(u128::from(byte), IntegerKind::Uint8)
                .expect("every host byte fits the uint8 runtime kind"),
        )
    }));
    Ok(Value::Vec(VecValue {
        element_type: Type::Named(bytes_runtime_owned_string("uint8")?, Vec::new()),
        elements,
    }))
}

fn bytes_runtime_single_payload(value: Value) -> Result<Vec<Value>> {
    let mut payloads = Vec::new();
    bytes_runtime_try_reserve(&mut payloads, 1)?;
    payloads.push(value);
    Ok(payloads)
}

fn bytes_runtime_enum_value(
    enum_name: &str,
    variant_name: &str,
    payloads: Vec<Value>,
) -> Result<Value> {
    Ok(Value::EnumVariant(EnumVariantValue {
        enum_name: bytes_runtime_owned_string(enum_name)?,
        variant_name: bytes_runtime_owned_string(variant_name)?,
        payloads,
    }))
}

fn bytes_runtime_result(variant_name: &str, value: Value) -> Result<Value> {
    bytes_runtime_enum_value("Result", variant_name, bytes_runtime_single_payload(value)?)
}

fn bytes_error_index(value: usize) -> Result<Value> {
    let value = i32::try_from(value).map_err(|_| {
        Diagnostic::coded(
            "AU4005",
            "byte-codec error metadata exceeds the `bytes.Error` int32 payload range",
        )
    })?;
    Ok(Value::Int(IntegerValue::from_i32(value)))
}

fn bytes_data_error_to_runtime(error: BytesDataError) -> Result<Value> {
    let (variant_name, payloads) = match error {
        BytesDataError::InvalidUtf8 { index } => (
            "InvalidUtf8",
            bytes_runtime_single_payload(bytes_error_index(index)?)?,
        ),
        BytesDataError::InvalidHexLength { length } => (
            "InvalidHexLength",
            bytes_runtime_single_payload(bytes_error_index(length)?)?,
        ),
        BytesDataError::InvalidHexDigit { index, byte } => {
            let mut payloads = Vec::new();
            bytes_runtime_try_reserve(&mut payloads, 2)?;
            payloads.push(bytes_error_index(index)?);
            payloads.push(Value::Int(
                IntegerValue::from_typed_unsigned(u128::from(byte), IntegerKind::Uint8)
                    .expect("every byte error payload fits uint8"),
            ));
            ("InvalidHexDigit", payloads)
        }
        BytesDataError::InvalidBase64 { index } => (
            "InvalidBase64",
            bytes_runtime_single_payload(bytes_error_index(index)?)?,
        ),
    };
    bytes_runtime_enum_value("bytes.Error", variant_name, payloads)
}

pub(crate) fn bytes_codec_error_to_result(error: BytesCodecError) -> Result<Value> {
    match error {
        BytesCodecError::Data(error) => {
            bytes_runtime_result("Err", bytes_data_error_to_runtime(error)?)
        }
        BytesCodecError::Resource(error) => Err(bytes_resource_error_to_diagnostic(error)),
    }
}

fn bytes_resource_only<T>(
    result: std::result::Result<T, BytesCodecError>,
    call: &str,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(BytesCodecError::Resource(error)) => Err(bytes_resource_error_to_diagnostic(error)),
        Err(BytesCodecError::Data(error)) => Err(Diagnostic::coded(
            "AU4001",
            format!("internal byte-codec data error in `{call}`: {error}"),
        )),
    }
}

fn host_string_value_ref<'a>(value: &'a Value, index: usize, call: &str) -> Result<&'a str> {
    match value {
        Value::String(value) => Ok(value),
        other => Err(Diagnostic::new(format!(
            "`{call}` expects argument {} to be `str`, found `{}`",
            index + 1,
            other.render()
        ))),
    }
}

fn bytes_host_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "bytes::hex_encode"
            | "bytes::hex_decode"
            | "bytes::base64_encode"
            | "bytes::base64_decode"
            | "bytes::sha256"
            | "bytes::sha256_string"
            | "str.to_bytes"
            | "str.from_bytes"
    )
}

pub(crate) fn evaluate_string_to_bytes_host_ref(text: &str) -> Result<Value> {
    bytes_resource_only(bytes_codec::string_to_bytes(text), "str.to_bytes")
        .and_then(|bytes| runtime_bytes_from_host(&bytes))
}

pub(crate) fn evaluate_bytes_host_builtin_ref(name: &str, value: &Value) -> Option<Result<Value>> {
    if !bytes_host_builtin_name(name) {
        return None;
    }

    let result = match name {
        "bytes::hex_encode" => {
            preflight_runtime_bytes_encoder(value, name, bytes_codec::hex_encoded_len)
                .and_then(host_bytes_from_runtime_elements)
                .and_then(|bytes| {
                    bytes_resource_only(bytes_codec::hex_encode(&bytes), name).map(Value::String)
                })
        }
        "bytes::hex_decode" => {
            let text = match host_string_value_ref(value, 0, name) {
                Ok(text) => text,
                Err(error) => return Some(Err(error)),
            };
            match bytes_codec::hex_decode(text) {
                Ok(bytes) => runtime_bytes_from_host(&bytes)
                    .and_then(|value| bytes_runtime_result("Ok", value)),
                Err(error) => bytes_codec_error_to_result(error),
            }
        }
        "bytes::base64_encode" => {
            preflight_runtime_bytes_encoder(value, name, bytes_codec::base64_encoded_len)
                .and_then(host_bytes_from_runtime_elements)
                .and_then(|bytes| {
                    bytes_resource_only(bytes_codec::base64_encode(&bytes), name).map(Value::String)
                })
        }
        "bytes::base64_decode" => {
            let text = match host_string_value_ref(value, 0, name) {
                Ok(text) => text,
                Err(error) => return Some(Err(error)),
            };
            match bytes_codec::base64_decode(text) {
                Ok(bytes) => runtime_bytes_from_host(&bytes)
                    .and_then(|value| bytes_runtime_result("Ok", value)),
                Err(error) => bytes_codec_error_to_result(error),
            }
        }
        "bytes::sha256" => host_bytes_from_runtime(value, name).and_then(|bytes| {
            bytes_resource_only(bytes_codec::sha256_bytes(&bytes), name)
                .and_then(|digest| runtime_bytes_from_host(&digest))
        }),
        "bytes::sha256_string" => {
            let text = match host_string_value_ref(value, 0, name) {
                Ok(text) => text,
                Err(error) => return Some(Err(error)),
            };
            bytes_resource_only(bytes_codec::sha256_string(text), name)
                .and_then(|digest| runtime_bytes_from_host(&digest))
        }
        "str.to_bytes" => {
            let text = match host_string_value_ref(value, 0, name) {
                Ok(text) => text,
                Err(error) => return Some(Err(error)),
            };
            evaluate_string_to_bytes_host_ref(text)
        }
        "str.from_bytes" => {
            let elements = match runtime_uint8_elements(value, name) {
                Ok(elements) => elements,
                Err(error) => return Some(Err(error)),
            };
            if let Some(index) = runtime_utf8_error_index(
                elements
                    .iter()
                    .map(|value| exact_runtime_uint8(value).expect("runtime bytes were validated")),
            ) {
                return Some(bytes_codec_error_to_result(BytesCodecError::Data(
                    BytesDataError::InvalidUtf8 { index },
                )));
            }
            let bytes = match host_bytes_from_runtime_elements(elements) {
                Ok(bytes) => bytes,
                Err(error) => return Some(Err(error)),
            };
            match bytes_codec::string_from_bytes(&bytes) {
                Ok(text) => bytes_runtime_result("Ok", Value::String(text)),
                Err(error) => bytes_codec_error_to_result(error),
            }
        }
        _ => unreachable!("byte host builtin names were filtered above"),
    };
    Some(result)
}

static HOST_MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();
static HOST_METRICS: OnceLock<Mutex<BTreeMap<String, i64>>> = OnceLock::new();

fn host_string_ref_arg<'a>(args: &'a [Value], index: usize, call: &str) -> Result<&'a str> {
    match args.get(index) {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(Diagnostic::new(format!(
            "`{call}` expects argument {} to be `str`, found `{}`",
            index + 1,
            other.render()
        ))),
        None => Err(Diagnostic::new(format!(
            "`{call}` is missing argument {}",
            index + 1
        ))),
    }
}

fn host_string_arg(args: &[Value], index: usize, call: &str) -> Result<String> {
    Ok(host_string_ref_arg(args, index, call)?.to_owned())
}

fn host_string_map_arg(
    args: &[Value],
    index: usize,
    call: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(Value::Map(map)) = args.get(index) else {
        return Err(Diagnostic::new(format!(
            "`{call}` expects argument {} to be `dict[str, str]`",
            index + 1
        )));
    };
    map.entries
        .iter()
        .map(|(key, value)| match (key, value) {
            (Value::String(key), Value::String(value)) => Ok((key.clone(), value.clone())),
            _ => Err(Diagnostic::new(format!(
                "`{call}` expects `dict[str, str]`"
            ))),
        })
        .collect()
}

fn host_string_map_value(entries: BTreeMap<String, String>) -> Value {
    Value::Map(MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("str"),
        entries: entries
            .into_iter()
            .map(|(key, value)| (Value::String(key), Value::String(value)))
            .collect(),
    })
}

pub(crate) fn host_process_args() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

fn host_expect_arity(name: &str, args: &[Value], expected: usize) -> Result<()> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(Diagnostic::new(format!(
            "`{name}` expects {expected} arguments, found {}",
            args.len()
        )))
    }
}

fn host_millis_value(millis: u128, clock: &str) -> Result<Value> {
    let millis = match i64::try_from(millis) {
        Ok(millis) => millis,
        Err(_) => {
            return Err(Diagnostic::new(format!(
                "{clock} does not fit in Aura `int64`"
            )))
        }
    };
    Ok(Value::Int(IntegerValue::from_signed(i128::from(millis))))
}

#[cfg(test)]
std::thread_local! {
    static JSON_RUNTIME_ALLOCATION_BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
    static JSON_RUNTIME_NODE_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
    static JSON_RUNTIME_CONVERSION_ALLOCATION_BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
    static JSON_CODEC_SOURCE_CLONE_COUNT: Cell<usize> = const { Cell::new(0) };
    static FAIL_NEXT_JSON_CODEC_SOURCE_CLONE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
struct JsonRuntimeAllocationBudgetGuard(Option<usize>);

#[cfg(test)]
impl Drop for JsonRuntimeAllocationBudgetGuard {
    fn drop(&mut self) {
        JSON_RUNTIME_ALLOCATION_BUDGET.with(|budget| budget.set(self.0));
    }
}

#[cfg(test)]
struct JsonRuntimeNodeLimitGuard(Option<usize>);

#[cfg(test)]
impl Drop for JsonRuntimeNodeLimitGuard {
    fn drop(&mut self) {
        JSON_RUNTIME_NODE_LIMIT.with(|limit| limit.set(self.0));
    }
}

#[cfg(test)]
struct JsonRuntimeConversionAllocationBudgetGuard(Option<usize>);

#[cfg(test)]
impl Drop for JsonRuntimeConversionAllocationBudgetGuard {
    fn drop(&mut self) {
        JSON_RUNTIME_CONVERSION_ALLOCATION_BUDGET.with(|budget| budget.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn with_json_runtime_allocation_budget<T>(
    successful_allocations: usize,
    operation: impl FnOnce() -> T,
) -> T {
    let previous =
        JSON_RUNTIME_ALLOCATION_BUDGET.with(|budget| budget.replace(Some(successful_allocations)));
    let _guard = JsonRuntimeAllocationBudgetGuard(previous);
    operation()
}

#[cfg(test)]
pub(crate) fn with_json_runtime_node_limit<T>(limit: usize, operation: impl FnOnce() -> T) -> T {
    let previous = JSON_RUNTIME_NODE_LIMIT.with(|current| current.replace(Some(limit)));
    let _guard = JsonRuntimeNodeLimitGuard(previous);
    operation()
}

#[cfg(test)]
pub(crate) fn with_json_runtime_conversion_allocation_budget<T>(
    successful_allocations: usize,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = JSON_RUNTIME_CONVERSION_ALLOCATION_BUDGET
        .with(|budget| budget.replace(Some(successful_allocations)));
    let _guard = JsonRuntimeConversionAllocationBudgetGuard(previous);
    operation()
}

fn json_runtime_node_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = JSON_RUNTIME_NODE_LIMIT.with(Cell::get) {
        return limit;
    }
    json_codec::MAX_JSON_VALUE_NODES
}

#[cfg(test)]
fn reset_json_codec_source_clone_count() {
    JSON_CODEC_SOURCE_CLONE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn json_codec_source_clone_count() -> usize {
    JSON_CODEC_SOURCE_CLONE_COUNT.with(Cell::get)
}

#[cfg(test)]
fn fail_next_json_codec_source_clone() {
    FAIL_NEXT_JSON_CODEC_SOURCE_CLONE.with(|fail| fail.set(true));
}

#[derive(Debug)]
enum JsonCodecServiceError {
    Codec(JsonCodecError),
    Panicked(String),
}

pub(crate) struct JsonCodecReservation {
    pool: Arc<JsonCodecPool>,
    submitted: bool,
}

impl JsonCodecReservation {
    fn submit(mut self, job: JsonCodecJob) {
        self.submitted = true;
        self.pool.submit_reserved(job);
    }
}

impl Drop for JsonCodecReservation {
    fn drop(&mut self) {
        if !self.submitted {
            self.pool.release_reservation();
        }
    }
}

fn reserve_json_codec_slot(pool: &Arc<JsonCodecPool>) -> JsonCodecReservation {
    if with_current_lightweight_task_context(|_| ()).is_some() {
        while !pool.try_reserve() {
            match pool
                .availability_signal
                .recv_result_with_deadline(None, None)
            {
                RecvValueResult::Value(_) => {}
                RecvValueResult::Closed => {
                    unreachable!("the process-lifetime JSON codec service remains open")
                }
                RecvValueResult::Cancelled | RecvValueResult::TimedOut => {
                    unreachable!("JSON codec admission has no cancellation or deadline")
                }
            }
        }
    } else {
        pool.reserve_blocking();
    }
    JsonCodecReservation {
        pool: pool.clone(),
        submitted: false,
    }
}

fn prepare_json_codec_source_with_pool<F>(
    pool: &Arc<JsonCodecPool>,
    clone_source: F,
) -> Result<(String, JsonCodecReservation)>
where
    F: FnOnce() -> Result<String>,
{
    let reservation = reserve_json_codec_slot(pool);
    let source = clone_source()?;
    Ok((source, reservation))
}

pub(crate) fn prepare_json_codec_source<F>(
    clone_source: F,
) -> Result<(String, JsonCodecReservation)>
where
    F: FnOnce() -> Result<String>,
{
    prepare_json_codec_source_with_pool(json_codec_pool(), clone_source)
}

pub(crate) fn clone_json_codec_source(source: &str) -> Result<String> {
    #[cfg(test)]
    JSON_CODEC_SOURCE_CLONE_COUNT.with(|count| count.set(count.get() + 1));
    #[cfg(test)]
    if FAIL_NEXT_JSON_CODEC_SOURCE_CLONE.with(|fail| fail.replace(false)) {
        return Err(Diagnostic::coded(
            "AU4005",
            JsonCodecError::AllocationFailed.to_string(),
        ));
    }

    let mut owned = String::new();
    if !source.is_empty() {
        owned.try_reserve(source.len()).map_err(|_| {
            Diagnostic::coded("AU4005", JsonCodecError::AllocationFailed.to_string())
        })?;
        owned.push_str(source);
    }
    Ok(owned)
}

fn run_json_codec_operation_on_pool_with<F>(
    reservation: JsonCodecReservation,
    operation: F,
) -> std::result::Result<JsonValue, JsonCodecServiceError>
where
    F: FnOnce() -> std::result::Result<JsonValue, JsonCodecError> + Send + 'static,
{
    let completion = ChannelValue::new();
    let result = Arc::new(Mutex::new(None));
    reservation.submit(JsonCodecJob {
        operation: Box::new(operation),
        result: result.clone(),
        completion: completion.clone(),
    });

    loop {
        match completion.recv_result_with_deadline(None, None) {
            RecvValueResult::Value(_) | RecvValueResult::Closed => {
                if let Some(outcome) = lock_mutex(&result).take() {
                    return outcome;
                }
            }
            RecvValueResult::Cancelled | RecvValueResult::TimedOut => {
                // JSON parse was synchronous before codec isolation. Defer task
                // cancellation until the admitted parse has completed, then let
                // the caller encounter cancellation at its next normal boundary.
                if let Some(outcome) = lock_mutex(&result).take() {
                    return outcome;
                }
                if yield_now_current_lightweight_task().is_none() {
                    thread::yield_now();
                }
            }
        }
    }
}

fn run_json_codec_parse_on_pool(
    source: String,
    reservation: JsonCodecReservation,
) -> std::result::Result<JsonValue, JsonCodecServiceError> {
    run_json_codec_operation_on_pool_with(reservation, move || json_codec::parse(&source))
}

fn json_runtime_node_limit_error(limit: usize) -> Diagnostic {
    Diagnostic::coded(
        "AU4005",
        JsonCodecError::MaterializationTooLarge { limit }.to_string(),
    )
}

fn consume_json_runtime_node(remaining: &mut usize, limit: usize) -> Result<()> {
    if *remaining == 0 {
        return Err(json_runtime_node_limit_error(limit));
    }
    *remaining -= 1;
    Ok(())
}

fn json_runtime_container_capacity(
    child_count: usize,
    remaining: usize,
    limit: usize,
) -> Result<usize> {
    if child_count <= remaining {
        Ok(child_count)
    } else {
        Err(json_runtime_node_limit_error(limit))
    }
}

fn json_parse_allocation_error() -> Diagnostic {
    Diagnostic::coded(
        "AU4005",
        "memory allocation failed while materializing parsed JSON",
    )
}

fn json_runtime_conversion_allocation_checkpoint() -> Result<()> {
    #[cfg(test)]
    {
        let injected_failure =
            JSON_RUNTIME_CONVERSION_ALLOCATION_BUDGET.with(|budget| match budget.get() {
                Some(0) => true,
                Some(remaining) => {
                    budget.set(Some(remaining - 1));
                    false
                }
                None => false,
            });
        if injected_failure {
            return Err(Diagnostic::coded(
                "AU4005",
                "memory allocation failed while preparing JSON output",
            ));
        }
    }
    Ok(())
}

fn json_parse_allocation_checkpoint() -> Result<()> {
    #[cfg(test)]
    {
        let injected_failure = JSON_RUNTIME_ALLOCATION_BUDGET.with(|budget| match budget.get() {
            Some(0) => true,
            Some(remaining) => {
                budget.set(Some(remaining - 1));
                false
            }
            None => false,
        });
        if injected_failure {
            return Err(json_parse_allocation_error());
        }
    }
    Ok(())
}

fn json_parse_try_reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<()> {
    if additional == 0 {
        return Ok(());
    }
    json_parse_allocation_checkpoint()?;
    values
        .try_reserve(additional)
        .map_err(|_| json_parse_allocation_error())
}

fn json_parse_owned_string(value: &str) -> Result<String> {
    let mut owned = String::new();
    if !value.is_empty() {
        json_parse_allocation_checkpoint()?;
        owned
            .try_reserve(value.len())
            .map_err(|_| json_parse_allocation_error())?;
        owned.push_str(value);
    }
    Ok(owned)
}

fn json_runtime_type(name: &str) -> Result<Type> {
    Ok(Type::Named(json_parse_owned_string(name)?, Vec::new()))
}

fn json_runtime_single_payload(value: Value) -> Result<Vec<Value>> {
    let mut payloads = Vec::new();
    json_parse_try_reserve(&mut payloads, 1)?;
    payloads.push(value);
    Ok(payloads)
}

fn json_runtime_enum_value(
    enum_name: &str,
    variant_name: &str,
    payloads: Vec<Value>,
) -> Result<Value> {
    Ok(Value::EnumVariant(EnumVariantValue {
        enum_name: json_parse_owned_string(enum_name)?,
        variant_name: json_parse_owned_string(variant_name)?,
        payloads,
    }))
}

pub(crate) fn json_value_to_runtime(value: JsonValue) -> Result<Value> {
    enum MaterializationFrame {
        Array {
            remaining: std::vec::IntoIter<JsonValue>,
            elements: Vec<Value>,
        },
        Object {
            remaining: std::vec::IntoIter<(String, JsonValue)>,
            entries: Vec<(Value, Value)>,
            pending_key: Option<String>,
        },
    }

    let limit = json_runtime_node_limit();
    let mut remaining = limit;
    let mut frames = Vec::new();
    let mut next = Some(value);
    let mut completed = None;
    let mut materialized_nodes = 0usize;

    loop {
        if let Some(value) = next.take() {
            consume_json_runtime_node(&mut remaining, limit)?;
            materialized_nodes += 1;
            if materialized_nodes.is_multiple_of(1024) {
                let _ = yield_now_current_lightweight_task();
            }
            completed = match value {
                JsonValue::Null => Some(json_runtime_enum_value("json.Value", "Null", Vec::new())?),
                JsonValue::Bool(value) => Some(json_runtime_enum_value(
                    "json.Value",
                    "Bool",
                    json_runtime_single_payload(Value::Bool(value))?,
                )?),
                JsonValue::Int(value) => Some(json_runtime_enum_value(
                    "json.Value",
                    "Int",
                    json_runtime_single_payload(Value::Int(IntegerValue::from_i64(value)))?,
                )?),
                JsonValue::Float(value) => Some(json_runtime_enum_value(
                    "json.Value",
                    "Float",
                    json_runtime_single_payload(Value::Float(value))?,
                )?),
                JsonValue::String(value) => Some(json_runtime_enum_value(
                    "json.Value",
                    "String",
                    json_runtime_single_payload(Value::String(value))?,
                )?),
                JsonValue::Array(values) => {
                    let capacity = json_runtime_container_capacity(values.len(), remaining, limit)?;
                    let mut elements = Vec::new();
                    json_parse_try_reserve(&mut elements, capacity)?;
                    json_parse_try_reserve(&mut frames, 1)?;
                    frames.push(MaterializationFrame::Array {
                        remaining: values.into_iter(),
                        elements,
                    });
                    None
                }
                JsonValue::Object(values) => {
                    let capacity = json_runtime_container_capacity(values.len(), remaining, limit)?;
                    let mut entries = Vec::new();
                    json_parse_try_reserve(&mut entries, capacity)?;
                    json_parse_try_reserve(&mut frames, 1)?;
                    frames.push(MaterializationFrame::Object {
                        remaining: values.into_iter(),
                        entries,
                        pending_key: None,
                    });
                    None
                }
            };
        } else {
            let frame = frames
                .pop()
                .expect("JSON materialization always has a pending frame");
            match frame {
                MaterializationFrame::Array {
                    mut remaining,
                    mut elements,
                } => {
                    if let Some(value) = completed.take() {
                        elements.push(value);
                    }
                    if let Some(value) = remaining.next() {
                        json_parse_try_reserve(&mut frames, 1)?;
                        frames.push(MaterializationFrame::Array {
                            remaining,
                            elements,
                        });
                        next = Some(value);
                    } else {
                        completed = Some(json_runtime_enum_value(
                            "json.Value",
                            "Array",
                            json_runtime_single_payload(Value::Vec(VecValue {
                                element_type: json_runtime_type("json.Value")?,
                                elements,
                            }))?,
                        )?);
                    }
                }
                MaterializationFrame::Object {
                    mut remaining,
                    mut entries,
                    mut pending_key,
                } => {
                    if let Some(value) = completed.take() {
                        entries.push((
                            Value::String(
                                pending_key
                                    .take()
                                    .expect("completed JSON object values always have a key"),
                            ),
                            value,
                        ));
                    }
                    if let Some((key, value)) = remaining.next() {
                        json_parse_try_reserve(&mut frames, 1)?;
                        frames.push(MaterializationFrame::Object {
                            remaining,
                            entries,
                            pending_key: Some(key),
                        });
                        next = Some(value);
                    } else {
                        completed = Some(json_runtime_enum_value(
                            "json.Value",
                            "Object",
                            json_runtime_single_payload(Value::Map(MapValue {
                                key_type: json_runtime_type("str")?,
                                value_type: json_runtime_type("json.Value")?,
                                entries,
                            }))?,
                        )?);
                    }
                }
            }
        }

        if next.is_none() && frames.is_empty() {
            return Ok(completed.expect("completed JSON materialization has a value"));
        }
    }
}

fn json_parse_success_value(value: JsonValue) -> Result<Value> {
    let value = json_value_to_runtime(value)?;
    json_runtime_enum_value("Result", "Ok", json_runtime_single_payload(value)?)
}

fn json_parse_failure_value(error: JsonCodecError) -> Result<Value> {
    let error = json_parse_error_value(error)?;
    json_runtime_enum_value("Result", "Err", json_runtime_single_payload(error)?)
}

pub(crate) fn json_parse_owned_to_runtime(
    source: String,
    reservation: JsonCodecReservation,
) -> Result<Value> {
    match run_json_codec_parse_on_pool(source, reservation) {
        Ok(value) => json_parse_success_value(value),
        Err(JsonCodecServiceError::Codec(
            error @ JsonCodecError::MaterializationTooLarge { .. },
        ))
        | Err(JsonCodecServiceError::Codec(error @ JsonCodecError::AllocationFailed)) => {
            Err(Diagnostic::coded("AU4005", error.to_string()))
        }
        Err(JsonCodecServiceError::Codec(error)) => json_parse_failure_value(error),
        Err(JsonCodecServiceError::Panicked(message)) => Err(Diagnostic::coded(
            "AU4005",
            format!("JSON codec worker panicked: {message}"),
        )),
    }
}

#[cfg(test)]
pub(crate) fn json_parse_to_runtime(source: &str) -> Result<Value> {
    let (source, reservation) = prepare_json_codec_source(|| clone_json_codec_source(source))?;
    json_parse_owned_to_runtime(source, reservation)
}

pub(crate) fn runtime_value_to_json(value: &Value) -> Result<JsonValue> {
    enum ConversionFrame<'a> {
        Array {
            values: &'a [Value],
            next_index: usize,
            converted: Vec<JsonValue>,
            child_depth: usize,
        },
        Object {
            entries: &'a [(Value, Value)],
            next_index: usize,
            converted: Vec<(String, JsonValue)>,
            pending_key: &'a str,
            child_depth: usize,
        },
    }

    fn malformed(message: impl Into<String>) -> Diagnostic {
        Diagnostic::coded(
            "AU4001",
            format!("malformed runtime `json.Value`: {}", message.into()),
        )
    }

    fn clone_string(value: &str) -> Result<String> {
        let mut cloned = String::new();
        if !value.is_empty() {
            json_runtime_conversion_allocation_checkpoint()?;
            cloned
                .try_reserve(value.len())
                .map_err(json_runtime_allocation_error)?;
        }
        cloned.push_str(value);
        Ok(cloned)
    }

    let limit = json_runtime_node_limit();
    let mut remaining = limit;
    let mut frames = Vec::new();
    let mut next = Some((value, 0usize));
    let mut completed = None;

    loop {
        if let Some((value, depth)) = next.take() {
            consume_json_runtime_node(&mut remaining, limit)?;

            let Value::EnumVariant(variant) = value else {
                return Err(malformed(format!(
                    "expected `json.Value`, found `{}`",
                    value.render()
                )));
            };
            if nominal_runtime_base_name(&variant.enum_name) != "json.Value" {
                return Err(malformed(format!(
                    "expected enum `json.Value`, found `{}`",
                    nominal_runtime_base_name(&variant.enum_name)
                )));
            }

            completed = match (variant.variant_name.as_str(), variant.payloads.as_slice()) {
                ("Null", []) => Some(JsonValue::Null),
                ("Bool", [Value::Bool(value)]) => Some(JsonValue::Bool(*value)),
                ("Int", [Value::Int(value)]) if json_int_metadata_is_exact(value) => {
                    let value = value
                        .as_i128()
                        .and_then(|value| i64::try_from(value).ok())
                        .ok_or_else(|| malformed("Value.Int payload is outside `int64`"))?;
                    Some(JsonValue::Int(value))
                }
                ("Int", [Value::Int(_)]) => {
                    return Err(malformed(
                        "Value.Int payload must be exactly `int64` at runtime",
                    ))
                }
                ("Float", [Value::Float(value)]) => Some(JsonValue::Float(*value)),
                ("String", [Value::String(value)]) => Some(JsonValue::String(clone_string(value)?)),
                ("Array", [Value::Vec(values)]) if json_array_metadata_is_exact(values) => {
                    let child_depth = json_runtime_child_depth(depth)?;
                    let capacity =
                        json_runtime_container_capacity(values.elements.len(), remaining, limit)?;
                    let mut converted = Vec::new();
                    json_runtime_conversion_try_reserve(&mut converted, capacity)?;
                    if values.elements.is_empty() {
                        Some(JsonValue::Array(converted))
                    } else {
                        frames
                            .try_reserve(1)
                            .map_err(json_runtime_allocation_error)?;
                        frames.push(ConversionFrame::Array {
                            values: &values.elements,
                            next_index: 1,
                            converted,
                            child_depth,
                        });
                        next = Some((&values.elements[0], child_depth));
                        None
                    }
                }
                ("Array", [Value::Vec(_)]) => {
                    return Err(malformed(
                        "Value.Array payload must be exactly `list[json.Value]` at runtime",
                    ))
                }
                ("Object", [Value::Map(entries)]) if json_object_metadata_is_exact(entries) => {
                    let child_depth = json_runtime_child_depth(depth)?;
                    let capacity =
                        json_runtime_container_capacity(entries.entries.len(), remaining, limit)?;
                    let mut converted = Vec::new();
                    json_runtime_conversion_try_reserve(&mut converted, capacity)?;
                    if entries.entries.is_empty() {
                        Some(JsonValue::Object(converted))
                    } else {
                        let (key, value) = &entries.entries[0];
                        let Value::String(key) = key else {
                            return Err(malformed(format!(
                                "Value.Object key must be `str`, found `{}`",
                                key.render()
                            )));
                        };
                        frames
                            .try_reserve(1)
                            .map_err(json_runtime_allocation_error)?;
                        frames.push(ConversionFrame::Object {
                            entries: &entries.entries,
                            next_index: 1,
                            converted,
                            pending_key: key,
                            child_depth,
                        });
                        next = Some((value, child_depth));
                        None
                    }
                }
                ("Object", [Value::Map(_)]) => {
                    return Err(malformed(
                        "Value.Object payload must be exactly `dict[str, json.Value]` at runtime",
                    ))
                }
                (variant_name, _) => {
                    return Err(malformed(format!(
                        "variant `{variant_name}` has an invalid payload shape"
                    )))
                }
            };
        } else {
            let frame = frames
                .pop()
                .expect("runtime JSON conversion always has a pending frame");
            match frame {
                ConversionFrame::Array {
                    values,
                    mut next_index,
                    mut converted,
                    child_depth,
                } => {
                    converted.push(
                        completed
                            .take()
                            .expect("a completed JSON array child has a value"),
                    );
                    if let Some(value) = values.get(next_index) {
                        next_index += 1;
                        frames
                            .try_reserve(1)
                            .map_err(json_runtime_allocation_error)?;
                        frames.push(ConversionFrame::Array {
                            values,
                            next_index,
                            converted,
                            child_depth,
                        });
                        next = Some((value, child_depth));
                    } else {
                        completed = Some(JsonValue::Array(converted));
                    }
                }
                ConversionFrame::Object {
                    entries,
                    mut next_index,
                    mut converted,
                    pending_key,
                    child_depth,
                } => {
                    let value = completed
                        .take()
                        .expect("a completed JSON object child has a value");
                    converted.push((clone_string(pending_key)?, value));
                    if let Some((key, value)) = entries.get(next_index) {
                        let Value::String(key) = key else {
                            return Err(malformed(format!(
                                "Value.Object key must be `str`, found `{}`",
                                key.render()
                            )));
                        };
                        next_index += 1;
                        frames
                            .try_reserve(1)
                            .map_err(json_runtime_allocation_error)?;
                        frames.push(ConversionFrame::Object {
                            entries,
                            next_index,
                            converted,
                            pending_key: key,
                            child_depth,
                        });
                        next = Some((value, child_depth));
                    } else {
                        completed = Some(JsonValue::Object(converted));
                    }
                }
            }
        }

        if next.is_none() && frames.is_empty() {
            return Ok(completed.expect("completed runtime JSON conversion has a value"));
        }
    }
}

pub(crate) fn json_int_metadata_is_exact(value: &IntegerValue) -> bool {
    value.runtime_kind() == Some(IntegerKind::Int64)
}

fn json_exact_nominal_type(value: &Type, expected_name: &str) -> bool {
    matches!(
        value,
        Type::Named(name, arguments) if name == expected_name && arguments.is_empty()
    )
}

pub(crate) fn json_array_metadata_is_exact(value: &VecValue) -> bool {
    json_exact_nominal_type(&value.element_type, "json.Value")
}

pub(crate) fn json_object_metadata_is_exact(value: &MapValue) -> bool {
    json_exact_nominal_type(&value.key_type, "str")
        && json_exact_nominal_type(&value.value_type, "json.Value")
}

fn json_runtime_child_depth(depth: usize) -> Result<usize> {
    let child_depth = depth.saturating_add(1);
    if child_depth <= json_codec::MAX_JSON_DEPTH {
        Ok(child_depth)
    } else {
        Err(json_dump_error_to_diagnostic(
            JsonCodecError::NestingTooDeep {
                limit: json_codec::MAX_JSON_DEPTH,
                line: 0,
                column: 0,
                offset: 0,
            },
        ))
    }
}

fn json_runtime_allocation_error(error: std::collections::TryReserveError) -> Diagnostic {
    Diagnostic::coded(
        "AU4005",
        format!("memory allocation failed while preparing JSON output: {error}"),
    )
}

fn json_runtime_conversion_try_reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<()> {
    if additional == 0 {
        return Ok(());
    }
    json_runtime_conversion_allocation_checkpoint()?;
    values
        .try_reserve(additional)
        .map_err(json_runtime_allocation_error)
}

fn json_error_variant(variant_name: &str, payloads: Vec<Value>) -> Result<Value> {
    json_runtime_enum_value("json.Error", variant_name, payloads)
}

fn json_location_payload(line: usize, column: usize) -> Result<Vec<Value>> {
    let mut payloads = Vec::new();
    json_parse_try_reserve(&mut payloads, 2)?;
    payloads.push(Value::Int(IntegerValue::from_i32(
        i32::try_from(line).expect("JSON input limit keeps error lines inside `int32`"),
    )));
    payloads.push(Value::Int(IntegerValue::from_i32(
        i32::try_from(column).expect("JSON input limit keeps error columns inside `int32`"),
    )));
    Ok(payloads)
}

fn json_parse_error_value(error: JsonCodecError) -> Result<Value> {
    match error {
        JsonCodecError::Syntax {
            message,
            line,
            column,
            ..
        } => {
            let mut payloads = Vec::new();
            json_parse_try_reserve(&mut payloads, 3)?;
            payloads.push(Value::String(message));
            payloads.extend(json_location_payload(line, column)?);
            json_error_variant("Syntax", payloads)
        }
        JsonCodecError::NumberOutOfRange { line, column, .. } => {
            json_error_variant("NumberOutOfRange", json_location_payload(line, column)?)
        }
        JsonCodecError::NestingTooDeep {
            limit,
            line,
            column,
            ..
        } => {
            let mut payloads = Vec::new();
            json_parse_try_reserve(&mut payloads, 3)?;
            payloads.push(Value::Int(IntegerValue::from_i32(
                i32::try_from(limit).expect("JSON depth limit fits `int32`"),
            )));
            payloads.extend(json_location_payload(line, column)?);
            json_error_variant("NestingTooDeep", payloads)
        }
        JsonCodecError::InputTooLarge {
            actual_bytes,
            limit_bytes,
        } => {
            let mut payloads = Vec::new();
            json_parse_try_reserve(&mut payloads, 2)?;
            payloads.push(Value::Int(IntegerValue::from_i64(
                i64::try_from(actual_bytes)
                    .expect("JSON input size is bounded by Aura String capacity"),
            )));
            payloads.push(Value::Int(IntegerValue::from_i64(
                i64::try_from(limit_bytes).expect("JSON input limit fits `int64`"),
            )));
            json_error_variant("InputTooLarge", payloads)
        }
        JsonCodecError::InvalidIndent { .. }
        | JsonCodecError::NonFiniteNumber
        | JsonCodecError::OutputTooLarge { .. }
        | JsonCodecError::MaterializationTooLarge { .. }
        | JsonCodecError::AllocationFailed => {
            unreachable!("JSON parse resource failures are diagnostics, not json.Error values")
        }
    }
}

pub(crate) fn json_dump_error_to_diagnostic(error: JsonCodecError) -> Diagnostic {
    let code = match error {
        JsonCodecError::InvalidIndent { .. } | JsonCodecError::NestingTooDeep { .. } => "AU4003",
        JsonCodecError::NonFiniteNumber => "AU4001",
        JsonCodecError::OutputTooLarge { .. }
        | JsonCodecError::MaterializationTooLarge { .. }
        | JsonCodecError::AllocationFailed => "AU4005",
        JsonCodecError::Syntax { .. }
        | JsonCodecError::NumberOutOfRange { .. }
        | JsonCodecError::InputTooLarge { .. } => {
            unreachable!("json.dumps only returns serialization errors")
        }
    };
    Diagnostic::coded(code, error.to_string())
}

fn host_json_variant<'a>(value: &'a Value, call: &str) -> Result<&'a EnumVariantValue> {
    match value {
        Value::EnumVariant(variant)
            if nominal_runtime_base_name(&variant.enum_name) == "json.Value" =>
        {
            Ok(variant)
        }
        other => Err(Diagnostic::coded(
            "AU4001",
            format!(
                "`{call}` expected a runtime `json.Value`, found `{}`",
                other.render()
            ),
        )),
    }
}

fn host_json_exact_payload(
    value: &Value,
    expected_variant: &str,
    call: &str,
) -> Result<Option<Value>> {
    let variant = host_json_variant(value, call)?;
    if variant.variant_name != expected_variant {
        return Ok(None);
    }
    match variant.payloads.as_slice() {
        [payload] => Ok(Some(payload.clone())),
        _ => Err(Diagnostic::coded(
            "AU4001",
            format!("malformed runtime `json.Value.{expected_variant}` payload in `{call}`"),
        )),
    }
}

fn host_json_into_exact_payload(
    value: Value,
    expected_variant: &str,
    call: &str,
) -> Result<Option<Value>> {
    let Value::EnumVariant(mut variant) = value else {
        return Err(Diagnostic::coded(
            "AU4001",
            format!("`{call}` expected a runtime `json.Value`"),
        ));
    };
    if nominal_runtime_base_name(&variant.enum_name) != "json.Value" {
        return Err(Diagnostic::coded(
            "AU4001",
            format!(
                "`{call}` expected enum `json.Value`, found `{}`",
                nominal_runtime_base_name(&variant.enum_name)
            ),
        ));
    }
    if variant.variant_name != expected_variant {
        return Ok(None);
    }
    if variant.payloads.len() != 1 {
        return Err(Diagnostic::coded(
            "AU4001",
            format!("malformed runtime `json.Value.{expected_variant}` payload in `{call}`"),
        ));
    }
    Ok(variant.payloads.pop())
}

fn host_json_indent_arg(value: &Value) -> Result<Option<i64>> {
    let Value::EnumVariant(option) = value else {
        return Err(Diagnostic::coded(
            "AU4001",
            "`json::dumps` expects `indent` to be `Option[int64]`",
        ));
    };
    match (
        nominal_runtime_base_name(&option.enum_name),
        option.variant_name.as_str(),
        option.payloads.as_slice(),
    ) {
        ("Option", "None", []) => Ok(None),
        ("Option", "Some", [Value::Int(value)]) => {
            if !json_int_metadata_is_exact(value) {
                return Err(Diagnostic::coded(
                    "AU4001",
                    "`json::dumps` expects `indent` to contain an `int64`",
                ));
            }
            let indent = value
                .as_i128()
                .and_then(|value| i64::try_from(value).ok())
                .expect("exact int64 metadata guarantees an int64 runtime value");
            Ok(Some(indent))
        }
        _ => Err(Diagnostic::coded(
            "AU4001",
            "`json::dumps` expects `indent` to be `Option[int64]`",
        )),
    }
}

fn legacy_json_value_is_finite(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => {
            number.as_i64().is_some()
                || number.as_u64().is_some()
                || number.as_f64().is_some_and(f64::is_finite)
        }
        serde_json::Value::Array(values) => values.iter().all(legacy_json_value_is_finite),
        serde_json::Value::Object(entries) => entries.values().all(legacy_json_value_is_finite),
        _ => true,
    }
}

fn legacy_json_is_valid(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .is_ok_and(|value| legacy_json_value_is_finite(&value))
}

fn evaluate_host_builtin_with_args(
    name: &str,
    args: Vec<Value>,
    program_args: Option<&[String]>,
) -> Result<Value> {
    if bytes_host_builtin_name(name) {
        host_expect_arity(name, &args, 1)?;
        return evaluate_bytes_host_builtin_ref(name, &args[0])
            .expect("recognized byte host builtin should be dispatched");
    }

    match name {
        "math::floor" | "math::ceil" | "math::trunc" => {
            host_expect_arity(name, &args, 1)?;
            let display_name = name.replace("::", ".");
            let Value::Float(value) = args[0] else {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!("`{display_name}` expects `float64`"),
                ));
            };
            let rounded = match name {
                "math::floor" => value.floor(),
                "math::ceil" => value.ceil(),
                "math::trunc" => value.trunc(),
                _ => unreachable!(),
            };
            // `i64::MAX as f64` rounds to 2^63, so the upper bound is
            // intentionally exclusive while -2^63 remains representable.
            const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
            const I64_LIMIT_F64: f64 = 9_223_372_036_854_775_808.0;
            if !rounded.is_finite() || !(I64_MIN_F64..I64_LIMIT_F64).contains(&rounded) {
                return Err(Diagnostic::coded(
                    "AU4002",
                    format!("`{display_name}` result cannot be represented as `int64`"),
                ));
            }
            let integer =
                IntegerValue::from_typed_signed(rounded as i64 as i128, IntegerKind::Int64)
                    .expect("validated math conversion must fit int64");
            Ok(Value::Int(integer))
        }
        "math::pow" => {
            host_expect_arity(name, &args, 2)?;
            let (Value::Float(base), Value::Float(exponent)) = (&args[0], &args[1]) else {
                return Err(Diagnostic::coded(
                    "AU4001",
                    "`math.pow` expects two `float64` arguments",
                ));
            };
            float_power(*base, *exponent, FloatPowerWidth::Float64).map(Value::Float)
        }
        "math::exp" | "math::log" | "math::log2" | "math::log10" | "math::sin" | "math::cos"
        | "math::tan" => {
            host_expect_arity(name, &args, 1)?;
            let display_name = name.replace("::", ".");
            let Value::Float(value) = args[0] else {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!("`{display_name}` expects `float64`"),
                ));
            };
            if matches!(name, "math::log" | "math::log2" | "math::log10")
                && value.is_finite()
                && value <= 0.0
            {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!("`{display_name}` domain error for `{value}`"),
                ));
            }
            if matches!(name, "math::sin" | "math::cos" | "math::tan") && value.is_infinite() {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!("`{display_name}` domain error for `{value}`"),
                ));
            }
            let result = match name {
                "math::exp" => value.exp(),
                "math::log" => value.ln(),
                "math::log2" => value.log2(),
                "math::log10" => value.log10(),
                "math::sin" => value.sin(),
                "math::cos" => value.cos(),
                "math::tan" => value.tan(),
                _ => unreachable!(),
            };
            if name == "math::exp" && value.is_finite() && result.is_infinite() {
                return Err(Diagnostic::coded("AU4002", "`math.exp` overflow"));
            }
            Ok(Value::Float(result))
        }
        "control::__retry_validate" => {
            host_expect_arity(name, &args, 2)?;
            let max_attempts = match &args[0] {
                Value::Int(value) => value
                    .as_i128()
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| {
                        Diagnostic::coded("AU4001", "control.retry max_attempts must be an int32")
                    })?,
                _ => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "control.retry max_attempts must be an int32",
                    ))
                }
            };
            let initial_backoff = match args[1] {
                Value::Duration(value) => value,
                _ => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "control.retry initial_backoff must be a Duration",
                    ))
                }
            };
            validate_retry_runtime_policy(max_attempts, initial_backoff)?;
            Ok(Value::Unit)
        }
        "control::__retry_next_backoff" => {
            host_expect_arity(name, &args, 1)?;
            let backoff = match args[0] {
                Value::Duration(value) => value,
                _ => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "control.retry backoff must be a Duration",
                    ))
                }
            };
            next_retry_runtime_backoff(backoff).map(Value::Duration)
        }
        "control::__retry_cancel_if_requested" => {
            host_expect_arity(name, &args, 0)?;
            propagate_retry_task_cancellation();
            Ok(Value::Unit)
        }
        "sys::args" => {
            host_expect_arity(name, &args, 0)?;
            Ok(Value::Vec(VecValue {
                element_type: Type::named("str"),
                elements: program_args
                    .map(<[String]>::to_vec)
                    .unwrap_or_else(host_process_args)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            }))
        }
        "sys::env" => {
            host_expect_arity(name, &args, 1)?;
            let name = host_string_arg(&args, 0, name)?;
            Ok(std::env::var(name)
                .ok()
                .map(Value::String)
                .map(option_some)
                .unwrap_or_else(option_none))
        }
        "sys::current_dir" => {
            host_expect_arity(name, &args, 0)?;
            Ok(match std::env::current_dir() {
                Ok(path) => result_ok(Value::String(path.to_string_lossy().to_string())),
                Err(error) => result_err(io_error(error)),
            })
        }
        "sys::unix_time_ms" => {
            host_expect_arity(name, &args, 0)?;
            let duration = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            {
                Ok(duration) => duration,
                Err(error) => {
                    return Err(Diagnostic::new(format!(
                        "system clock is before unix epoch: {error}"
                    )))
                }
            };
            host_millis_value(duration.as_millis(), "unix time")
        }
        "sys::monotonic_time_ms" => {
            host_expect_arity(name, &args, 0)?;
            let millis = HOST_MONOTONIC_EPOCH
                .get_or_init(Instant::now)
                .elapsed()
                .as_millis();
            host_millis_value(millis, "monotonic time")
        }
        "path::join" => {
            host_expect_arity(name, &args, 2)?;
            let base = host_string_arg(&args, 0, name)?;
            let child = host_string_arg(&args, 1, name)?;
            Ok(Value::String(
                Path::new(&base).join(child).to_string_lossy().to_string(),
            ))
        }
        "path::parent" | "path::file_name" | "path::extension" => {
            host_expect_arity(name, &args, 1)?;
            let path = host_string_arg(&args, 0, name)?;
            let path = Path::new(&path);
            let value = match name {
                "path::parent" => path
                    .parent()
                    .map(|value| value.to_string_lossy().to_string()),
                "path::file_name" => path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string()),
                "path::extension" => path
                    .extension()
                    .map(|value| value.to_string_lossy().to_string()),
                _ => unreachable!(),
            };
            Ok(value
                .map(Value::String)
                .map(option_some)
                .unwrap_or_else(option_none))
        }
        "path::is_absolute" => {
            host_expect_arity(name, &args, 1)?;
            Ok(Value::Bool(
                Path::new(&host_string_arg(&args, 0, name)?).is_absolute(),
            ))
        }
        "json::parse" => {
            host_expect_arity(name, &args, 1)?;
            let (source, reservation) = prepare_json_codec_source(|| {
                clone_json_codec_source(host_string_ref_arg(&args, 0, name)?)
            })?;
            json_parse_owned_to_runtime(source, reservation)
        }
        "json::dumps" => {
            host_expect_arity(name, &args, 2)?;
            let indent = host_json_indent_arg(&args[1])?;
            let value = runtime_value_to_json(&args[0])?;
            json_codec::dumps(&value, indent)
                .map(Value::String)
                .map_err(json_dump_error_to_diagnostic)
        }
        "json::is_null" => {
            host_expect_arity(name, &args, 1)?;
            let variant = host_json_variant(&args[0], name)?;
            if variant.variant_name == "Null" && !variant.payloads.is_empty() {
                return Err(Diagnostic::coded(
                    "AU4001",
                    "malformed runtime `json.Value.Null` payload in `json::is_null`",
                ));
            }
            Ok(Value::Bool(variant.variant_name == "Null"))
        }
        "json::as_bool" => {
            host_expect_arity(name, &args, 1)?;
            Ok(match host_json_exact_payload(&args[0], "Bool", name)? {
                Some(Value::Bool(value)) => option_some(Value::Bool(value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::as_int" => {
            host_expect_arity(name, &args, 1)?;
            Ok(match host_json_exact_payload(&args[0], "Int", name)? {
                Some(Value::Int(value)) if json_int_metadata_is_exact(&value) => {
                    option_some(Value::Int(value))
                }
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Int` payload in `json::as_int`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::as_float" => {
            host_expect_arity(name, &args, 1)?;
            Ok(match host_json_exact_payload(&args[0], "Float", name)? {
                Some(Value::Float(value)) => option_some(Value::Float(value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Float` payload in `json::as_float`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::into_string" => {
            host_expect_arity(name, &args, 1)?;
            let value = args
                .into_iter()
                .next()
                .expect("validated host builtin arity provides one argument");
            Ok(match host_json_into_exact_payload(value, "String", name)? {
                Some(Value::String(value)) => option_some(Value::String(value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.String` payload in `json::into_string`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::into_array" => {
            host_expect_arity(name, &args, 1)?;
            let value = args
                .into_iter()
                .next()
                .expect("validated host builtin arity provides one argument");
            Ok(match host_json_into_exact_payload(value, "Array", name)? {
                Some(Value::Vec(value)) if json_array_metadata_is_exact(&value) => {
                    option_some(Value::Vec(value))
                }
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Array` payload in `json::into_array`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::into_object" => {
            host_expect_arity(name, &args, 1)?;
            let value = args
                .into_iter()
                .next()
                .expect("validated host builtin arity provides one argument");
            Ok(match host_json_into_exact_payload(value, "Object", name)? {
                Some(Value::Map(value)) if json_object_metadata_is_exact(&value) => {
                    option_some(Value::Map(value))
                }
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Object` payload in `json::into_object`",
                    ))
                }
                None => option_none(),
            })
        }
        "json::is_valid" => {
            host_expect_arity(name, &args, 1)?;
            Ok(Value::Bool(legacy_json_is_valid(&host_string_arg(
                &args, 0, name,
            )?)))
        }
        "json::stringify_map" => {
            host_expect_arity(name, &args, 1)?;
            let value = host_string_map_arg(&args, 0, name)?;
            Ok(match serde_json::to_string(&value) {
                Ok(text) => result_ok(Value::String(text)),
                Err(error) => result_err(Value::String(error.to_string())),
            })
        }
        "json::parse_string_map" => {
            host_expect_arity(name, &args, 1)?;
            let text = host_string_arg(&args, 0, name)?;
            Ok(
                match serde_json::from_str::<BTreeMap<String, String>>(&text) {
                    Ok(value) => result_ok(host_string_map_value(value)),
                    Err(error) => result_err(Value::String(error.to_string())),
                },
            )
        }
        "toml::is_valid" => {
            host_expect_arity(name, &args, 1)?;
            Ok(Value::Bool(
                toml::from_str::<toml::Value>(&host_string_arg(&args, 0, name)?).is_ok(),
            ))
        }
        "toml::stringify_map" => {
            host_expect_arity(name, &args, 1)?;
            let value = host_string_map_arg(&args, 0, name)?;
            Ok(match toml::to_string(&value) {
                Ok(text) => result_ok(Value::String(text)),
                Err(error) => result_err(Value::String(error.to_string())),
            })
        }
        "toml::parse_string_map" => {
            host_expect_arity(name, &args, 1)?;
            let text = host_string_arg(&args, 0, name)?;
            Ok(match toml::from_str::<BTreeMap<String, String>>(&text) {
                Ok(value) => result_ok(host_string_map_value(value)),
                Err(error) => result_err(Value::String(error.to_string())),
            })
        }
        "metrics::increment" => {
            host_expect_arity(name, &args, 2)?;
            let metric = host_string_arg(&args, 0, name)?;
            let Some(Value::Int(value)) = args.get(1) else {
                return Err(Diagnostic::new(
                    "`metrics.increment` expects `int64` for `value`",
                ));
            };
            let value = value
                .as_i128()
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| Diagnostic::new("metric increment does not fit in `int64`"))?;
            let mut metrics = lock_mutex(HOST_METRICS.get_or_init(|| Mutex::new(BTreeMap::new())));
            let entry = metrics.entry(metric).or_insert(0);
            *entry = entry
                .checked_add(value)
                .ok_or_else(|| Diagnostic::new("metric value overflowed `int64`"))?;
            Ok(Value::Unit)
        }
        "metrics::get" => {
            host_expect_arity(name, &args, 1)?;
            let metric = host_string_arg(&args, 0, name)?;
            let metrics = lock_mutex(HOST_METRICS.get_or_init(|| Mutex::new(BTreeMap::new())));
            Ok(Value::Int(IntegerValue::from_signed(i128::from(
                metrics.get(&metric).copied().unwrap_or(0),
            ))))
        }
        "metrics::reset" => {
            host_expect_arity(name, &args, 0)?;
            lock_mutex(HOST_METRICS.get_or_init(|| Mutex::new(BTreeMap::new()))).clear();
            Ok(Value::Unit)
        }
        "log::debug" | "log::info" | "log::warn" | "log::error" | "trace::event" => {
            host_expect_arity(name, &args, 2)?;
            let message = host_string_arg(&args, 0, name)?;
            let fields = host_string_map_arg(&args, 1, name)?;
            let record = serde_json::json!({
                "kind": if name == "trace::event" { "trace" } else { "log" },
                "level": name.split_once("::").map(|(_, value)| value).unwrap_or(name),
                "message": message,
                "fields": fields,
            });
            eprintln!("{record}");
            Ok(Value::Unit)
        }
        _ => Err(Diagnostic::new(format!("unknown host builtin `{name}`"))),
    }
}

pub(crate) fn evaluate_host_builtin(name: &str, args: Vec<Value>) -> Result<Value> {
    evaluate_host_builtin_with_args(name, args, None)
}

pub(crate) fn evaluate_host_builtin_with_program_args(
    name: &str,
    args: Vec<Value>,
    program_args: &[String],
) -> Result<Value> {
    evaluate_host_builtin_with_args(name, args, Some(program_args))
}

pub(crate) fn send_error_closed(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SendError".to_string(),
        variant_name: "Closed".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn send_error_cancelled(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SendError".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn send_error_timed_out(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SendError".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn send_error_full(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SendError".to_string(),
        variant_name: "Full".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn queue_receive_item(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "QueueReceive".to_string(),
        variant_name: "Item".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn queue_receive_closed() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "QueueReceive".to_string(),
        variant_name: "Closed".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn queue_receive_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "QueueReceive".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn queue_receive_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "QueueReceive".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn task_result_ready(value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "TaskResult".to_string(),
        variant_name: "Ready".to_string(),
        payloads: vec![value],
    })
}

pub(crate) fn task_result_error(message: String) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "TaskResult".to_string(),
        variant_name: "Error".to_string(),
        payloads: vec![Value::String(message)],
    })
}

pub(crate) fn task_result_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "TaskResult".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn task_result_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "TaskResult".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn select_outcome_queue(index: i64, outcome: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SelectOutcome".to_string(),
        variant_name: "Queue".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i64(index)), outcome],
    })
}

pub(crate) fn select_outcome_task(index: i64, outcome: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SelectOutcome".to_string(),
        variant_name: "Task".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i64(index)), outcome],
    })
}

pub(crate) fn select_outcome_deadline(index: i64) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SelectOutcome".to_string(),
        variant_name: "Deadline".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i64(index))],
    })
}

pub(crate) fn select_outcome_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SelectOutcome".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn wait_any_ready(index: i64, value: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAny".to_string(),
        variant_name: "Ready".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i64(index)), value],
    })
}

pub(crate) fn wait_any_error(index: i64, message: String) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAny".to_string(),
        variant_name: "Error".to_string(),
        payloads: vec![
            Value::Int(IntegerValue::from_i64(index)),
            Value::String(message),
        ],
    })
}

pub(crate) fn wait_any_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAny".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn wait_any_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAny".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn wait_all_ready(values: Vec<Value>) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAll".to_string(),
        variant_name: "Ready".to_string(),
        payloads: vec![Value::Vec(VecValue {
            element_type: Type::named("Unknown"),
            elements: values,
        })],
    })
}

pub(crate) fn wait_all_error(index: i64, message: String) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAll".to_string(),
        variant_name: "Error".to_string(),
        payloads: vec![
            Value::Int(IntegerValue::from_i64(index)),
            Value::String(message),
        ],
    })
}

pub(crate) fn wait_all_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAll".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn wait_all_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "WaitAll".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_stdio_inherit() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Stdio".to_string(),
        variant_name: "Inherit".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_stdio_null() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Stdio".to_string(),
        variant_name: "Null".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_stdio_pipe() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Stdio".to_string(),
        variant_name: "Pipe".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_exit_status(status: StdExitStatus) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Value::EnumVariant(EnumVariantValue {
                enum_name: "ExitStatus".to_string(),
                variant_name: "Signaled".to_string(),
                payloads: vec![Value::Int(IntegerValue::from_signed(signal as i128))],
            });
        }
    }
    let code = status.code().unwrap_or_default();
    Value::EnumVariant(EnumVariantValue {
        enum_name: "ExitStatus".to_string(),
        variant_name: "Exited".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_signed(code as i128))],
    })
}

pub(crate) fn process_wait_exited(status: StdExitStatus) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Wait".to_string(),
        variant_name: "Exited".to_string(),
        payloads: vec![process_exit_status(status)],
    })
}

pub(crate) fn process_wait_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Wait".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_wait_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Wait".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_wait_failed(error: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Wait".to_string(),
        variant_name: "Failed".to_string(),
        payloads: vec![error],
    })
}

pub(crate) fn process_supervisor_wait_event(event: Value) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorWait".to_string(),
        variant_name: "Event".to_string(),
        payloads: vec![event],
    })
}

pub(crate) fn process_supervisor_wait_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorWait".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_supervisor_wait_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorWait".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_supervisor_event_exited(
    name: String,
    status: StdExitStatus,
    restart_count: IntegerValue,
) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorEvent".to_string(),
        variant_name: "Exited".to_string(),
        payloads: vec![
            Value::String(name),
            process_exit_status(status),
            Value::Int(restart_count),
        ],
    })
}

pub(crate) fn process_supervisor_event_restarted(
    name: String,
    status: StdExitStatus,
    restart_count: IntegerValue,
) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorEvent".to_string(),
        variant_name: "Restarted".to_string(),
        payloads: vec![
            Value::String(name),
            process_exit_status(status),
            Value::Int(restart_count),
        ],
    })
}

pub(crate) fn process_supervisor_event_failed(
    name: String,
    error: Value,
    restart_count: IntegerValue,
) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "SupervisorEvent".to_string(),
        variant_name: "Failed".to_string(),
        payloads: vec![Value::String(name), error, Value::Int(restart_count)],
    })
}

pub(crate) fn process_error_no_command() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "NoCommand".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_error_timed_out() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "TimedOut".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_error_cancelled() -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "Cancelled".to_string(),
        payloads: Vec::new(),
    })
}

pub(crate) fn process_error_io(error: io::Error) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "Io".to_string(),
        payloads: vec![io_error(error)],
    })
}

pub(crate) fn process_error_spawn(message: String) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "Spawn".to_string(),
        payloads: vec![Value::String(message)],
    })
}

pub(crate) fn process_error_other(message: String) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "Error".to_string(),
        variant_name: "Other".to_string(),
        payloads: vec![Value::String(message)],
    })
}

pub(crate) fn decode_process_stdio(value: &Value, label: &str) -> Result<ProcessStdioConfig> {
    match value {
        Value::EnumVariant(variant)
            if matches!(variant.enum_name.as_str(), "Stdio" | "process.Stdio") =>
        {
            match variant.variant_name.as_str() {
                "Inherit" => Ok(ProcessStdioConfig::Inherit),
                "Null" => Ok(ProcessStdioConfig::Null),
                "Pipe" => Ok(ProcessStdioConfig::Pipe),
                _ => Err(Diagnostic::new(format!(
                    "`{}` received an unknown `process.Stdio` variant `{}`",
                    label, variant.variant_name
                ))),
            }
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `process.Stdio`, found `{}`",
            label,
            other.render()
        ))),
    }
}

pub(crate) fn decode_process_restart_policy(
    value: &Value,
    label: &str,
) -> Result<ProcessRestartPolicy> {
    match value {
        Value::EnumVariant(variant)
            if matches!(
                variant.enum_name.as_str(),
                "RestartPolicy" | "process.RestartPolicy"
            ) =>
        {
            match variant.variant_name.as_str() {
                "Never" => Ok(ProcessRestartPolicy::Never),
                "OnFailure" => Ok(ProcessRestartPolicy::OnFailure),
                "Always" => Ok(ProcessRestartPolicy::Always),
                _ => Err(Diagnostic::new(format!(
                    "`{}` received an unknown `process.RestartPolicy` variant `{}`",
                    label, variant.variant_name
                ))),
            }
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `process.RestartPolicy`, found `{}`",
            label,
            other.render()
        ))),
    }
}

pub(crate) fn io_error(error: io::Error) -> Value {
    let (variant_name, payloads) = match error.kind() {
        io::ErrorKind::NotFound => ("NotFound", Vec::new()),
        io::ErrorKind::PermissionDenied => ("PermissionDenied", Vec::new()),
        io::ErrorKind::AlreadyExists => ("AlreadyExists", Vec::new()),
        io::ErrorKind::IsADirectory => ("IsDirectory", Vec::new()),
        io::ErrorKind::ConnectionRefused => ("ConnectionRefused", Vec::new()),
        io::ErrorKind::ConnectionReset => ("ConnectionReset", Vec::new()),
        io::ErrorKind::ConnectionAborted => ("ConnectionAborted", Vec::new()),
        io::ErrorKind::NotConnected => ("NotConnected", Vec::new()),
        io::ErrorKind::AddrInUse => ("AddrInUse", Vec::new()),
        io::ErrorKind::AddrNotAvailable => ("AddrNotAvailable", Vec::new()),
        io::ErrorKind::BrokenPipe if error.to_string() == "resource is closed" => {
            ("Closed", Vec::new())
        }
        io::ErrorKind::Interrupted if error.to_string() == "operation cancelled" => {
            ("Cancelled", Vec::new())
        }
        io::ErrorKind::BrokenPipe => ("BrokenPipe", Vec::new()),
        io::ErrorKind::TimedOut => ("TimedOut", Vec::new()),
        io::ErrorKind::WouldBlock => ("WouldBlock", Vec::new()),
        io::ErrorKind::UnexpectedEof => ("UnexpectedEof", Vec::new()),
        io::ErrorKind::InvalidInput => ("InvalidInput", Vec::new()),
        io::ErrorKind::InvalidData => ("InvalidData", Vec::new()),
        _ => ("Other", vec![Value::String(error.to_string())]),
    };
    Value::EnumVariant(EnumVariantValue {
        enum_name: "io.Error".to_string(),
        variant_name: variant_name.to_string(),
        payloads,
    })
}

#[cfg(test)]
#[path = "runtime_value_tests.rs"]
mod tests;
