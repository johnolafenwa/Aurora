#![cfg_attr(coverage, allow(dead_code))]

use std::borrow::Borrow;
#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::mem;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::process;
use std::slice;
use std::str;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration as StdDuration, Instant};

use crate::ast::{BinaryOp, ReceiverKind, UnaryOp};
use crate::builtin_modules::host_builtin_metadata;
use crate::diag::{Diagnostic, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame, Span};
use crate::ffi::{FfiError, FfiSignature, FfiType, FfiValue, OpaqueHandle};
use crate::integer::{
    IntegerKind, IntegerPowerError, IntegerRepresentation, IntegerShiftError, IntegerValue,
};
use crate::json_codec;
use crate::randomness::{self, SecureRandomError};
use crate::runtime_value::{
    cancel_current_lightweight_task_boundary, cast_numeric_value, claim_task_result_observations,
    clone_json_codec_source, collect_queue_values, concat_strings_checked,
    current_lightweight_task_cancellation, current_lightweight_task_id,
    decode_process_restart_policy, decode_process_stdio, divmod_numeric_values,
    embedded_nominal_runtime_type_name, evaluate_bytes_host_builtin_ref, evaluate_host_builtin,
    fail_current_lightweight_task, float_floor_divmod, float_power, format_runtime_value, io_error,
    io_read_line, json_array_metadata_is_exact, json_dump_error_to_diagnostic,
    json_int_metadata_is_exact, json_object_metadata_is_exact, json_parse_owned_to_runtime,
    nominal_runtime_base_name, option_none, option_some, poll_cancellation,
    prepare_json_codec_source, process_error_cancelled, process_error_io, process_error_no_command,
    process_error_spawn, process_error_timed_out, process_exit_status, process_stdio_inherit,
    process_stdio_null, process_stdio_pipe, process_supervisor_event_failed,
    process_supervisor_wait_cancelled, process_supervisor_wait_event,
    process_supervisor_wait_timed_out, process_wait_cancelled, process_wait_exited,
    process_wait_failed, process_wait_timed_out, queue_receive_cancelled, queue_receive_closed,
    queue_receive_item, queue_receive_timed_out, read_file_limited,
    recv_for_registered_producers_iteration, recv_for_task_group_iteration, render_float,
    render_float32, result_err, result_ok, round_numeric_value, run_blocking_io,
    run_lightweight_root_task_with_forced_exit_cleanup, runtime_value_to_json,
    select_runtime_values, send_error_cancelled, send_error_closed, send_error_full,
    send_error_timed_out, sleep_with_runtime_scheduler, slice_string_owned, slice_vec_owned,
    spawn_lightweight_task_with_cancellation,
    spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack_and_result_repeatability_registered,
    task_group_cleanup_should_cancel, task_result_cancelled, task_result_error, task_result_ready,
    task_result_timed_out, try_array_buffer, try_clone_array_containing_value, wait_all_cancelled,
    wait_all_error, wait_all_ready, wait_all_timed_out, wait_any_cancelled, wait_any_error,
    wait_any_ready, wait_any_timed_out, wait_for_runtime_scheduler,
    yield_now_with_runtime_scheduler, ArrayBinaryOp, ArrayDType, ArrayReduction, ArrayValue,
    CancellationContext, ChannelValue, ClosureCaptureValue, ClosureEnvironment, EnumVariantValue,
    FfiHandleValue, FileValue, FloatPowerWidth, FunctionValue, HttpListenerValue,
    HttpResponseValue, InstanceValue, IntegerArithmeticMode, LightweightTaskFailureSignal,
    MapValue, ProcessChildValue, ProcessChildWaitStatus, ProcessCompletedValue,
    ProcessSupervisorValue, ProcessSupervisorWaitStatus, RangeValue, RecvValueResult, RngValue,
    RuntimeSchedulerWakeReason, SendValueError, SetValue, TaskCancelledSignal, TaskGroupValue,
    TaskValue, TaskWaitStatus, TcpListenerValue, TcpStreamValue, TlsListenerValue, TlsStreamValue,
    TupleValue, UdpSocketValue, UnixListenerValue, UnixStreamValue, Value, VecValue,
    WebSocketListenerValue, WebSocketValue, DIRECT_RUNTIME_TYPE_FIELD,
    DIRECT_RUNTIME_TYPE_SEPARATOR,
};
use crate::sema::Type;

const DIRECT_FFI_SPEC_MAGIC: &[u8; 4] = b"AUFI";
const DIRECT_FFI_SPEC_VERSION: u8 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectFfiType {
    pub ffi_type: FfiType,
    pub opaque_name: Option<String>,
}

impl DirectFfiType {
    pub(crate) fn scalar(ffi_type: FfiType) -> Self {
        debug_assert_ne!(ffi_type, FfiType::OpaqueHandle);
        Self {
            ffi_type,
            opaque_name: None,
        }
    }

    pub(crate) fn opaque(name: impl Into<String>) -> Self {
        Self {
            ffi_type: FfiType::OpaqueHandle,
            opaque_name: Some(name.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectFfiParam {
    pub passing: ReceiverKind,
    pub ty: DirectFfiType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectFfiCallSpec {
    pub symbol: String,
    pub params: Vec<DirectFfiParam>,
    pub result: DirectFfiType,
}

struct DirectCleanupRegistration {
    id: i64,
    thunk_ptr: i64,
    args: *mut i64,
    arg_count: usize,
    call_depth: usize,
}

impl DirectCleanupRegistration {
    unsafe fn release_args(&mut self) {
        let args = std::mem::replace(&mut self.args, std::ptr::null_mut());
        let arg_count = std::mem::take(&mut self.arg_count);
        unsafe {
            release_direct_cleanup_args(args, arg_count);
        }
    }
}

impl Drop for DirectCleanupRegistration {
    fn drop(&mut self) {
        unsafe {
            self.release_args();
        }
    }
}

#[derive(Clone, Copy)]
struct DirectStaticFrameText {
    address: usize,
    len: usize,
}

impl DirectStaticFrameText {
    unsafe fn validate(ptr: *const u8, len: usize) -> std::result::Result<Self, ()> {
        if ptr.is_null() {
            return Err(());
        }
        let bytes = unsafe { slice::from_raw_parts(ptr, len) };
        str::from_utf8(bytes).map_err(|_| ())?;
        Ok(Self {
            address: ptr as usize,
            len,
        })
    }

    fn as_str(&self) -> &str {
        // SAFETY: the private generated-code ABI requires frame metadata to
        // remain readable and unchanged for the active call or spawned task
        // lifetime, and `validate` establishes UTF-8 before this handle enters
        // runtime state.
        let bytes = unsafe { slice::from_raw_parts(self.address as *const u8, self.len) };
        unsafe { str::from_utf8_unchecked(bytes) }
    }
}

#[derive(Clone)]
enum DirectFrameText {
    Static(DirectStaticFrameText),
    Shared(Arc<str>),
}

impl DirectFrameText {
    unsafe fn validate_static(ptr: *const u8, len: usize) -> std::result::Result<Self, ()> {
        unsafe { DirectStaticFrameText::validate(ptr, len) }.map(Self::Static)
    }

    fn shared(value: String) -> Self {
        Self::Shared(Arc::from(value))
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Static(value) => value.as_str(),
            Self::Shared(value) => value,
        }
    }

    fn materialize(&self) -> String {
        self.as_str().to_string()
    }
}

#[derive(Clone)]
struct DirectRuntimeSourceSpan {
    path: Option<DirectFrameText>,
    start: Span,
    end: Span,
}

impl DirectRuntimeSourceSpan {
    fn point(path: Option<DirectFrameText>, start: Span) -> Self {
        Self {
            path,
            start,
            end: Span::new(start.line, start.column.saturating_add(1)),
        }
    }

    fn materialize(&self) -> RuntimeSourceSpan {
        RuntimeSourceSpan {
            path: self.path.as_ref().map(DirectFrameText::materialize),
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Clone)]
struct DirectRuntimeCallFrame {
    function: DirectFrameText,
    span: DirectRuntimeSourceSpan,
}

impl DirectRuntimeCallFrame {
    fn materialize(&self) -> RuntimeCallFrame {
        note_direct_runtime_frame_materialized();
        RuntimeCallFrame {
            function: self.function.materialize(),
            span: self.span.materialize(),
        }
    }
}

#[derive(Clone)]
struct DirectRuntimeTaskFrame {
    task_function: DirectFrameText,
    task_entry_span: DirectRuntimeSourceSpan,
    parent_function: DirectFrameText,
    spawn_span: DirectRuntimeSourceSpan,
}

impl DirectRuntimeTaskFrame {
    fn materialize(&self) -> RuntimeTaskFrame {
        note_direct_runtime_frame_materialized();
        RuntimeTaskFrame {
            task_function: self.task_function.materialize(),
            task_entry_span: self.task_entry_span.materialize(),
            parent_function: self.parent_function.materialize(),
            spawn_span: self.spawn_span.materialize(),
        }
    }

    #[cfg(test)]
    fn from_runtime(frame: RuntimeTaskFrame) -> Self {
        Self {
            task_function: DirectFrameText::shared(frame.task_function),
            task_entry_span: DirectRuntimeSourceSpan {
                path: frame.task_entry_span.path.map(DirectFrameText::shared),
                start: frame.task_entry_span.start,
                end: frame.task_entry_span.end,
            },
            parent_function: DirectFrameText::shared(frame.parent_function),
            spawn_span: DirectRuntimeSourceSpan {
                path: frame.spawn_span.path.map(DirectFrameText::shared),
                start: frame.spawn_span.start,
                end: frame.spawn_span.end,
            },
        }
    }
}

#[derive(Clone)]
struct DirectTaskAncestryNode {
    frame: DirectRuntimeTaskFrame,
    parent: Option<Arc<DirectTaskAncestryNode>>,
}

#[derive(Clone, Default)]
struct DirectTaskAncestry {
    youngest: Option<Arc<DirectTaskAncestryNode>>,
    len: usize,
}

impl DirectTaskAncestry {
    fn prepend(&self, frame: DirectRuntimeTaskFrame) -> Self {
        Self {
            youngest: Some(Arc::new(DirectTaskAncestryNode {
                frame,
                parent: self.youngest.clone(),
            })),
            len: self.len + 1,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }

    fn materialize(&self) -> Vec<RuntimeTaskFrame> {
        let mut frames = Vec::with_capacity(self.len);
        let mut current = self.youngest.as_deref();
        while let Some(node) = current {
            frames.push(node.frame.materialize());
            current = node.parent.as_deref();
        }
        frames
    }

    #[cfg(test)]
    fn from_runtime(frames: Vec<RuntimeTaskFrame>) -> Self {
        frames
            .into_iter()
            .rev()
            .fold(Self::default(), |ancestry, frame| {
                ancestry.prepend(DirectRuntimeTaskFrame::from_runtime(frame))
            })
    }
}

impl Drop for DirectTaskAncestry {
    fn drop(&mut self) {
        let mut current = self.youngest.take();
        while let Some(node) = current {
            match Arc::try_unwrap(node) {
                Ok(mut node) => current = node.parent.take(),
                Err(shared) => {
                    // Another ancestry snapshot owns the rest of this
                    // persistent chain. Releasing this reference cannot retire
                    // the shared node, so its eventual last owner will perform
                    // the iterative teardown.
                    drop(shared);
                    break;
                }
            }
        }
    }
}

#[derive(Default)]
enum DirectCallFrameStorage {
    #[default]
    Empty,
    Inline(DirectRuntimeCallFrame),
    Spill(Vec<DirectRuntimeCallFrame>),
}

impl DirectCallFrameStorage {
    fn push(&mut self, frame: DirectRuntimeCallFrame) {
        match std::mem::take(self) {
            Self::Empty => *self = Self::Inline(frame),
            Self::Inline(first) => {
                let mut frames = Vec::with_capacity(4);
                frames.push(first);
                frames.push(frame);
                *self = Self::Spill(frames);
            }
            Self::Spill(mut frames) => {
                frames.push(frame);
                *self = Self::Spill(frames);
            }
        }
    }

    fn pop(&mut self) -> Option<DirectRuntimeCallFrame> {
        match std::mem::take(self) {
            Self::Empty => None,
            Self::Inline(frame) => Some(frame),
            Self::Spill(mut frames) => {
                let popped = frames.pop();
                *self = match frames.len() {
                    0 => Self::Empty,
                    1 => Self::Inline(frames.pop().expect("one frame remains")),
                    _ => Self::Spill(frames),
                };
                popped
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Inline(_) => 1,
            Self::Spill(frames) => frames.len(),
        }
    }

    #[cfg(test)]
    fn has_heap_spill(&self) -> bool {
        matches!(self, Self::Spill(_))
    }

    fn materialize_innermost_first(&self) -> Vec<RuntimeCallFrame> {
        match self {
            Self::Empty => Vec::new(),
            Self::Inline(frame) => vec![frame.materialize()],
            Self::Spill(frames) => frames
                .iter()
                .rev()
                .map(DirectRuntimeCallFrame::materialize)
                .collect(),
        }
    }
}

#[cfg(test)]
thread_local! {
    static DIRECT_RUNTIME_FRAME_MATERIALIZATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn note_direct_runtime_frame_materialized() {
    DIRECT_RUNTIME_FRAME_MATERIALIZATION_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn note_direct_runtime_frame_materialized() {}

#[cfg(test)]
fn reset_direct_runtime_frame_materialization_count() {
    DIRECT_RUNTIME_FRAME_MATERIALIZATION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn direct_runtime_frame_materialization_count() -> usize {
    DIRECT_RUNTIME_FRAME_MATERIALIZATION_COUNT.with(Cell::get)
}

struct DirectTaskRuntimeState {
    ownership_tracking_active: bool,
    owned_value_refs: BTreeMap<usize, usize>,
    runtime_error_capture: bool,
    cleanup_stack: Vec<DirectCleanupRegistration>,
    next_cleanup_id: i64,
    cleanup_draining: bool,
    primary_runtime_diagnostic: Option<Diagnostic>,
    call_depth: usize,
    call_frames: DirectCallFrameStorage,
    task_ancestry: DirectTaskAncestry,
    fallback_cancellation: CancellationContext,
    returned_view_projection: Option<String>,
}

impl Default for DirectTaskRuntimeState {
    fn default() -> Self {
        Self {
            ownership_tracking_active: false,
            owned_value_refs: BTreeMap::new(),
            runtime_error_capture: false,
            cleanup_stack: Vec::new(),
            next_cleanup_id: 1,
            cleanup_draining: false,
            primary_runtime_diagnostic: None,
            call_depth: 0,
            call_frames: DirectCallFrameStorage::Empty,
            task_ancestry: DirectTaskAncestry::default(),
            fallback_cancellation: CancellationContext::default(),
            returned_view_projection: None,
        }
    }
}

fn boxed_direct_task_runtime_state(
    ownership_tracking_active: bool,
    task_ancestry: DirectTaskAncestry,
) -> Box<DirectTaskRuntimeState> {
    let mut state = Box::new(DirectTaskRuntimeState::default());
    state.ownership_tracking_active = ownership_tracking_active;
    state.task_ancestry = task_ancestry;
    state
}

struct PreparedDirectTaskRuntimeState {
    state: Box<DirectTaskRuntimeState>,
}

// SAFETY: the prepared state is constructed with empty cleanup and ownership
// collections, so it contains no live raw-pointer registrations. The wrapper
// is consumed exactly once on the selected worker before user task code can
// populate either collection.
unsafe impl Send for PreparedDirectTaskRuntimeState {}

impl PreparedDirectTaskRuntimeState {
    fn new(task_ancestry: DirectTaskAncestry) -> Self {
        Self {
            state: boxed_direct_task_runtime_state(true, task_ancestry),
        }
    }

    fn into_state(self) -> Box<DirectTaskRuntimeState> {
        self.state
    }
}

thread_local! {
    // Each pinned worker multiplexes several stackful native tasks, so an
    // ordinary worker-local flag would leak across every yield. Keep all
    // resumable direct state behind the globally unique task identity instead.
    static DIRECT_TASK_RUNTIME_STATES: RefCell<BTreeMap<u64, Box<DirectTaskRuntimeState>>> =
        const { RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
thread_local! {
    static DIRECT_VALUE_CLONE_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
static DIRECT_TASK_CLAIM_FLAG_LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static DIRECT_TASK_CLAIM_FLAG_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
fn direct_task_claim_flag_test_guard() -> MutexGuard<'static, ()> {
    match DIRECT_TASK_CLAIM_FLAG_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn direct_task_runtime_key() -> u64 {
    current_lightweight_task_id().unwrap_or(0)
}

fn with_direct_task_runtime_state_for_key<T>(
    key: u64,
    work: impl FnOnce(&mut DirectTaskRuntimeState) -> T,
) -> T {
    DIRECT_TASK_RUNTIME_STATES.with(|states| {
        let mut states = states.borrow_mut();
        work(
            states
                .entry(key)
                .or_insert_with(|| {
                    boxed_direct_task_runtime_state(false, DirectTaskAncestry::default())
                })
                .as_mut(),
        )
    })
}

fn with_direct_task_runtime_state<T>(work: impl FnOnce(&mut DirectTaskRuntimeState) -> T) -> T {
    with_direct_task_runtime_state_for_key(direct_task_runtime_key(), work)
}

fn register_direct_owned_value(value: *mut OpaqueValue) {
    if value.is_null() {
        return;
    }
    let key = direct_task_runtime_key();
    let overflow = DIRECT_TASK_RUNTIME_STATES.with(|states| {
        let mut states = states.borrow_mut();
        let Some(state) = states.get_mut(&key) else {
            return false;
        };
        if !state.ownership_tracking_active {
            return false;
        }
        let count = state.owned_value_refs.entry(value as usize).or_default();
        match count.checked_add(1) {
            Some(next) => {
                *count = next;
                false
            }
            None => true,
        }
    });
    if overflow {
        runtime_error("direct task owned-value reference count overflow");
    }
}

fn unregister_direct_owned_value(value: *mut OpaqueValue) -> bool {
    if value.is_null() {
        return false;
    }
    let key = direct_task_runtime_key();
    let removed = DIRECT_TASK_RUNTIME_STATES.with(|states| {
        let mut states = states.borrow_mut();
        let Some(state) = states.get_mut(&key) else {
            return false;
        };
        let address = value as usize;
        let Some(count) = state.owned_value_refs.get_mut(&address) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            state.owned_value_refs.remove(&address);
        }
        true
    });
    removed
}

struct DirectTaskRuntimeScopeGuard {
    key: u64,
    previous: Option<Box<DirectTaskRuntimeState>>,
}

impl Drop for DirectTaskRuntimeScopeGuard {
    fn drop(&mut self) {
        let (current, restored_previous) = DIRECT_TASK_RUNTIME_STATES.with(|states| {
            let mut states = states.borrow_mut();
            let current = states.remove(&self.key);
            let previous = self.previous.take();
            let restored_previous = previous.is_some();
            if let Some(previous) = previous {
                states.insert(self.key, previous);
            }
            (current, restored_previous)
        });
        let outstanding_owned_values = current
            .as_ref()
            .map(|current| current.owned_value_refs.clone())
            .unwrap_or_default();
        let outstanding_owned_value_types = outstanding_owned_values
            .iter()
            .map(|(address, count)| {
                let type_name = unsafe {
                    with_value(*address as *mut OpaqueValue, |value| {
                        value_type_name(value).to_string()
                    })
                };
                (*address, *count, type_name)
            })
            .collect::<Vec<_>>();
        if let Some(current) = current {
            release_direct_task_runtime_state(current);
        }

        // Dropping owned cleanup registrations should not need direct runtime state,
        // but remove any fresh default entry created by a release boundary so a
        // completed task cannot leave state behind for a reused task id.
        if !restored_previous {
            let transient =
                DIRECT_TASK_RUNTIME_STATES.with(|states| states.borrow_mut().remove(&self.key));
            if let Some(transient) = transient {
                release_direct_task_runtime_state(transient);
            }
        }

        if !std::thread::panicking() {
            assert!(
                outstanding_owned_values.is_empty(),
                "normally completed direct task retained owned opaque values: {:?}",
                outstanding_owned_value_types
            );
        }
    }
}

fn with_direct_task_runtime_scope<T>(work: impl FnOnce() -> T) -> T {
    with_direct_task_runtime_scope_with_direct_ancestry(DirectTaskAncestry::default(), work)
}

#[cfg(test)]
fn with_direct_task_runtime_scope_with_ancestry<T>(
    task_ancestry: Vec<RuntimeTaskFrame>,
    work: impl FnOnce() -> T,
) -> T {
    with_direct_task_runtime_scope_with_direct_ancestry(
        DirectTaskAncestry::from_runtime(task_ancestry),
        work,
    )
}

fn with_direct_task_runtime_scope_with_direct_ancestry<T>(
    task_ancestry: DirectTaskAncestry,
    work: impl FnOnce() -> T,
) -> T {
    let state = boxed_direct_task_runtime_state(true, task_ancestry);
    with_direct_task_runtime_scope_with_state(state, work)
}

fn with_direct_task_runtime_scope_with_state<T>(
    state: Box<DirectTaskRuntimeState>,
    work: impl FnOnce() -> T,
) -> T {
    let _guard = install_direct_task_runtime_state(state);
    work()
}

#[inline(never)]
fn install_direct_task_runtime_state(
    state: Box<DirectTaskRuntimeState>,
) -> DirectTaskRuntimeScopeGuard {
    let key = direct_task_runtime_key();
    let previous = DIRECT_TASK_RUNTIME_STATES.with(|states| states.borrow_mut().insert(key, state));
    DirectTaskRuntimeScopeGuard { key, previous }
}

fn direct_runtime_call_frames() -> Vec<RuntimeCallFrame> {
    with_direct_task_runtime_state(|state| state.call_frames.materialize_innermost_first())
}

fn direct_runtime_task_ancestry() -> Vec<RuntimeTaskFrame> {
    with_direct_task_runtime_state(|state| state.task_ancestry.materialize())
}

fn direct_runtime_compact_task_ancestry() -> DirectTaskAncestry {
    with_direct_task_runtime_state(|state| state.task_ancestry.clone())
}

fn capture_direct_runtime_frames_once(diagnostic: &mut Diagnostic) {
    if diagnostic.capture_runtime_frames_once(Vec::new(), Vec::new()) {
        diagnostic.call_frames = direct_runtime_call_frames();
        diagnostic.task_ancestry = direct_runtime_task_ancestry();
    }
}

fn clear_direct_task_runtime_states() {
    let stale = DIRECT_TASK_RUNTIME_STATES.with(|states| std::mem::take(&mut *states.borrow_mut()));
    for state in stale.into_values() {
        release_direct_task_runtime_state(state);
    }
}

fn discard_current_direct_task_runtime_state() {
    let key = direct_task_runtime_key();
    let state = DIRECT_TASK_RUNTIME_STATES.with(|states| states.borrow_mut().remove(&key));
    if let Some(state) = state {
        release_direct_task_runtime_state(state);
    }

    // Releasing owned cleanup arguments should not create runtime state, but
    // discard any defensive fallback entry before the task id can be observed
    // again by scheduler teardown.
    let transient = DIRECT_TASK_RUNTIME_STATES.with(|states| states.borrow_mut().remove(&key));
    if let Some(transient) = transient {
        release_direct_task_runtime_state(transient);
    }
}

fn release_direct_task_runtime_state(mut state: Box<DirectTaskRuntimeState>) {
    let owned_value_refs = std::mem::take(&mut state.owned_value_refs);
    drop(state);
    for (address, count) in owned_value_refs {
        for _ in 0..count {
            unsafe {
                release_untracked_value(address as *mut OpaqueValue);
            }
        }
    }
}

fn next_direct_cleanup_id(state: &mut DirectTaskRuntimeState) -> i64 {
    let id = state.next_cleanup_id;
    let mut next_id = id.checked_add(1).unwrap_or(1);
    if next_id == 0 {
        next_id = 1;
    }
    state.next_cleanup_id = next_id;
    id
}

fn push_direct_cleanup_registration(thunk_ptr: i64, args: *mut i64, arg_count: usize) -> i64 {
    with_direct_task_runtime_state(|state| {
        let id = next_direct_cleanup_id(state);
        state.cleanup_stack.push(DirectCleanupRegistration {
            id,
            thunk_ptr,
            args,
            arg_count,
            call_depth: state.call_depth,
        });
        id
    })
}

fn take_direct_cleanup_registration(id: i64) -> Option<DirectCleanupRegistration> {
    if id == 0 {
        return None;
    }
    with_direct_task_runtime_state(|state| {
        state
            .cleanup_stack
            .iter()
            .rposition(|registration| registration.id == id)
            .map(|index| state.cleanup_stack.remove(index))
    })
}

struct DirectCleanupDrainGuard {
    key: u64,
}

impl Drop for DirectCleanupDrainGuard {
    fn drop(&mut self) {
        with_direct_task_runtime_state_for_key(self.key, |state| {
            state.cleanup_draining = false;
        });
    }
}

struct DirectCallDepthGuard {
    key: u64,
    previous: usize,
}

impl Drop for DirectCallDepthGuard {
    fn drop(&mut self) {
        with_direct_task_runtime_state_for_key(self.key, |state| {
            state.call_depth = self.previous;
        });
    }
}

struct DirectPrimaryDiagnosticGuard {
    key: u64,
    installed: bool,
}

impl DirectPrimaryDiagnosticGuard {
    fn install(diagnostic: Diagnostic) -> Self {
        let key = direct_task_runtime_key();
        let installed = with_direct_task_runtime_state_for_key(key, |state| {
            if state.primary_runtime_diagnostic.is_some() {
                false
            } else {
                state.primary_runtime_diagnostic = Some(diagnostic);
                true
            }
        });
        Self { key, installed }
    }
}

impl Drop for DirectPrimaryDiagnosticGuard {
    fn drop(&mut self) {
        if self.installed {
            with_direct_task_runtime_state_for_key(self.key, |state| {
                state.primary_runtime_diagnostic = None;
            });
        }
    }
}

fn direct_primary_runtime_diagnostic() -> Option<Diagnostic> {
    with_direct_task_runtime_state(|state| state.primary_runtime_diagnostic.clone())
}

fn direct_cleanup_is_draining() -> bool {
    with_direct_task_runtime_state(|state| state.cleanup_draining)
}

fn direct_runtime_error_capture_enabled() -> bool {
    with_direct_task_runtime_state(|state| state.runtime_error_capture)
}

fn is_call_depth_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic.message.starts_with("maximum call depth")
}

fn write_stdout(text: &str) {
    let mut stdout = io::stdout().lock();
    let write_result = with_sigpipe_blocked(|| stdout.write_all(text.as_bytes()));
    let flush_result = if write_result.is_ok() {
        with_sigpipe_blocked(|| stdout.flush())
    } else {
        Ok(())
    };
    if let Some(error) = write_result.err().or_else(|| flush_result.err()) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            // `with_sigpipe_blocked` only leaves SIGPIPE ignored on this path because this
            // caller exits the process immediately after observing BrokenPipe.
            process::exit(0);
        }
        let _ = writeln!(io::stderr().lock(), "failed to write to stdout: {}", error);
        process::exit(1);
    }
}

fn write_stdout_result(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    with_sigpipe_blocked(|| stdout.write_all(text.as_bytes()))
}

fn flush_stdout_result() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    with_sigpipe_blocked(|| stdout.flush())
}

#[cfg(unix)]
fn with_sigpipe_blocked<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    unsafe {
        let previous_handler = libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        let mut sigpipe_set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut sigpipe_set);
        libc::sigaddset(&mut sigpipe_set, libc::SIGPIPE);

        let mut old_mask: libc::sigset_t = std::mem::zeroed();
        if libc::pthread_sigmask(libc::SIG_BLOCK, &sigpipe_set, &mut old_mask) != 0 {
            let result = f();
            if previous_handler != libc::SIG_ERR {
                let _ = libc::signal(libc::SIGPIPE, previous_handler);
            }
            return result;
        }

        let restore_sigpipe_state = || {
            let _ = libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
            if previous_handler != libc::SIG_ERR {
                let _ = libc::signal(libc::SIGPIPE, previous_handler);
            }
        };

        let result = f();
        if matches!(&result, Err(error) if error.kind() == io::ErrorKind::BrokenPipe) {
            let mut pending: libc::sigset_t = std::mem::zeroed();
            if libc::sigpending(&mut pending) == 0
                && libc::sigismember(&pending, libc::SIGPIPE) == 1
            {
                let mut received = 0;
                let _ = libc::sigwait(&sigpipe_set, &mut received);
            }
            // Restore the thread's signal mask so the helper does not leak blocked SIGPIPE
            // state. We intentionally keep SIGPIPE ignored on this path because the caller
            // exits immediately after seeing BrokenPipe; restoring the previous disposition
            // before that exit can cause the pending SIGPIPE to terminate the process.
            let _ = libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
            return result;
        }

        restore_sigpipe_state();
        result
    }
}

#[cfg(not(unix))]
fn with_sigpipe_blocked<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    f()
}

fn render_bool(value: i64) -> &'static str {
    if value == 0 {
        "false"
    } else {
        "true"
    }
}

fn int32_overflow_message(value: i64) -> String {
    format!("integer value `{}` does not fit in `int32`", value)
}

pub struct OpaqueValue {
    ref_count: AtomicUsize,
    value: RwLock<Value>,
    runtime_type_name: RwLock<Option<String>>,
}

#[cfg(coverage)]
#[doc(hidden)]
pub static DIRECT_VALUE_LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

type NativeThunk = unsafe extern "C-unwind" fn(*const i64, usize) -> *mut OpaqueValue;
const DIRECT_MAX_CALL_DEPTH: usize = 256;
const DIRECT_RUNTIME_STACK_SIZE: usize = 64 * 1024 * 1024;

struct ProgramSourceContext {
    path: String,
    source: String,
}

static DIRECT_PROGRAM_SOURCE: OnceLock<ProgramSourceContext> = OnceLock::new();

#[cfg(unix)]
struct InternalDiagnosticChannels {
    data: Option<std::fs::File>,
    signal: Option<std::fs::File>,
}

#[cfg(unix)]
static INTERNAL_DIAGNOSTIC_CHANNELS: std::sync::Mutex<Option<InternalDiagnosticChannels>> =
    std::sync::Mutex::new(None);

#[cfg(unix)]
fn lock_internal_diagnostic_channels(
) -> std::sync::MutexGuard<'static, Option<InternalDiagnosticChannels>> {
    INTERNAL_DIAGNOSTIC_CHANNELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
fn inherited_internal_diagnostic_file(fd: RawFd) -> Option<std::fs::File> {
    if fd < 0 {
        return None;
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return None;
    }
    // SAFETY: the private ABI transfers ownership of each inherited
    // descriptor to the runtime during initialization. `F_GETFD` above also
    // rejects stale or otherwise invalid numbers before ownership is assumed.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return None;
    }
    Some(file)
}

#[cfg(unix)]
fn install_internal_diagnostic_channels(data_fd: RawFd, signal_fd: RawFd) {
    let channels = if data_fd == signal_fd {
        // Never construct two `File` owners for one descriptor. Treat a
        // malformed pair as unavailable while still taking and closing it.
        let _ = inherited_internal_diagnostic_file(data_fd);
        InternalDiagnosticChannels {
            data: None,
            signal: None,
        }
    } else {
        InternalDiagnosticChannels {
            data: inherited_internal_diagnostic_file(data_fd),
            signal: inherited_internal_diagnostic_file(signal_fd),
        }
    };
    *lock_internal_diagnostic_channels() = Some(channels);
}

#[cfg(unix)]
fn initialize_internal_diagnostic_channels() {
    let data = std::env::var_os(crate::INTERNAL_DIAGNOSTIC_FD_ENV);
    let signal = std::env::var_os(crate::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV);
    // SAFETY: generated direct programs initialize the runtime before starting
    // any Aura tasks or user code, so no concurrent environment access has
    // begun. Hiding both private keys also prevents `sys.env` and spawned
    // descendants from observing the control channel.
    unsafe {
        std::env::remove_var(crate::INTERNAL_DIAGNOSTIC_FD_ENV);
        std::env::remove_var(crate::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV);
    }
    if data.is_none() && signal.is_none() {
        return;
    }
    let parse_fd = |value: Option<std::ffi::OsString>| {
        value
            .and_then(|value| value.into_string().ok())
            .and_then(|value| value.parse::<RawFd>().ok())
            .unwrap_or(-1)
    };
    install_internal_diagnostic_channels(parse_fd(data), parse_fd(signal));
}

#[cfg(not(unix))]
fn initialize_internal_diagnostic_channels() {
    // The direct structured-diagnostic transport is Unix-only. Keep the
    // initialization call unconditional so generated entry behavior is
    // platform-independent.
}

fn current_cancellation() -> CancellationContext {
    if let Some(cancellation) = current_lightweight_task_cancellation() {
        return cancellation;
    }
    with_direct_task_runtime_state(|state| state.fallback_cancellation.clone())
}

fn with_cancellation_scope<T>(cancellation: CancellationContext, work: impl FnOnce() -> T) -> T {
    struct CancellationGuard {
        key: u64,
        previous: CancellationContext,
    }

    impl Drop for CancellationGuard {
        fn drop(&mut self) {
            with_direct_task_runtime_state_for_key(self.key, |state| {
                state.fallback_cancellation = self.previous.clone();
            });
        }
    }

    let key = direct_task_runtime_key();
    let previous = with_direct_task_runtime_state_for_key(key, |state| {
        std::mem::replace(&mut state.fallback_cancellation, cancellation)
    });
    let _guard = CancellationGuard { key, previous };
    work()
}

fn extract_duration_nanoseconds(value: impl Borrow<Value>) -> i128 {
    match value.borrow() {
        Value::Int(value) => match value.as_i128() {
            Some(value) => value,
            None => {
                runtime_error("expected `Duration`, found an integer outside signed timer range")
            }
        },
        Value::Duration(value) => *value,
        other => runtime_error(format!(
            "expected `Duration`, found `{}`",
            value_type_name(other)
        )),
    }
}

fn duration_value_to_host_timer(value: &Value, label: &str) -> io::Result<StdDuration> {
    let nanoseconds = extract_duration_nanoseconds(value);
    crate::runtime_value::duration_to_host_timer(nanoseconds, label)
}

fn direct_timer_result_or_trap<T>(result: io::Result<T>) -> T {
    result.unwrap_or_else(|error| direct_timer_error(error))
}

fn direct_timer_diagnostic(error: io::Error) -> Diagnostic {
    Diagnostic::coded("AU4001", error.to_string())
}

fn direct_timer_error(error: io::Error) -> ! {
    runtime_diagnostic_error(direct_timer_diagnostic(error))
}

fn boxed_value(value: Value) -> *mut OpaqueValue {
    boxed_value_with_type(value, None)
}

fn boxed_typed_value(value: Value, runtime_type_name: &str) -> *mut OpaqueValue {
    boxed_value_with_type(value, Some(runtime_type_name.to_string()))
}

fn boxed_value_with_type(value: Value, runtime_type_name: Option<String>) -> *mut OpaqueValue {
    let value = Box::into_raw(Box::new(OpaqueValue {
        ref_count: AtomicUsize::new(1),
        value: RwLock::new(value),
        runtime_type_name: RwLock::new(runtime_type_name),
    }));
    #[cfg(coverage)]
    DIRECT_VALUE_LIVE_COUNT.fetch_add(1, Ordering::Relaxed);
    register_direct_owned_value(value);
    value
}

// These helpers validate the explicit refcount stored in `OpaqueValue`, but they cannot detect
// stale or forged raw pointers after an object has been freed and the address reused. The
// codegen/runtime ABI must still guarantee that callers only retain or release live values.
fn retain_ref_count(ref_count: &AtomicUsize) -> std::result::Result<(), &'static str> {
    loop {
        let current = ref_count.load(Ordering::Relaxed);
        if current == 0 {
            return Err("attempted to retain an already-released direct runtime value");
        }
        if current == usize::MAX {
            return Err("direct runtime value reference count overflow");
        }
        if ref_count
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(());
        }
    }
}

fn release_ref_count(ref_count: &AtomicUsize) -> std::result::Result<bool, &'static str> {
    loop {
        let current = ref_count.load(Ordering::Acquire);
        if current == 0 {
            return Err("attempted to release an already-released direct runtime value");
        }
        let next = current - 1;
        if ref_count
            .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(next == 0);
        }
    }
}

unsafe fn retain_untracked_value(value: *mut OpaqueValue) {
    if value.is_null() {
        return;
    }
    let opaque = unsafe {
        value
            .as_ref()
            .unwrap_or_else(|| runtime_error("direct runtime received an invalid opaque value"))
    };
    if let Err(message) = retain_ref_count(&opaque.ref_count) {
        runtime_error(message);
    }
}

unsafe fn release_untracked_value(value: *mut OpaqueValue) {
    if value.is_null() {
        return;
    }
    let opaque = unsafe {
        value
            .as_ref()
            .unwrap_or_else(|| runtime_error("direct runtime received an invalid opaque value"))
    };
    if release_ref_count(&opaque.ref_count).unwrap_or_else(|message| runtime_error(message)) {
        #[cfg(coverage)]
        {
            let previous = DIRECT_VALUE_LIVE_COUNT.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "direct runtime live-value counter underflow");
        }
        unsafe {
            drop(Box::from_raw(value));
        }
    }
}

unsafe fn with_value<T>(ptr: *mut OpaqueValue, read: impl FnOnce(&Value) -> T) -> T {
    let value = match ptr.as_ref() {
        Some(value) => value,
        None => runtime_error("direct runtime received a null opaque value pointer"),
    };
    let guard = match value.value.read() {
        Ok(guard) => guard,
        Err(_) => runtime_error("direct runtime value lock was poisoned"),
    };
    read(&guard)
}

unsafe fn value_ref(ptr: *mut OpaqueValue) -> Value {
    #[cfg(test)]
    DIRECT_VALUE_CLONE_COUNT.with(|count| count.set(count.get() + 1));
    with_value(ptr, Clone::clone)
}

#[cfg(test)]
fn direct_value_clone_count() -> usize {
    DIRECT_VALUE_CLONE_COUNT.with(Cell::get)
}

unsafe fn explicit_runtime_type_name(ptr: *mut OpaqueValue) -> Option<String> {
    match ptr.as_ref() {
        Some(value) => match value.runtime_type_name.read() {
            Ok(runtime_type_name) => runtime_type_name.clone(),
            Err(_) => runtime_error("direct runtime type lock was poisoned"),
        },
        None => runtime_error("direct runtime received a null opaque value pointer"),
    }
}

const CANONICAL_RUNTIME_TYPE_PREFIX: &str = "__aura_type_json_v1__:";

pub(crate) fn canonical_runtime_type_name(ty: &Type) -> String {
    format!(
        "{CANONICAL_RUNTIME_TYPE_PREFIX}{}",
        serde_json::to_string(ty).expect("Aura semantic types must serialize")
    )
}

fn canonical_runtime_type_from_name(name: &str) -> Option<Type> {
    name.strip_prefix(CANONICAL_RUNTIME_TYPE_PREFIX)
        .and_then(|json| serde_json::from_str(json).ok())
}

fn embedded_runtime_type_name(value: &Value) -> Option<String> {
    match value {
        Value::Int(value) => value.runtime_type_name().map(str::to_string),
        Value::Tuple(tuple) => Some(canonical_runtime_type_name(&Type::Tuple(
            tuple.element_types.clone(),
        ))),
        Value::Vec(vector) => Some(canonical_runtime_type_name(&Type::Named(
            "list".to_string(),
            vec![vector.element_type.clone()],
        ))),
        Value::Array(array) => Some(canonical_runtime_type_name(&Type::Named(
            "Array".to_string(),
            vec![array.element_type()],
        ))),
        Value::Set(set) => Some(canonical_runtime_type_name(&Type::Named(
            "set".to_string(),
            vec![set.element_type.clone()],
        ))),
        Value::Map(map) => Some(canonical_runtime_type_name(&Type::Named(
            "dict".to_string(),
            vec![map.key_type.clone(), map.value_type.clone()],
        ))),
        Value::Instance(instance) => {
            instance
                .fields
                .get(DIRECT_RUNTIME_TYPE_FIELD)
                .and_then(|value| match value {
                    Value::String(runtime_type_name) => Some(runtime_type_name.clone()),
                    _ => None,
                })
        }
        Value::EnumVariant(variant) => {
            embedded_nominal_runtime_type_name(&variant.enum_name).map(str::to_string)
        }
        Value::Channel(channel) => channel.runtime_type_name(),
        Value::Task(task) => task.runtime_type_name(),
        Value::Function(function) => Some(canonical_runtime_type_name(&function.signature)),
        _ => None,
    }
}

unsafe fn effective_runtime_type_name(ptr: *mut OpaqueValue) -> Option<String> {
    explicit_runtime_type_name(ptr).or_else(|| with_value(ptr, embedded_runtime_type_name))
}

fn runtime_type_from_name(name: &str) -> Type {
    fn split_type_list(source: &str) -> Vec<Type> {
        let mut values = Vec::new();
        let mut bracket_depth = 0usize;
        let mut tuple_depth = 0usize;
        let mut start = 0usize;
        for (index, character) in source.char_indices() {
            match character {
                '[' => bracket_depth += 1,
                ']' => bracket_depth = bracket_depth.saturating_sub(1),
                '(' => tuple_depth += 1,
                ')' => tuple_depth = tuple_depth.saturating_sub(1),
                ',' if bracket_depth == 0 && tuple_depth == 0 => {
                    let value = source[start..index].trim();
                    if !value.is_empty() {
                        values.push(runtime_type_from_name(value));
                    }
                    start = index + 1;
                }
                _ => {}
            }
        }
        let value = source[start..].trim();
        if !value.is_empty() {
            values.push(runtime_type_from_name(value));
        }
        values
    }

    if let Some(ty) = canonical_runtime_type_from_name(name) {
        return ty;
    }
    if name == "None" {
        return Type::Unit;
    }
    if let Some(module) = name.strip_prefix("module ") {
        return Type::Module(module.to_string());
    }
    if name.starts_with('(') && name.ends_with(')') {
        return Type::Tuple(split_type_list(&name[1..name.len() - 1]));
    }
    let Some(open) = name.find('[') else {
        return Type::named(name);
    };
    if !name.ends_with(']') {
        return Type::named(name);
    }
    let base = &name[..open];
    let inner = &name[open + 1..name.len() - 1];
    Type::Named(base.to_string(), split_type_list(inner))
}

fn runtime_type_pattern_from_name(name: &str) -> Type {
    fn decode_pattern(ty: Type) -> Type {
        match ty {
            Type::Named(name, args) if args.is_empty() && name.starts_with('?') => {
                Type::TypeParam(name[1..].to_string())
            }
            Type::Named(name, args) => Type::Named(
                name,
                args.into_iter().map(decode_pattern).collect::<Vec<_>>(),
            ),
            Type::Tuple(elements) => {
                Type::Tuple(elements.into_iter().map(decode_pattern).collect())
            }
            Type::Function {
                params,
                return_type,
            } => Type::Function {
                params: params
                    .into_iter()
                    .map(|mut param| {
                        param.ty = decode_pattern(param.ty);
                        param
                    })
                    .collect(),
                return_type: Box::new(decode_pattern(*return_type)),
            },
            Type::Closure {
                params,
                return_type,
                mut captures,
                call_kind,
            } => {
                for capture in captures.iter_mut() {
                    capture.ty = decode_pattern(capture.ty.clone());
                }
                Type::Closure {
                    params: Box::new(
                        params
                            .into_iter()
                            .map(|mut param| {
                                param.ty = decode_pattern(param.ty);
                                param
                            })
                            .collect(),
                    ),
                    return_type: Box::new(decode_pattern(*return_type)),
                    captures,
                    call_kind,
                }
            }
            Type::Unit | Type::Module(_) | Type::TypeParam(_) => ty,
        }
    }

    decode_pattern(runtime_type_from_name(name))
}

fn runtime_type_pattern_matches(
    pattern: &Type,
    actual: &Type,
    substitutions: &mut BTreeMap<String, Type>,
) -> bool {
    match pattern {
        Type::TypeParam(name) => match substitutions.get(name) {
            Some(existing) => existing == actual,
            None => {
                substitutions.insert(name.clone(), actual.clone());
                true
            }
        },
        Type::Named(name, pattern_args) => {
            let Type::Named(actual_name, actual_args) = actual else {
                return false;
            };
            name == actual_name
                && pattern_args.len() == actual_args.len()
                && pattern_args
                    .iter()
                    .zip(actual_args)
                    .all(|(pattern_arg, actual_arg)| {
                        runtime_type_pattern_matches(pattern_arg, actual_arg, substitutions)
                    })
        }
        Type::Tuple(pattern_elements) => {
            let Type::Tuple(actual_elements) = actual else {
                return false;
            };
            pattern_elements.len() == actual_elements.len()
                && pattern_elements.iter().zip(actual_elements).all(
                    |(pattern_element, actual_element)| {
                        runtime_type_pattern_matches(pattern_element, actual_element, substitutions)
                    },
                )
        }
        Type::Function {
            params: pattern_params,
            return_type: pattern_return,
        } => {
            let Type::Function {
                params: actual_params,
                return_type: actual_return,
            } = actual
            else {
                return false;
            };
            pattern_params.len() == actual_params.len()
                && pattern_params.iter().zip(actual_params.iter()).all(
                    |(pattern_param, actual_param)| {
                        pattern_param.passing == actual_param.passing
                            && runtime_type_pattern_matches(
                                &pattern_param.ty,
                                &actual_param.ty,
                                substitutions,
                            )
                    },
                )
                && runtime_type_pattern_matches(pattern_return, actual_return, substitutions)
        }
        Type::Closure {
            params: pattern_params,
            return_type: pattern_return,
            captures: pattern_captures,
            call_kind: pattern_call_kind,
        } => {
            let Type::Closure {
                params: actual_params,
                return_type: actual_return,
                captures: actual_captures,
                call_kind: actual_call_kind,
            } = actual
            else {
                return false;
            };
            pattern_call_kind == actual_call_kind
                && pattern_params.len() == actual_params.len()
                && pattern_captures.len() == actual_captures.len()
                && pattern_params.iter().zip(actual_params.iter()).all(
                    |(pattern_param, actual_param)| {
                        pattern_param.passing == actual_param.passing
                            && runtime_type_pattern_matches(
                                &pattern_param.ty,
                                &actual_param.ty,
                                substitutions,
                            )
                    },
                )
                && pattern_captures.iter().zip(actual_captures.iter()).all(
                    |(pattern_capture, actual_capture)| {
                        pattern_capture.mode == actual_capture.mode
                            && runtime_type_pattern_matches(
                                &pattern_capture.ty,
                                &actual_capture.ty,
                                substitutions,
                            )
                    },
                )
                && runtime_type_pattern_matches(pattern_return, actual_return, substitutions)
        }
        Type::Module(path) => matches!(actual, Type::Module(actual_path) if path == actual_path),
        Type::Unit => *actual == Type::Unit,
    }
}

unsafe fn set_explicit_runtime_type_name(ptr: *mut OpaqueValue, runtime_type_name: String) {
    match ptr.as_ref() {
        Some(value) => match value.runtime_type_name.write() {
            Ok(mut stored) => *stored = Some(runtime_type_name.clone()),
            Err(_) => runtime_error("direct runtime type lock was poisoned"),
        },
        None => runtime_error("direct runtime received a null opaque value pointer"),
    }
    let parsed = runtime_type_from_name(&runtime_type_name);
    unsafe {
        value_mut(ptr, |value| match value {
            Value::Int(integer) => {
                if let Some(kind) = IntegerKind::from_runtime_type_name(&runtime_type_name) {
                    if let Some(typed) = integer.with_runtime_kind(kind) {
                        *integer = typed;
                    }
                }
            }
            Value::Vec(vector) => {
                if let Type::Named(name, args) = &parsed {
                    if name == "list" && args.len() == 1 {
                        vector.element_type = args[0].clone();
                    }
                }
            }
            Value::Set(set) => {
                if let Type::Named(name, args) = &parsed {
                    if name == "set" && args.len() == 1 {
                        set.element_type = args[0].clone();
                    }
                }
            }
            Value::Map(map) => {
                if let Type::Named(name, args) = &parsed {
                    if name == "dict" && args.len() == 2 {
                        map.key_type = args[0].clone();
                        map.value_type = args[1].clone();
                    }
                }
            }
            Value::Tuple(tuple) => {
                if let Type::Tuple(element_types) = &parsed {
                    if element_types.len() == tuple.elements.len() {
                        tuple.element_types = element_types.clone();
                    }
                }
            }
            Value::Instance(instance) if matches!(&parsed, Type::Named(_, args) if !args.is_empty()) =>
            {
                instance.fields.insert(
                    DIRECT_RUNTIME_TYPE_FIELD.to_string(),
                    Value::String(runtime_type_name.clone()),
                );
            }
            Value::EnumVariant(variant) if matches!(&parsed, Type::Named(_, args) if !args.is_empty()) =>
            {
                let base = nominal_runtime_base_name(&variant.enum_name);
                variant.enum_name =
                    format!("{base}{DIRECT_RUNTIME_TYPE_SEPARATOR}{runtime_type_name}");
            }
            Value::Channel(channel) => channel.set_runtime_type_name(runtime_type_name.clone()),
            Value::Task(task) => task.set_runtime_type_name(runtime_type_name.clone()),
            Value::Function(function) => {
                if matches!(parsed, Type::Function { .. } | Type::Closure { .. }) {
                    function.signature = parsed.clone();
                }
            }
            _ => {}
        });
    }
}

unsafe fn value_mut<T>(ptr: *mut OpaqueValue, write: impl FnOnce(&mut Value) -> T) -> T {
    let value = match ptr.as_ref() {
        Some(value) => value,
        None => runtime_error("direct runtime received a null opaque value pointer"),
    };
    let mut guard = match value.value.write() {
        Ok(guard) => guard,
        Err(_) => runtime_error("direct runtime value lock was poisoned"),
    };
    write(&mut guard)
}

unsafe fn take_value(ptr: *mut OpaqueValue) -> Value {
    value_ref(ptr)
}

unsafe fn consume_value(ptr: *mut OpaqueValue) -> Value {
    let value = value_ref(ptr);
    unsafe {
        aura_direct_release_value(ptr);
    }
    value
}

unsafe fn consume_owned_value(ptr: *mut OpaqueValue) -> Value {
    let value = unsafe { value_mut(ptr, |value| std::mem::replace(value, Value::Unit)) };
    unsafe {
        aura_direct_release_value(ptr);
    }
    value
}

unsafe fn consume_untracked_value(ptr: *mut OpaqueValue) -> Value {
    let value = unsafe { value_ref(ptr) };
    unsafe {
        release_untracked_value(ptr);
    }
    value
}

unsafe fn consume_owned_untracked_value(ptr: *mut OpaqueValue) -> Value {
    let value = unsafe { value_mut(ptr, |value| std::mem::replace(value, Value::Unit)) };
    unsafe {
        release_untracked_value(ptr);
    }
    value
}

#[cfg(coverage)]
#[doc(hidden)]
pub unsafe fn aura_direct_coverage_clone_value(ptr: *mut OpaqueValue) -> Value {
    unsafe { value_ref(ptr) }
}

unsafe fn consume_opaque_buffer(buffer: *mut i64, count: usize) -> Vec<Value> {
    let handles = unsafe { Vec::from_raw_parts(buffer, count, count) };
    handles
        .into_iter()
        .map(|handle| {
            if handle == 0 {
                runtime_error("direct runtime received a null enum payload handle");
            }
            unsafe { consume_untracked_value(handle as *mut OpaqueValue) }
        })
        .collect()
}

unsafe fn consume_owned_opaque_buffer(buffer: *mut i64, count: usize) -> Vec<Value> {
    unsafe { consume_owned_opaque_buffer_for(buffer, count, "enum payload") }
}

unsafe fn consume_owned_opaque_buffer_for(
    buffer: *mut i64,
    count: usize,
    element_description: &str,
) -> Vec<Value> {
    let handles = unsafe { Vec::from_raw_parts(buffer, count, count) };
    handles
        .into_iter()
        .map(|handle| {
            if handle == 0 {
                runtime_error(format!(
                    "direct runtime received a null owned {element_description} handle"
                ));
            }
            unsafe { consume_owned_untracked_value(handle as *mut OpaqueValue) }
        })
        .collect()
}

struct DirectHostArgBuffer {
    handles: Vec<i64>,
}

impl DirectHostArgBuffer {
    unsafe fn from_raw(buffer: *mut i64, count: usize) -> Self {
        Self {
            handles: unsafe { Vec::from_raw_parts(buffer, count, count) },
        }
    }

    fn validate(&self, name: &str) -> std::result::Result<(), Diagnostic> {
        let Some(metadata) = host_builtin_metadata(name) else {
            return Err(Diagnostic::coded(
                "AU4001",
                format!("unknown dynamic host builtin `{name}`"),
            ));
        };
        if self.handles.len() != metadata.params.len() {
            return Err(Diagnostic::coded(
                "AU4001",
                format!(
                    "`{name}` expects {} arguments, found {}",
                    metadata.params.len(),
                    self.handles.len()
                ),
            ));
        }
        Ok(())
    }

    fn handle(
        &self,
        name: &str,
        index: usize,
        expected_passing: ReceiverKind,
    ) -> std::result::Result<*mut OpaqueValue, Diagnostic> {
        let metadata = host_builtin_metadata(name).ok_or_else(|| {
            Diagnostic::coded("AU4001", format!("unknown dynamic host builtin `{name}`"))
        })?;
        let parameter = metadata.params.get(index).ok_or_else(|| {
            Diagnostic::coded("AU4001", format!("`{name}` has no argument {}", index + 1))
        })?;
        if parameter.passing != expected_passing {
            return Err(Diagnostic::coded(
                "AU4001",
                format!(
                    "dynamic host ABI expected `{name}` argument `{}` to use {:?} passing, found {:?}",
                    parameter.name, expected_passing, parameter.passing
                ),
            ));
        }
        let handle = *self.handles.get(index).ok_or_else(|| {
            Diagnostic::coded(
                "AU4001",
                format!("`{name}` is missing argument {}", index + 1),
            )
        })?;
        if handle == 0 {
            return Err(Diagnostic::coded(
                "AU4001",
                format!("`{name}` received a null argument {}", index + 1),
            ));
        }
        Ok(handle as *mut OpaqueValue)
    }

    fn with_borrow<T>(
        &self,
        name: &str,
        index: usize,
        read: impl FnOnce(&Value) -> std::result::Result<T, Diagnostic>,
    ) -> std::result::Result<T, Diagnostic> {
        let handle = self.handle(name, index, ReceiverKind::Borrow)?;
        unsafe { with_value(handle, read) }
    }

    /// Reads a copy-typed argument without taking ownership.
    ///
    /// ADR-0022 Q1 makes a bare parameter shared for every type, including
    /// declaration-known copy types, so the declared passing this asserts is
    /// `Borrow`. The ABI still hands over copied bits; what changed is the
    /// source-level capability, not how the adapter reads the value.
    fn with_copy<T>(
        &self,
        name: &str,
        index: usize,
        read: impl FnOnce(&Value) -> std::result::Result<T, Diagnostic>,
    ) -> std::result::Result<T, Diagnostic> {
        let handle = self.handle(name, index, ReceiverKind::Borrow)?;
        unsafe { with_value(handle, read) }
    }

    fn take_value(&self, name: &str, index: usize) -> std::result::Result<Value, Diagnostic> {
        let handle = self.handle(name, index, ReceiverKind::Value)?;
        Ok(unsafe { value_mut(handle, |value| mem::replace(value, Value::Unit)) })
    }
}

impl Drop for DirectHostArgBuffer {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            if handle != 0 {
                unsafe {
                    release_untracked_value(handle as *mut OpaqueValue);
                }
            }
        }
    }
}

fn is_dynamic_json_host_builtin(name: &str) -> bool {
    matches!(
        name,
        "json::parse"
            | "json::dumps"
            | "json::is_null"
            | "json::as_bool"
            | "json::as_int"
            | "json::as_float"
            | "json::into_string"
            | "json::into_array"
            | "json::into_object"
    )
}

fn is_dynamic_bytes_host_builtin(name: &str) -> bool {
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

fn evaluate_direct_bytes_host_builtin(
    name: &str,
    args: &DirectHostArgBuffer,
) -> std::result::Result<Value, Diagnostic> {
    debug_assert!(is_dynamic_bytes_host_builtin(name));
    args.validate(name)?;
    args.with_borrow(name, 0, |value| {
        evaluate_bytes_host_builtin_ref(name, value)
            .expect("classified byte host builtins are registered with the shared adapter")
    })
}

fn direct_json_variant<'a>(
    value: &'a Value,
    call: &str,
) -> std::result::Result<&'a EnumVariantValue, Diagnostic> {
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

fn direct_json_exact_payload<'a>(
    value: &'a Value,
    expected_variant: &str,
    call: &str,
) -> std::result::Result<Option<&'a Value>, Diagnostic> {
    let variant = direct_json_variant(value, call)?;
    if variant.variant_name != expected_variant {
        return Ok(None);
    }
    match variant.payloads.as_slice() {
        [payload] => Ok(Some(payload)),
        _ => Err(Diagnostic::coded(
            "AU4001",
            format!("malformed runtime `json.Value.{expected_variant}` payload in `{call}`"),
        )),
    }
}

fn direct_json_into_exact_payload(
    value: Value,
    expected_variant: &str,
    call: &str,
) -> std::result::Result<Option<Value>, Diagnostic> {
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

fn direct_json_indent(value: &Value) -> std::result::Result<Option<i64>, Diagnostic> {
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

fn evaluate_direct_json_host_builtin(
    name: &str,
    args: &DirectHostArgBuffer,
) -> std::result::Result<Value, Diagnostic> {
    args.validate(name)?;
    match name {
        "json::parse" => {
            let (source, reservation) = prepare_json_codec_source(|| {
                args.with_borrow(name, 0, |value| {
                    let Value::String(text) = value else {
                        return Err(Diagnostic::coded(
                            "AU4001",
                            format!("`{name}` expects argument 1 to be `str`"),
                        ));
                    };
                    clone_json_codec_source(text)
                })
            })?;
            json_parse_owned_to_runtime(source, reservation)
        }
        "json::dumps" => {
            let indent = args.with_copy(name, 1, direct_json_indent)?;
            args.with_borrow(name, 0, |value| {
                let value = runtime_value_to_json(value)?;
                json_codec::dumps(&value, indent)
                    .map(Value::String)
                    .map_err(json_dump_error_to_diagnostic)
            })
        }
        "json::is_null" => args.with_borrow(name, 0, |value| {
            let variant = direct_json_variant(value, name)?;
            if variant.variant_name == "Null" && !variant.payloads.is_empty() {
                return Err(Diagnostic::coded(
                    "AU4001",
                    "malformed runtime `json.Value.Null` payload in `json::is_null`",
                ));
            }
            Ok(Value::Bool(variant.variant_name == "Null"))
        }),
        "json::as_bool" => args.with_borrow(name, 0, |value| {
            Ok(match direct_json_exact_payload(value, "Bool", name)? {
                Some(Value::Bool(value)) => option_some(Value::Bool(*value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
                    ))
                }
                None => option_none(),
            })
        }),
        "json::as_int" => args.with_borrow(name, 0, |value| {
            Ok(match direct_json_exact_payload(value, "Int", name)? {
                Some(Value::Int(value)) if json_int_metadata_is_exact(value) => {
                    option_some(Value::Int(*value))
                }
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Int` payload in `json::as_int`",
                    ))
                }
                None => option_none(),
            })
        }),
        "json::as_float" => args.with_borrow(name, 0, |value| {
            Ok(match direct_json_exact_payload(value, "Float", name)? {
                Some(Value::Float(value)) => option_some(Value::Float(*value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.Float` payload in `json::as_float`",
                    ))
                }
                None => option_none(),
            })
        }),
        "json::into_string" => Ok(
            match direct_json_into_exact_payload(args.take_value(name, 0)?, "String", name)? {
                Some(Value::String(value)) => option_some(Value::String(value)),
                Some(_) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        "malformed runtime `json.Value.String` payload in `json::into_string`",
                    ))
                }
                None => option_none(),
            },
        ),
        "json::into_array" => Ok(
            match direct_json_into_exact_payload(args.take_value(name, 0)?, "Array", name)? {
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
            },
        ),
        "json::into_object" => Ok(
            match direct_json_into_exact_payload(args.take_value(name, 0)?, "Object", name)? {
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
            },
        ),
        _ => Err(Diagnostic::coded(
            "AU4001",
            format!("unknown dynamic JSON host builtin `{name}`"),
        )),
    }
}

fn decode_bytes(ptr: *const u8, len: usize) -> String {
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    str::from_utf8(bytes)
        .unwrap_or_else(|_| runtime_error("aura direct runtime received invalid UTF-8 bytes"))
        .to_string()
}

fn direct_ffi_type_code(ffi_type: FfiType) -> u8 {
    match ffi_type {
        FfiType::Unit => 0,
        FfiType::Bool => 1,
        FfiType::I8 => 2,
        FfiType::I16 => 3,
        FfiType::I32 => 4,
        FfiType::I64 => 5,
        FfiType::U8 => 6,
        FfiType::U16 => 7,
        FfiType::U32 => 8,
        FfiType::U64 => 9,
        FfiType::F32 => 10,
        FfiType::F64 => 11,
        FfiType::StringView => 12,
        FfiType::BytesView => 13,
        FfiType::BytesViewMut => 14,
        FfiType::OpaqueHandle => 15,
    }
}

fn direct_ffi_type_from_code(code: u8) -> Option<FfiType> {
    Some(match code {
        0 => FfiType::Unit,
        1 => FfiType::Bool,
        2 => FfiType::I8,
        3 => FfiType::I16,
        4 => FfiType::I32,
        5 => FfiType::I64,
        6 => FfiType::U8,
        7 => FfiType::U16,
        8 => FfiType::U32,
        9 => FfiType::U64,
        10 => FfiType::F32,
        11 => FfiType::F64,
        12 => FfiType::StringView,
        13 => FfiType::BytesView,
        14 => FfiType::BytesViewMut,
        15 => FfiType::OpaqueHandle,
        _ => return None,
    })
}

fn direct_ffi_passing_code(passing: ReceiverKind) -> u8 {
    match passing {
        ReceiverKind::Borrow => 0,
        ReceiverKind::BorrowMut => 1,
        ReceiverKind::Value => 2,
    }
}

fn direct_ffi_passing_from_code(code: u8) -> Option<ReceiverKind> {
    Some(match code {
        0 => ReceiverKind::Borrow,
        1 => ReceiverKind::BorrowMut,
        2 => ReceiverKind::Value,
        _ => return None,
    })
}

fn append_direct_ffi_text(encoded: &mut Vec<u8>, text: &str) {
    let len = u32::try_from(text.len()).expect("validated direct FFI metadata fits in u32");
    encoded.extend_from_slice(&len.to_le_bytes());
    encoded.extend_from_slice(text.as_bytes());
}

fn append_direct_ffi_type(encoded: &mut Vec<u8>, ty: &DirectFfiType) {
    encoded.push(direct_ffi_type_code(ty.ffi_type));
    append_direct_ffi_text(encoded, ty.opaque_name.as_deref().unwrap_or(""));
}

pub(crate) fn encode_direct_ffi_call_spec(spec: &DirectFfiCallSpec) -> Vec<u8> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(DIRECT_FFI_SPEC_MAGIC);
    encoded.push(DIRECT_FFI_SPEC_VERSION);
    append_direct_ffi_text(&mut encoded, &spec.symbol);
    let count = u32::try_from(spec.params.len()).expect("validated direct FFI arity fits in u32");
    encoded.extend_from_slice(&count.to_le_bytes());
    for param in &spec.params {
        encoded.push(direct_ffi_passing_code(param.passing));
        append_direct_ffi_type(&mut encoded, &param.ty);
    }
    append_direct_ffi_type(&mut encoded, &spec.result);
    encoded
}

struct DirectFfiSpecDecoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> DirectFfiSpecDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, len: usize) -> std::result::Result<&'a [u8], String> {
        let end = self
            .cursor
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "metadata ended unexpectedly".to_string())?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> std::result::Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> std::result::Result<u32, String> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .expect("decoder took exactly four bytes");
        Ok(u32::from_le_bytes(bytes))
    }

    fn text(&mut self) -> std::result::Result<String, String> {
        let len = usize::try_from(self.u32()?)
            .map_err(|_| "metadata text length does not fit this host".to_string())?;
        let bytes = self.take(len)?;
        str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| "metadata text is not valid UTF-8".to_string())
    }

    fn ffi_type(&mut self) -> std::result::Result<DirectFfiType, String> {
        let code = self.byte()?;
        let ffi_type = direct_ffi_type_from_code(code)
            .ok_or_else(|| format!("unknown FFI type code {code}"))?;
        let opaque_name = self.text()?;
        match (ffi_type, opaque_name.is_empty()) {
            (FfiType::OpaqueHandle, false) => Ok(DirectFfiType {
                ffi_type,
                opaque_name: Some(opaque_name),
            }),
            (FfiType::OpaqueHandle, true) => {
                Err("opaque-handle metadata is missing its nominal type".to_string())
            }
            (_, true) => Ok(DirectFfiType {
                ffi_type,
                opaque_name: None,
            }),
            (_, false) => Err(format!(
                "non-handle FFI type `{ffi_type}` carries an opaque nominal name"
            )),
        }
    }
}

fn decode_direct_ffi_call_spec(bytes: &[u8]) -> std::result::Result<DirectFfiCallSpec, String> {
    let mut decoder = DirectFfiSpecDecoder::new(bytes);
    if decoder.take(DIRECT_FFI_SPEC_MAGIC.len())? != DIRECT_FFI_SPEC_MAGIC {
        return Err("metadata magic is not `AUFI`".to_string());
    }
    let version = decoder.byte()?;
    if version != DIRECT_FFI_SPEC_VERSION {
        return Err(format!("unsupported metadata version {version}"));
    }
    let symbol = decoder.text()?;
    if symbol.is_empty() {
        return Err("symbol name is empty".to_string());
    }
    let count = usize::try_from(decoder.u32()?)
        .map_err(|_| "parameter count does not fit this host".to_string())?;
    let mut params = Vec::with_capacity(count);
    for _ in 0..count {
        let code = decoder.byte()?;
        let passing = direct_ffi_passing_from_code(code)
            .ok_or_else(|| format!("unknown ownership mode code {code}"))?;
        params.push(DirectFfiParam {
            passing,
            ty: decoder.ffi_type()?,
        });
    }
    let result = decoder.ffi_type()?;
    if decoder.cursor != bytes.len() {
        return Err("metadata has trailing bytes".to_string());
    }
    Ok(DirectFfiCallSpec {
        symbol,
        params,
        result,
    })
}

fn bytes_vec_value(bytes: Vec<u8>) -> Value {
    Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: bytes
            .into_iter()
            .map(|byte| {
                Value::Int(
                    IntegerValue::from_typed_unsigned(byte as u128, IntegerKind::Uint8)
                        .expect("every byte fits the uint8 runtime kind"),
                )
            })
            .collect(),
    })
}

fn direct_ffi_integer_as_i128(
    value: IntegerValue,
    expected: FfiType,
) -> std::result::Result<i128, String> {
    value
        .as_i128()
        .ok_or_else(|| format!("FFI argument expected {expected}, but the integer is too large"))
}

fn direct_ffi_integer_as_u128(
    value: IntegerValue,
    expected: FfiType,
) -> std::result::Result<u128, String> {
    match value.representation() {
        IntegerRepresentation::Unsigned(value) => Ok(value),
        IntegerRepresentation::Signed(value) => u128::try_from(value)
            .map_err(|_| format!("FFI argument expected {expected}, but received {value}")),
    }
}

fn direct_value_to_ffi(value: &Value, ty: &DirectFfiType) -> std::result::Result<FfiValue, String> {
    let mismatch = || {
        format!(
            "FFI argument expected {}, but received `{}`",
            ty.ffi_type,
            value_type_name(value)
        )
    };
    Ok(match (ty.ffi_type, value) {
        (FfiType::Bool, Value::Bool(value)) => FfiValue::Bool(*value),
        (FfiType::I8, Value::Int(value)) => FfiValue::I8(
            i8::try_from(direct_ffi_integer_as_i128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::I16, Value::Int(value)) => FfiValue::I16(
            i16::try_from(direct_ffi_integer_as_i128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::I32, Value::Int(value)) => FfiValue::I32(
            i32::try_from(direct_ffi_integer_as_i128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::I64, Value::Int(value)) => FfiValue::I64(
            i64::try_from(direct_ffi_integer_as_i128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::U8, Value::Int(value)) => FfiValue::U8(
            u8::try_from(direct_ffi_integer_as_u128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::U16, Value::Int(value)) => FfiValue::U16(
            u16::try_from(direct_ffi_integer_as_u128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::U32, Value::Int(value)) => FfiValue::U32(
            u32::try_from(direct_ffi_integer_as_u128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::U64, Value::Int(value)) => FfiValue::U64(
            u64::try_from(direct_ffi_integer_as_u128(*value, ty.ffi_type)?)
                .map_err(|_| mismatch())?,
        ),
        (FfiType::F32, Value::Float(value)) => FfiValue::F32(*value as f32),
        (FfiType::F64, Value::Float(value)) => FfiValue::F64(*value),
        (FfiType::StringView, Value::String(value)) => FfiValue::String(value.clone()),
        (FfiType::BytesView | FfiType::BytesViewMut, Value::Vec(_)) => {
            FfiValue::Bytes(expect_bytes_value(value, "FFI byte view"))
        }
        (FfiType::OpaqueHandle, Value::FfiHandle(handle)) => {
            let expected_name = ty
                .opaque_name
                .as_deref()
                .ok_or_else(|| "opaque FFI metadata is missing its nominal type".to_string())?;
            if handle.type_name() != expected_name {
                return Err(mismatch());
            }
            let handle = OpaqueHandle::new(handle.as_ptr())
                .ok_or_else(|| "FFI opaque handles cannot contain a null address".to_string())?;
            FfiValue::OpaqueHandle(handle)
        }
        _ => return Err(mismatch()),
    })
}

fn direct_ffi_to_value(value: FfiValue, ty: &DirectFfiType) -> std::result::Result<Value, String> {
    let mismatch = || {
        format!(
            "FFI engine returned a value incompatible with declared result `{}`",
            ty.ffi_type
        )
    };
    Ok(match (ty.ffi_type, value) {
        (FfiType::Unit, FfiValue::Unit) => Value::Unit,
        (FfiType::Bool, FfiValue::Bool(value)) => Value::Bool(value),
        (FfiType::I8, FfiValue::I8(value)) => Value::Int(
            IntegerValue::from_typed_signed(value as i128, IntegerKind::Int8)
                .expect("every i8 fits its exact runtime kind"),
        ),
        (FfiType::I16, FfiValue::I16(value)) => Value::Int(
            IntegerValue::from_typed_signed(value as i128, IntegerKind::Int16)
                .expect("every i16 fits its exact runtime kind"),
        ),
        (FfiType::I32, FfiValue::I32(value)) => Value::Int(IntegerValue::from_i32(value)),
        (FfiType::I64, FfiValue::I64(value)) => Value::Int(IntegerValue::from_i64(value)),
        (FfiType::U8, FfiValue::U8(value)) => Value::Int(
            IntegerValue::from_typed_unsigned(value as u128, IntegerKind::Uint8)
                .expect("every u8 fits its exact runtime kind"),
        ),
        (FfiType::U16, FfiValue::U16(value)) => Value::Int(
            IntegerValue::from_typed_unsigned(value as u128, IntegerKind::Uint16)
                .expect("every u16 fits its exact runtime kind"),
        ),
        (FfiType::U32, FfiValue::U32(value)) => Value::Int(
            IntegerValue::from_typed_unsigned(value as u128, IntegerKind::Uint32)
                .expect("every u32 fits its exact runtime kind"),
        ),
        (FfiType::U64, FfiValue::U64(value)) => Value::Int(IntegerValue::from_u64(value)),
        (FfiType::F32, FfiValue::F32(value)) => Value::Float(f64::from(value)),
        (FfiType::F64, FfiValue::F64(value)) => Value::Float(value),
        (FfiType::OpaqueHandle, FfiValue::OpaqueHandle(handle)) => {
            let type_name = ty
                .opaque_name
                .clone()
                .ok_or_else(|| "opaque FFI metadata is missing its nominal type".to_string())?;
            Value::FfiHandle(
                FfiHandleValue::new(type_name, handle.as_ptr())
                    .ok_or_else(|| "FFI function returned a null opaque handle".to_string())?,
            )
        }
        _ => return Err(mismatch()),
    })
}

fn direct_ffi_write_back_mut_bytes(
    handle: *mut OpaqueValue,
    value: &FfiValue,
) -> std::result::Result<(), String> {
    let FfiValue::Bytes(bytes) = value else {
        return Err("FFI mutable byte view lost its byte buffer".to_string());
    };
    unsafe {
        value_mut(handle, |value| {
            *value = bytes_vec_value(bytes.clone());
        });
    }
    Ok(())
}

fn direct_ffi_error(symbol: &str, error: FfiError) -> ! {
    let code = if matches!(error, FfiError::NonCanonicalBoolReturn(_)) {
        "AU4001"
    } else {
        "AU4005"
    };
    runtime_diagnostic_error(Diagnostic::coded(
        code,
        format!("FFI call to `{symbol}` failed: {error}"),
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum DirectFfiCompletionError {
    Engine(FfiError),
    Runtime(String),
}

fn finish_direct_ffi_call(
    spec: &DirectFfiCallSpec,
    handles: &[i64],
    arguments: &[FfiValue],
    result: std::result::Result<FfiValue, FfiError>,
) -> std::result::Result<Value, DirectFfiCompletionError> {
    for (index, param) in spec.params.iter().enumerate() {
        if param.passing == ReceiverKind::BorrowMut {
            direct_ffi_write_back_mut_bytes(handles[index] as *mut OpaqueValue, &arguments[index])
                .map_err(DirectFfiCompletionError::Runtime)?;
        }
    }
    let result = result.map_err(DirectFfiCompletionError::Engine)?;
    direct_ffi_to_value(result, &spec.result).map_err(DirectFfiCompletionError::Runtime)
}

#[no_mangle]
pub extern "C-unwind" fn aura_direct_ffi_call(
    spec_ptr: *const u8,
    spec_len: i64,
    args_ptr: *const i64,
    arg_count: i64,
) -> *mut OpaqueValue {
    let spec_len = usize::try_from(spec_len)
        .unwrap_or_else(|_| runtime_error("invalid direct FFI call-spec length"));
    if spec_ptr.is_null() && spec_len != 0 {
        runtime_error("direct FFI call received a null call-spec pointer");
    }
    let spec_bytes = unsafe { slice::from_raw_parts(spec_ptr, spec_len) };
    let spec = decode_direct_ffi_call_spec(spec_bytes)
        .unwrap_or_else(|error| runtime_error(format!("invalid direct FFI call spec: {error}")));
    let arg_count = usize::try_from(arg_count)
        .unwrap_or_else(|_| runtime_error("invalid direct FFI argument count"));
    if args_ptr.is_null() && arg_count != 0 {
        runtime_error("direct FFI call received a null argument buffer");
    }
    if arg_count != spec.params.len() {
        runtime_error(format!(
            "direct FFI call spec expected {} argument(s), but received {arg_count}",
            spec.params.len()
        ));
    }
    let handles = unsafe { slice::from_raw_parts(args_ptr, arg_count) };
    let mut arguments = Vec::with_capacity(arg_count);
    for (index, (handle, param)) in handles.iter().zip(&spec.params).enumerate() {
        if *handle == 0 {
            runtime_error(format!(
                "direct FFI argument {} has a null runtime value",
                index + 1
            ));
        }
        let value = unsafe { value_ref(*handle as *mut OpaqueValue) };
        arguments.push(
            direct_value_to_ffi(&value, &param.ty).unwrap_or_else(|error| {
                runtime_diagnostic_error(Diagnostic::coded(
                    "AU4005",
                    format!("FFI call to `{}` failed: {error}", spec.symbol),
                ))
            }),
        );
    }
    let signature = FfiSignature::new(
        spec.params.iter().map(|param| param.ty.ffi_type).collect(),
        spec.result.ffi_type,
    );
    let result =
        unsafe { crate::ffi::call_process_symbol(&spec.symbol, &signature, &mut arguments) };
    let result =
        finish_direct_ffi_call(&spec, handles, &arguments, result).unwrap_or_else(|error| {
            match error {
                DirectFfiCompletionError::Engine(error) => direct_ffi_error(&spec.symbol, error),
                DirectFfiCompletionError::Runtime(error) => {
                    runtime_diagnostic_error(Diagnostic::coded(
                        "AU4005",
                        format!("FFI call to `{}` failed: {error}", spec.symbol),
                    ))
                }
            }
        });
    boxed_value(result)
}

fn headers_map_value(headers: Vec<(String, String)>) -> Value {
    Value::Map(MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("str"),
        entries: headers
            .into_iter()
            .map(|(key, value)| (Value::String(key), Value::String(value)))
            .collect(),
    })
}

fn expect_string_value(value: &Value, label: &str) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => runtime_error(format!(
            "`{}` expects `str`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn expect_bytes_value(value: &Value, label: &str) -> Vec<u8> {
    match value {
        Value::Vec(vector)
            if (vector.element_type == Type::named("uint8")
                || vector.element_type == Type::named("Unknown"))
                && vector
                    .elements
                    .iter()
                    .all(|element| matches!(element, Value::Int(_))) =>
        {
            let mut bytes = Vec::with_capacity(vector.elements.len());
            for element in &vector.elements {
                let Value::Int(value) = element else {
                    runtime_error(format!("`{}` expects `list[uint8]`", label));
                };
                let byte = value
                    .as_i128()
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or_else(|| runtime_error(format!("`{}` expects `list[uint8]`", label)));
                bytes.push(byte);
            }
            bytes
        }
        other => runtime_error(format!(
            "`{}` expects `list[uint8]`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn expect_bool_value(value: &Value, label: &str) -> bool {
    match value {
        Value::Bool(value) => *value,
        other => runtime_error(format!(
            "`{}` expects `bool`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn expect_i32_value(value: &Value, label: &str) -> i32 {
    match value {
        Value::Int(number) => number
            .as_i128()
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or_else(|| runtime_error(format!("`{}` expects `int32`", label))),
        other => runtime_error(format!(
            "`{}` expects `int32`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn expect_headers_map(value: &Value, label: &str) -> Vec<(String, String)> {
    match value {
        Value::Map(map)
            if (map.key_type == Type::named("str") || map.key_type == Type::named("Unknown"))
                && (map.value_type == Type::named("str")
                    || map.value_type == Type::named("Unknown")) =>
        {
            map.entries
                .iter()
                .map(|(key, value)| {
                    (
                        expect_string_value(key, label),
                        expect_string_value(value, label),
                    )
                })
                .collect()
        }
        other => runtime_error(format!(
            "`{}` expects `dict[str, str]`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn optional_timeout_result_from_ptr(
    value: *mut OpaqueValue,
    label: &str,
) -> io::Result<Option<StdDuration>> {
    if value.is_null() {
        return Ok(None);
    }
    match unsafe { value_ref(value) } {
        Value::Unit => Ok(None),
        value @ Value::Duration(_) => duration_value_to_host_timer(&value, label).map(Some),
        other => runtime_error(format!(
            "`{}` expects `Duration`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

#[cfg(test)]
fn optional_timeout_from_ptr(value: *mut OpaqueValue, label: &str) -> Option<StdDuration> {
    optional_timeout_result_from_ptr(value, label).unwrap_or_else(|error| direct_timer_error(error))
}

fn process_optional_timeout_result_from_ptr(
    value: *mut OpaqueValue,
    label: &str,
) -> io::Result<Option<StdDuration>> {
    optional_timeout_result_from_ptr(value, label)
}

#[cfg(test)]
fn process_optional_timeout_from_ptr(value: *mut OpaqueValue, label: &str) -> Option<StdDuration> {
    process_optional_timeout_result_from_ptr(value, label)
        .unwrap_or_else(|error| direct_timer_error(error))
}

fn duration_result_from_value(value: &Value, label: &str) -> io::Result<StdDuration> {
    match value {
        value @ Value::Duration(_) => duration_value_to_host_timer(value, label),
        other => runtime_error(format!(
            "`{}` expects `Duration`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn duration_result_from_ptr(value: *mut OpaqueValue, label: &str) -> io::Result<StdDuration> {
    duration_result_from_value(&unsafe { value_ref(value) }, label)
}

fn duration_from_ptr(value: *mut OpaqueValue, label: &str) -> StdDuration {
    duration_result_from_ptr(value, label).unwrap_or_else(|error| direct_timer_error(error))
}

macro_rules! io_timeout_or_return {
    ($value:expr, $label:expr) => {
        match optional_timeout_result_from_ptr($value, $label) {
            Ok(timeout) => timeout,
            Err(error) => return boxed_value(result_err(io_error(error))),
        }
    };
}

macro_rules! process_timeout_or_return {
    ($value:expr, $label:expr) => {
        match process_optional_timeout_result_from_ptr($value, $label) {
            Ok(timeout) => timeout,
            Err(error) => return boxed_value(result_err(process_error_from_io(error))),
        }
    };
}

fn supervisor_max_restarts_from_value(value: &Value, label: &str) -> Option<i32> {
    let value = expect_i32_value(value, label);
    if value < -1 {
        runtime_error(format!(
            "`{}` expects `max_restarts` to be -1 or greater",
            label
        ));
    }
    (value >= 0).then_some(value)
}

#[cfg(test)]
fn supervisor_max_restarts_from_ptr(value: *mut OpaqueValue, label: &str) -> Option<i32> {
    supervisor_max_restarts_from_value(&unsafe { value_ref(value) }, label)
}

fn expect_command_vec(value: &Value, label: &str) -> Vec<String> {
    match value {
        Value::Vec(vector)
            if vector.element_type == Type::named("str")
                || vector.element_type == Type::named("Unknown") =>
        {
            vector
                .elements
                .iter()
                .map(|element| expect_string_value(element, label))
                .collect()
        }
        other => runtime_error(format!(
            "`{}` expects `list[str]`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn expect_optional_string_value(value: &Value, label: &str) -> Option<String> {
    match value {
        Value::Unit => None,
        Value::EnumVariant(variant)
            if nominal_runtime_base_name(&variant.enum_name) == "Option"
                && variant.variant_name == "None" =>
        {
            None
        }
        Value::EnumVariant(variant)
            if nominal_runtime_base_name(&variant.enum_name) == "Option"
                && variant.variant_name == "Some" =>
        {
            match variant.payloads.as_slice() {
                [text] => Some(expect_string_value(text, label)),
                _ => runtime_error(format!(
                    "`{}` expects `Option[str]`, found malformed option payload",
                    label
                )),
            }
        }
        other => runtime_error(format!(
            "`{}` expects `Option[str]`, found `{}`",
            label,
            value_type_name(other)
        )),
    }
}

fn process_error_from_io(error: io::Error) -> Value {
    match error.kind() {
        io::ErrorKind::TimedOut => process_error_timed_out(),
        io::ErrorKind::Interrupted => process_error_cancelled(),
        _ => process_error_io(error),
    }
}

fn await_process_capture_task(task: Option<TaskValue>, label: &str) -> Vec<u8> {
    let Some(task) = task else {
        return Vec::new();
    };
    match direct_timer_result_or_trap(
        task.wait_result_with_cancellation_observed(None, Some(&current_cancellation())),
    ) {
        TaskWaitStatus::Ready(Ok(Value::Vec(vector)))
            if vector.element_type == Type::named("uint8") =>
        {
            vector
                .elements
                .into_iter()
                .map(|value| match value {
                    Value::Int(value) => value
                        .as_i128()
                        .and_then(|value| u8::try_from(value).ok())
                        .unwrap_or_else(|| {
                            runtime_error(format!(
                                "process {} capture returned a non-byte integer",
                                label
                            ))
                        }),
                    other => runtime_error(format!(
                        "process {} capture returned `{}` inside `list[uint8]`",
                        label,
                        other.render()
                    )),
                })
                .collect()
        }
        TaskWaitStatus::Ready(Ok(other)) => runtime_error(format!(
            "process {} capture returned `{}` instead of `list[uint8]`",
            label,
            other.render()
        )),
        TaskWaitStatus::Ready(Err(error)) => runtime_diagnostic_error(error),
        TaskWaitStatus::TimedOut => {
            runtime_error(format!("process {} capture timed out unexpectedly", label))
        }
        TaskWaitStatus::Cancelled => runtime_error(format!(
            "process {} capture was cancelled unexpectedly",
            label
        )),
    }
}

fn render_runtime_diagnostic(diagnostic: Diagnostic) -> String {
    if let Some(context) = DIRECT_PROGRAM_SOURCE.get() {
        diagnostic.render_with_source(&context.path, &context.source)
    } else {
        format!("error[{}]: {}", diagnostic.code, diagnostic.message)
    }
}

unsafe fn release_direct_cleanup_args(args: *mut i64, arg_count: usize) {
    if args.is_null() {
        return;
    }
    let values = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(args, arg_count)) };
    for value in values.iter().copied() {
        if value != 0 {
            unsafe {
                release_untracked_value(value as *mut OpaqueValue);
            }
        }
    }
}

fn drain_direct_cleanup_stack() {
    let key = direct_task_runtime_key();
    let already_draining = with_direct_task_runtime_state_for_key(key, |state| {
        if state.cleanup_draining {
            true
        } else {
            state.cleanup_draining = true;
            false
        }
    });
    if already_draining {
        return;
    }
    let _guard = DirectCleanupDrainGuard { key };
    let previous_depth = with_direct_task_runtime_state_for_key(key, |state| {
        let previous = state.call_depth;
        state.call_depth = 0;
        previous
    });
    let _depth_guard = DirectCallDepthGuard {
        key,
        previous: previous_depth,
    };
    let skip_max_depth_cleanup = direct_primary_runtime_diagnostic()
        .as_ref()
        .is_some_and(is_call_depth_diagnostic);
    loop {
        // Keep every registration owned by task-local state until its thunk
        // returns. A cleanup thunk may itself trap, and the scheduler then
        // force-resets the generated stack without running Rust destructors.
        // The forced-exit callback can still reclaim this snapshot and every
        // remaining outer registration from `DirectTaskRuntimeState`.
        let registration = with_direct_task_runtime_state_for_key(key, |state| {
            state.cleanup_stack.last().map(|registration| {
                (
                    registration.id,
                    registration.thunk_ptr,
                    registration.args,
                    registration.arg_count,
                    registration.call_depth,
                )
            })
        });
        let Some((id, thunk_ptr, args, arg_count, call_depth)) = registration else {
            break;
        };
        // Match the interpreter: a cleanup call captured at the saturated Aura
        // call depth cannot enter its `close` method during recursion unwinding.
        if skip_max_depth_cleanup && call_depth >= DIRECT_MAX_CALL_DEPTH {
            drop(take_direct_cleanup_registration(id));
            continue;
        }
        if thunk_ptr != 0 {
            let thunk: NativeThunk = unsafe { std::mem::transmute(thunk_ptr as usize) };
            let result = unsafe { thunk(args as *const i64, arg_count) };
            unsafe {
                aura_direct_release_value(result);
            }
        }
        drop(take_direct_cleanup_registration(id));
    }
}

fn emit_runtime_diagnostic_error(diagnostic: Diagnostic) -> ! {
    if direct_runtime_error_capture_enabled() {
        std::panic::panic_any(LightweightTaskFailureSignal(diagnostic));
    }
    if matches!(
        try_emit_internal_structured_diagnostic(&diagnostic),
        InternalDiagnosticEmission::NoChannel
    ) {
        let _ = writeln!(
            io::stderr().lock(),
            "{}",
            render_runtime_diagnostic(diagnostic)
        );
    }
    process::exit(1);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InternalDiagnosticEmission {
    /// No usable marker reached the parent, so human stderr remains the only
    /// reliable diagnostic path.
    NoChannel,
    /// The marker and complete structured record were emitted.
    Emitted,
    /// The marker reached the parent but the record failed. The parent owns
    /// reporting this as one JSON host error; human stderr must stay silent.
    SignaledWithoutRecord,
}

#[cfg(unix)]
fn write_internal_structured_diagnostic_to_fd(
    diagnostic: &Diagnostic,
    channel: &mut std::fs::File,
) -> io::Result<()> {
    let fallback_path = DIRECT_PROGRAM_SOURCE
        .get()
        .map(|context| context.path.as_str())
        .unwrap_or("<direct>");
    let encoded = serde_json::to_vec(&diagnostic.structured(fallback_path))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if encoded.len() > crate::MAX_INTERNAL_DIAGNOSTIC_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "structured native diagnostic exceeds the internal 1 MiB limit",
        ));
    }
    channel.write_all(&encoded)?;
    channel.flush()
}

fn try_emit_internal_structured_diagnostic(diagnostic: &Diagnostic) -> InternalDiagnosticEmission {
    #[cfg(unix)]
    {
        let Some(mut channels) = lock_internal_diagnostic_channels().take() else {
            return InternalDiagnosticEmission::NoChannel;
        };
        let (Some(mut data), Some(mut signal)) = (channels.data.take(), channels.signal.take())
        else {
            return InternalDiagnosticEmission::NoChannel;
        };
        if signal
            .write_all(&[crate::INTERNAL_DIAGNOSTIC_SIGNAL_MARKER])
            .and_then(|()| signal.flush())
            .is_err()
        {
            return InternalDiagnosticEmission::NoChannel;
        }
        if write_internal_structured_diagnostic_to_fd(diagnostic, &mut data).is_ok() {
            InternalDiagnosticEmission::Emitted
        } else {
            InternalDiagnosticEmission::SignaledWithoutRecord
        }
    }
    #[cfg(not(unix))]
    {
        let _ = diagnostic;
        InternalDiagnosticEmission::NoChannel
    }
}

fn runtime_diagnostic_error(diagnostic: Diagnostic) -> ! {
    let mut diagnostic = diagnostic.into_runtime_trap();
    capture_direct_runtime_frames_once(&mut diagnostic);
    if direct_cleanup_is_draining() {
        emit_runtime_diagnostic_error(direct_primary_runtime_diagnostic().unwrap_or(diagnostic));
    }
    let _primary_guard = DirectPrimaryDiagnosticGuard::install(diagnostic.clone());
    drain_direct_cleanup_stack();
    emit_runtime_diagnostic_error(diagnostic);
}

fn runtime_error(message: impl AsRef<str>) -> ! {
    runtime_diagnostic_error(Diagnostic::new(message.as_ref()))
}

fn runtime_error_at(span: Span, message: impl AsRef<str>) -> ! {
    runtime_diagnostic_error(Diagnostic::at(span, message.as_ref()))
}

fn runtime_diagnostic_error_at(mut diagnostic: Diagnostic, span: Option<Span>) -> ! {
    if diagnostic.span.is_none() {
        diagnostic.span = span;
    }
    runtime_diagnostic_error(diagnostic)
}

fn with_task_runtime_error_capture<T>(f: impl FnOnce() -> T) -> T {
    struct CaptureGuard {
        key: u64,
        previous: bool,
    }

    impl Drop for CaptureGuard {
        fn drop(&mut self) {
            with_direct_task_runtime_state_for_key(self.key, |state| {
                state.runtime_error_capture = self.previous;
            });
        }
    }

    let key = direct_task_runtime_key();
    let previous = with_direct_task_runtime_state_for_key(key, |state| {
        std::mem::replace(&mut state.runtime_error_capture, true)
    });
    let _guard = CaptureGuard { key, previous };
    f()
}

#[track_caller]
fn task_runtime_boundary<T>(f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) if payload.is::<TaskCancelledSignal>() => {
            if direct_runtime_error_capture_enabled() {
                drain_direct_cleanup_stack();
            }
            cancel_current_lightweight_task_boundary()
        }
        Err(payload) => match payload.downcast::<LightweightTaskFailureSignal>() {
            Ok(signal) => {
                let mut diagnostic = signal.0;
                capture_direct_runtime_frames_once(&mut diagnostic);
                // A trapping cleanup must not replace the failure that caused this
                // boundary to drain the remaining task-local cleanup registrations.
                let _primary_guard = DirectPrimaryDiagnosticGuard::install(diagnostic.clone());
                if direct_runtime_error_capture_enabled() {
                    drain_direct_cleanup_stack();
                }
                fail_current_lightweight_task(diagnostic)
            }
            Err(payload) => std::panic::resume_unwind(payload),
        },
    }
}

fn runtime_span(line: i64, column: i64) -> Option<Span> {
    if line <= 0 || column <= 0 {
        return None;
    }
    Some(Span::new(line as usize, column as usize))
}

fn value_type_name(value: impl Borrow<Value>) -> String {
    match value.borrow() {
        Value::Int(_) => "integer".to_string(),
        Value::Float(_) => "float64".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::String(_) => "str".to_string(),
        Value::Tuple(_) => "tuple".to_string(),
        Value::Vec(_) => "list".to_string(),
        Value::Array(array) => {
            format!("Array[{}]", array.dtype().runtime_type_name())
        }
        Value::Set(_) => "set".to_string(),
        Value::Map(_) => "dict".to_string(),
        Value::Duration(_) => "Duration".to_string(),
        Value::Rng(_) => "random.Rng".to_string(),
        Value::Range(_) => "Range".to_string(),
        Value::ModuleNamespace(namespace) => format!("module {}", namespace.path),
        Value::Function(function) => function.signature.to_string(),
        Value::FfiHandle(handle) => handle.type_name().to_string(),
        Value::Unit => "None".to_string(),
        Value::Instance(instance) => nominal_runtime_base_name(&instance.class_name).to_string(),
        Value::EnumVariant(variant) => nominal_runtime_base_name(&variant.enum_name).to_string(),
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
        Value::UnixListener(_) => "net.UnixListener".to_string(),
        Value::UnixStream(_) => "net.UnixStream".to_string(),
        Value::TlsListener(_) => "net.TlsListener".to_string(),
        Value::TlsStream(_) => "net.TlsStream".to_string(),
        Value::ProcessChild(_) => "process.Child".to_string(),
        Value::ProcessPipe(_) => "process.Pipe".to_string(),
        Value::ProcessCompleted(_) => "process.Completed".to_string(),
        Value::ProcessSupervisor(_) => "process.Supervisor".to_string(),
    }
}

fn inferred_collection_type(value: &Value) -> Type {
    if let Value::Function(function) = value {
        return function.signature.clone();
    }
    if let Some(runtime_type_name) = embedded_runtime_type_name(value) {
        return runtime_type_from_name(&runtime_type_name);
    }
    match value {
        Value::String(_) => Type::named("str"),
        Value::Bool(_) => Type::named("bool"),
        Value::Float(_) => Type::named("float64"),
        Value::Tuple(tuple) => Type::Tuple(tuple.element_types.clone()),
        Value::Vec(vector) => Type::Named("list".to_string(), vec![vector.element_type.clone()]),
        Value::Array(array) => Type::Named("Array".to_string(), vec![array.element_type()]),
        Value::Set(set) => Type::Named("set".to_string(), vec![set.element_type.clone()]),
        Value::Map(map) => Type::Named(
            "dict".to_string(),
            vec![map.key_type.clone(), map.value_type.clone()],
        ),
        Value::Duration(_) => Type::named("Duration"),
        Value::Rng(_) => Type::named("random.Rng"),
        Value::Range(_) => Type::named("Range"),
        Value::Function(function) => function.signature.clone(),
        Value::FfiHandle(handle) => Type::named(handle.type_name()),
        Value::Instance(instance) => Type::named(nominal_runtime_base_name(&instance.class_name)),
        Value::EnumVariant(variant) => Type::named(nominal_runtime_base_name(&variant.enum_name)),
        Value::Channel(_) => Type::named("Queue"),
        Value::Task(_) => Type::named("Task"),
        Value::TaskGroup(_) => Type::named("TaskGroup"),
        Value::File(_) => Type::named("fs.File"),
        Value::TcpListener(_) => Type::named("net.TcpListener"),
        Value::TcpStream(_) => Type::named("net.TcpStream"),
        Value::UdpSocket(_) => Type::named("net.UdpSocket"),
        Value::UdpDatagram(_) => Type::named("net.UdpDatagram"),
        Value::HttpListener(_) => Type::named("net.HttpListener"),
        Value::HttpExchange(_) => Type::named("net.HttpExchange"),
        Value::HttpResponse(_) => Type::named("net.HttpResponse"),
        Value::WebSocketListener(_) => Type::named("net.WebSocketListener"),
        Value::WebSocket(_) => Type::named("net.WebSocket"),
        Value::UnixListener(_) => Type::named("net.UnixListener"),
        Value::UnixStream(_) => Type::named("net.UnixStream"),
        Value::TlsListener(_) => Type::named("net.TlsListener"),
        Value::TlsStream(_) => Type::named("net.TlsStream"),
        Value::ProcessChild(_) => Type::named("process.Child"),
        Value::ProcessPipe(_) => Type::named("process.Pipe"),
        Value::ProcessCompleted(_) => Type::named("process.Completed"),
        Value::ProcessSupervisor(_) => Type::named("process.Supervisor"),
        Value::Int(_) => Type::named("Unknown"),
        Value::ModuleNamespace(_) | Value::Unit => Type::named("Unknown"),
    }
}

fn compare_values(
    left: Value,
    right: Value,
    op: BinaryOp,
) -> std::result::Result<Value, Diagnostic> {
    if matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
        return Ok(Value::Bool(match op {
            BinaryOp::Eq => left == right,
            BinaryOp::NotEq => left != right,
            _ => unreachable!("equality branch only handles `==` and `!=`"),
        }));
    }
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => Ok(Value::Bool(match op {
            BinaryOp::Less => left < right,
            BinaryOp::LessEq => left <= right,
            BinaryOp::Greater => left > right,
            BinaryOp::GreaterEq => left >= right,
            _ => {
                return Err(Diagnostic::new(format!(
                    "unsupported comparison operator `{:?}` for int values",
                    op
                )))
            }
        })),
        (Value::Float(left), Value::Float(right)) => Ok(Value::Bool(match op {
            BinaryOp::Less => left < right,
            BinaryOp::LessEq => left <= right,
            BinaryOp::Greater => left > right,
            BinaryOp::GreaterEq => left >= right,
            _ => {
                return Err(Diagnostic::new(format!(
                    "unsupported comparison operator `{:?}` for float values",
                    op
                )))
            }
        })),
        (Value::String(left), Value::String(right)) => Ok(Value::Bool(match op {
            BinaryOp::Less => left < right,
            BinaryOp::LessEq => left <= right,
            BinaryOp::Greater => left > right,
            BinaryOp::GreaterEq => left >= right,
            _ => {
                return Err(Diagnostic::new(format!(
                    "unsupported comparison operator `{:?}` for string values",
                    op
                )))
            }
        })),
        (Value::Duration(left), Value::Duration(right)) => Ok(Value::Bool(match op {
            BinaryOp::Less => left < right,
            BinaryOp::LessEq => left <= right,
            BinaryOp::Greater => left > right,
            BinaryOp::GreaterEq => left >= right,
            _ => {
                return Err(Diagnostic::new(format!(
                    "unsupported comparison operator `{:?}` for Duration values",
                    op
                )))
            }
        })),
        (left, right) => Err(Diagnostic::new(format!(
            "unsupported comparison between `{}` and `{}`",
            value_type_name(&left),
            value_type_name(&right)
        ))),
    }
}

fn unsupported_binary_operands(operator: &str, left: &Value, right: &Value) -> Diagnostic {
    Diagnostic::new(format!(
        "unsupported `{operator}` operands `{}` and `{}`",
        value_type_name(left),
        value_type_name(right)
    ))
}

fn eval_binary_value(
    left: Value,
    right: Value,
    op: BinaryOp,
) -> std::result::Result<Value, Diagnostic> {
    eval_binary_value_with_float_width(left, right, op, FloatPowerWidth::Float64)
}

fn eval_binary_value_with_float_width(
    left: Value,
    right: Value,
    op: BinaryOp,
    float_width: FloatPowerWidth,
) -> std::result::Result<Value, Diagnostic> {
    match op {
        BinaryOp::And => match (left, right) {
            (Value::Bool(left), Value::Bool(right)) => Ok(Value::Bool(left && right)),
            (left, right) => Err(Diagnostic::new(format!(
                "logical `and` expects bool operands, found `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Or => match (left, right) {
            (Value::Bool(left), Value::Bool(right)) => Ok(Value::Bool(left || right)),
            (left, right) => Err(Diagnostic::new(format!(
                "logical `or` expects bool operands, found `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Less
        | BinaryOp::LessEq
        | BinaryOp::Greater
        | BinaryOp::GreaterEq => compare_values(left, right, op),
        BinaryOp::Add => match (left, right) {
            (Value::Duration(left), Value::Duration(right)) => left
                .checked_add(right)
                .map(Value::Duration)
                .ok_or_else(|| Diagnostic::new("duration overflow")),
            (Value::Int(left), Value::Int(right)) => match left.checked_add(right) {
                Some(value) => Ok(Value::Int(value)),
                None => Err(Diagnostic::new("integer overflow")),
            },
            (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left + right)),
            (Value::String(left), Value::String(right)) => {
                Ok(Value::String(concat_strings_checked(left, &right)?))
            }
            (left, right) => Err(Diagnostic::new(format!(
                "unsupported `+` operands `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Sub => match (left, right) {
            (Value::Duration(left), Value::Duration(right)) => left
                .checked_sub(right)
                .map(Value::Duration)
                .ok_or_else(|| Diagnostic::new("duration overflow")),
            (Value::Int(left), Value::Int(right)) => match left.checked_sub(right) {
                Some(value) => Ok(Value::Int(value)),
                None => Err(Diagnostic::new("integer overflow")),
            },
            (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left - right)),
            (left, right) => Err(Diagnostic::new(format!(
                "unsupported `-` operands `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Mul => match (left, right) {
            (Value::Duration(duration), Value::Int(factor))
            | (Value::Int(factor), Value::Duration(duration)) => factor
                .as_i128()
                .and_then(|factor| duration.checked_mul(factor))
                .map(Value::Duration)
                .ok_or_else(|| Diagnostic::new("duration overflow")),
            (Value::Int(left), Value::Int(right)) => match left.checked_mul(right) {
                Some(value) => Ok(Value::Int(value)),
                None => Err(Diagnostic::new("integer overflow")),
            },
            (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left * right)),
            (left, right) => Err(Diagnostic::new(format!(
                "unsupported `*` operands `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Div => match (left, right) {
            (Value::Int(_), Value::Int(right)) if right.is_zero() => {
                Err(Diagnostic::new("division by zero"))
            }
            (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                left.checked_div(right)
                    .expect("non-zero integer division is total"),
            )),
            (Value::Float(_), Value::Float(0.0)) => Err(Diagnostic::new("division by zero")),
            (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left / right)),
            (left, right) => Err(unsupported_binary_operands("/", &left, &right)),
        },
        BinaryOp::FloorDiv => match (left, right) {
            (Value::Duration(_), Value::Int(right)) if right.is_zero() => {
                Err(Diagnostic::new("division by zero"))
            }
            (Value::Duration(left), Value::Int(right)) => right
                .as_i128()
                .and_then(|right| checked_i128_floor_div(left, right))
                .map(Value::Duration)
                .ok_or_else(|| Diagnostic::new("duration overflow")),
            (Value::Int(_), Value::Int(right)) if right.is_zero() => {
                Err(Diagnostic::new("division by zero"))
            }
            (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                left.checked_floor_div(right)
                    .expect("non-zero matching integer floor division is total"),
            )),
            (Value::Float(_), Value::Float(0.0)) => Err(Diagnostic::new("division by zero")),
            (Value::Float(left), Value::Float(right)) => {
                Ok(Value::Float(float_floor_divmod(left, right).0))
            }
            (left, right) => Err(unsupported_binary_operands("//", &left, &right)),
        },
        BinaryOp::Mod => match (left, right) {
            (Value::Int(_), Value::Int(right)) if right.is_zero() => {
                Err(Diagnostic::new("division by zero"))
            }
            (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                left.checked_floor_rem(right)
                    .expect("non-zero integer remainder is total"),
            )),
            (Value::Float(_), Value::Float(0.0)) => Err(Diagnostic::new("division by zero")),
            (Value::Float(left), Value::Float(right)) => {
                Ok(Value::Float(float_floor_divmod(left, right).1))
            }
            (left, right) => Err(Diagnostic::new(format!(
                "unsupported `%` operands `{}` and `{}`",
                value_type_name(&left),
                value_type_name(&right)
            ))),
        },
        BinaryOp::Pow => match (left, right) {
            (Value::Int(left), Value::Int(right)) => left
                .checked_pow(right)
                .map(Value::Int)
                .map_err(native_integer_power_diagnostic),
            (Value::Float(left), Value::Float(right)) => {
                float_power(left, right, float_width).map(Value::Float)
            }
            (left, right) => Err(unsupported_binary_operands("**", &left, &right)),
        },
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => match (left, right) {
            (Value::Int(left), Value::Int(right)) => {
                let result = match op {
                    BinaryOp::BitAnd => left.checked_bitand(right),
                    BinaryOp::BitOr => left.checked_bitor(right),
                    BinaryOp::BitXor => left.checked_bitxor(right),
                    _ => unreachable!(),
                };
                result.map(Value::Int).ok_or_else(|| {
                    Diagnostic::coded("AU2002", "bitwise integer operand types must match")
                })
            }
            (left, right) => Err(unsupported_binary_operands("bitwise", &left, &right)),
        },
        BinaryOp::Shl | BinaryOp::Shr => match (left, right) {
            (Value::Int(left), Value::Int(right)) => {
                let result = if op == BinaryOp::Shl {
                    left.checked_shl(right)
                } else {
                    left.checked_shr(right)
                };
                result
                    .map(Value::Int)
                    .map_err(native_integer_shift_diagnostic)
            }
            (left, right) => Err(unsupported_binary_operands("shift", &left, &right)),
        },
    }
}

fn native_integer_power_diagnostic(error: IntegerPowerError) -> Diagnostic {
    match error {
        IntegerPowerError::MismatchedKinds => {
            Diagnostic::coded("AU2002", "integer power operand types must match")
        }
        IntegerPowerError::NegativeExponent => Diagnostic::coded(
            "AU4001",
            "runtime negative integer exponent; use explicit floating operands for fractional power",
        ),
        IntegerPowerError::Overflow => Diagnostic::coded("AU4002", "integer power overflow"),
    }
}

fn native_integer_shift_diagnostic(error: IntegerShiftError) -> Diagnostic {
    match error {
        IntegerShiftError::MismatchedKinds => {
            Diagnostic::coded("AU2002", "shift operand types must match")
        }
        IntegerShiftError::InvalidCount { count, width } => Diagnostic::coded(
            "AU4002",
            format!("integer shift count `{count}` is outside the required range `0..{width}`"),
        ),
        IntegerShiftError::Overflow => Diagnostic::coded("AU4002", "integer left shift overflow"),
    }
}

fn checked_i128_floor_div(left: i128, right: i128) -> Option<i128> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder != 0 && (remainder < 0) != (right < 0) {
        quotient.checked_sub(1)
    } else {
        Some(quotient)
    }
}

fn eval_unary_value(value: Value, op: UnaryOp) -> std::result::Result<Value, Diagnostic> {
    match (op, value) {
        (UnaryOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        (UnaryOp::Neg, Value::Int(value)) => match value.checked_neg() {
            Some(value) => Ok(Value::Int(value)),
            None => Err(Diagnostic::new("integer overflow")),
        },
        (UnaryOp::Neg, Value::Float(value)) => Ok(Value::Float(-value)),
        (UnaryOp::BitNot, Value::Int(value)) => value
            .bitnot()
            .map(Value::Int)
            .ok_or_else(|| Diagnostic::coded("AU4001", "invalid typed integer for unary `~`")),
        (UnaryOp::Not, other) => Err(Diagnostic::new(format!(
            "`not` expects `bool`, found `{}`",
            value_type_name(&other)
        ))),
        (UnaryOp::Neg, other) => Err(Diagnostic::new(format!(
            "unary `-` expects a numeric value, found `{}`",
            value_type_name(&other)
        ))),
        (UnaryOp::BitNot, other) => Err(Diagnostic::coded(
            "AU2003",
            format!(
                "unary `~` expects an integer value, found `{}`",
                value_type_name(&other)
            ),
        )),
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_runtime_init(
    path_ptr: *const u8,
    path_len: usize,
    source_ptr: *const u8,
    source_len: usize,
) {
    initialize_internal_diagnostic_channels();
    task_runtime_boundary(|| {
        clear_direct_task_runtime_states();
        clear_direct_module_constants();
        let _ = DIRECT_PROGRAM_SOURCE.set(ProgramSourceContext {
            path: decode_bytes(path_ptr, path_len),
            source: decode_bytes(source_ptr, source_len),
        });
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_run_root(thunk_ptr: i64) -> i32 {
    task_runtime_boundary(|| {
        if thunk_ptr == 0 {
            runtime_error("invalid direct root thunk pointer");
        }
        let thunk: NativeThunk = unsafe { std::mem::transmute(thunk_ptr as usize) };
        struct ModuleConstantCleanup;
        impl Drop for ModuleConstantCleanup {
            fn drop(&mut self) {
                clear_direct_module_constants();
            }
        }
        let _module_constant_cleanup = ModuleConstantCleanup;
        let result = run_direct_root_task(thunk);
        match result {
            Ok(Value::Int(value)) => value.as_i128().unwrap_or_default() as i32,
            Ok(Value::Unit) => 0,
            Ok(other) => runtime_error(format!(
                "direct main entry must return `int32` or `None`, found `{}`",
                value_type_name(&other)
            )),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

fn run_direct_root_task(thunk: NativeThunk) -> std::result::Result<Value, Diagnostic> {
    unsafe {
        run_direct_root_task_with_forced_exit_cleanup(thunk, || {
            discard_current_direct_task_runtime_state();
        })
    }
}

/// Runs a generated direct root whose frames cannot be unwound after a
/// scheduler boundary turns a trap or cancellation into a forced exit.
///
/// # Safety
///
/// `forced_exit_cleanup` must release every direct-runtime resource that is
/// externalized from the generated root's abandoned coroutine frames. It must
/// not call language cleanup thunks, because the generated stack has already
/// been reset when the callback runs, and it must not panic because scheduler
/// teardown must continue retiring the remaining task records.
unsafe fn run_direct_root_task_with_forced_exit_cleanup<C>(
    thunk: NativeThunk,
    forced_exit_cleanup: C,
) -> std::result::Result<Value, Diagnostic>
where
    C: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(DIRECT_RUNTIME_STACK_SIZE)
        .spawn(move || unsafe {
            run_lightweight_root_task_with_forced_exit_cleanup(
                move || {
                    with_direct_task_runtime_scope(|| {
                        with_cancellation_scope(CancellationContext::default(), || {
                            Ok(with_task_runtime_error_capture(|| {
                                let result_ptr = thunk(std::ptr::null(), 0);
                                consume_value(result_ptr)
                            }))
                        })
                    })
                },
                forced_exit_cleanup,
            )
        })
        .map_err(|error| {
            Diagnostic::new(format!("failed to start direct runtime thread: {}", error))
        })?
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_enter_call(
    line: i64,
    column: i64,
    function_ptr: *const u8,
    function_len: usize,
) {
    unsafe {
        aura_direct_enter_call_with_frame(
            line,
            column,
            std::ptr::null(),
            0,
            function_ptr,
            function_len,
        );
    }
}

fn direct_program_path_frame_text() -> Option<DirectFrameText> {
    DIRECT_PROGRAM_SOURCE.get().map(|context| {
        DirectFrameText::Static(DirectStaticFrameText {
            address: context.path.as_ptr() as usize,
            len: context.path.len(),
        })
    })
}

#[cold]
#[inline(never)]
fn reject_invalid_direct_frame_utf8() -> ! {
    task_runtime_boundary(|| runtime_error("aura direct runtime received invalid UTF-8 bytes"))
}

#[cold]
#[inline(never)]
fn reject_direct_call_depth(line: i64, column: i64, function: &DirectFrameText) -> ! {
    task_runtime_boundary(|| {
        let message = format!(
            "maximum call depth of {} exceeded while calling `{}`",
            DIRECT_MAX_CALL_DEPTH,
            function.as_str()
        );
        if line > 0 && column > 0 {
            runtime_error_at(Span::new(line as usize, column as usize), message);
        }
        runtime_error(message);
    })
}

/// Enters one generated Aura call frame.
///
/// # Safety
///
/// Each non-null byte range must remain readable and unchanged until the
/// matching `aura_direct_exit_call`. UTF-8 is validated by the runtime before
/// the range is retained. Native codegen satisfies this private ABI contract
/// with immutable object-file data.
#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_enter_call_with_frame(
    line: i64,
    column: i64,
    path_ptr: *const u8,
    path_len: usize,
    function_ptr: *const u8,
    function_len: usize,
) {
    let function = match unsafe { DirectFrameText::validate_static(function_ptr, function_len) } {
        Ok(function) => function,
        Err(()) => reject_invalid_direct_frame_utf8(),
    };
    let path = if path_ptr.is_null() || path_len == 0 {
        direct_program_path_frame_text()
    } else {
        match unsafe { DirectFrameText::validate_static(path_ptr, path_len) } {
            Ok(path) => Some(path),
            Err(()) => reject_invalid_direct_frame_utf8(),
        }
    };
    let start = Span::new(
        usize::try_from(line).unwrap_or_default(),
        usize::try_from(column).unwrap_or_default(),
    );
    let frame = DirectRuntimeCallFrame {
        function: function.clone(),
        span: DirectRuntimeSourceSpan::point(path, start),
    };
    let depth_exceeded = with_direct_task_runtime_state(|state| {
        if state.call_depth >= DIRECT_MAX_CALL_DEPTH {
            true
        } else {
            state.call_depth += 1;
            state.call_frames.push(frame);
            false
        }
    });
    if depth_exceeded {
        reject_direct_call_depth(line, column, &function);
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_exit_call() {
    with_direct_task_runtime_state(|state| {
        if state.call_depth > 0 {
            state.call_depth -= 1;
            let _ = state.call_frames.pop();
        }
    });
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_i64(value: i64) {
    task_runtime_boundary(|| {
        write_stdout(&format!("{}\n", value));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_u64(value: u64) {
    task_runtime_boundary(|| {
        write_stdout(&format!("{}\n", value));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_f64(value: f64) {
    task_runtime_boundary(|| {
        write_stdout(&format!("{}\n", render_float(value)));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_f32(value: f64) {
    task_runtime_boundary(|| {
        write_stdout(&format!("{}\n", render_float32(value as f32)));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_bool(value: i64) {
    task_runtime_boundary(|| {
        write_stdout(&format!("{}\n", render_bool(value)));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_i64(value: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_typed_value(Value::Int(IntegerValue::from_i64(value)), "int64"))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_i32(value: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value =
            i32::try_from(value).unwrap_or_else(|_| runtime_error(int32_overflow_message(value)));
        boxed_typed_value(Value::Int(IntegerValue::from_i32(value)), "int32")
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_u64(value: u64) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_typed_value(Value::Int(IntegerValue::from_u64(value)), "uint64"))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_uint_literal(
    ptr: *const u8,
    len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = decode_bytes(ptr, len);
        let value = match text.parse::<u128>() {
            Ok(value) => value,
            Err(_) => runtime_error(format!("invalid embedded uint literal `{}`", text)),
        };
        boxed_value(Value::Int(IntegerValue::from_literal(value)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_f64(value: f64) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::Float(value)))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_bool(value: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::Bool(value != 0)))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_function_value(
    thunk_ptr: i64,
    default_binder_ptr: i64,
    name_ptr: *const u8,
    name_len: usize,
    signature_ptr: *const u8,
    signature_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        if thunk_ptr == 0 {
            runtime_error("direct runtime received a null function thunk");
        }
        let signature = serde_json::from_str::<Type>(&decode_bytes(signature_ptr, signature_len))
            .unwrap_or_else(|error| {
                runtime_error(format!(
                    "direct runtime received invalid function signature metadata: {error}"
                ))
            });
        boxed_value(Value::Function(Box::new(FunctionValue {
            name: decode_bytes(name_ptr, name_len),
            signature,
            source_path: (path_len > 0).then(|| decode_bytes(path_ptr, path_len)),
            entry_span: Span::new(
                usize::try_from(line).unwrap_or_default(),
                usize::try_from(column).unwrap_or_default(),
            ),
            direct_thunk: Some(thunk_ptr),
            direct_default_binder: Some(default_binder_ptr),
            closure_environment: None,
        })))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_closure_value(
    function: *mut OpaqueValue,
    captures_ptr: *mut i64,
    capture_count: i64,
    capture_modes_ptr: *const i64,
    consuming: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let capture_count = usize::try_from(capture_count)
            .unwrap_or_else(|_| runtime_error("invalid closure capture count"));
        let captures = if capture_count == 0 {
            Vec::new()
        } else {
            unsafe {
                consume_owned_opaque_buffer_for(captures_ptr, capture_count, "closure capture")
            }
        };
        let function = unsafe { consume_owned_value(function) };
        let Value::Function(mut function) = function else {
            runtime_error("direct closure construction expected a function value");
        };
        function.closure_environment = Some(Arc::new(ClosureEnvironment::new(
            captures
                .into_iter()
                .enumerate()
                .map(|(index, value)| ClosureCaptureValue {
                    name: format!("__capture_{index}"),
                    ty: Type::named("Unknown"),
                    value,
                    source_place: None,
                    mutable: !capture_modes_ptr.is_null()
                        && unsafe { *capture_modes_ptr.add(index) } != 0,
                })
                .collect(),
            consuming != 0,
        )));
        boxed_value(Value::Function(function))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_closure_capture(
    function: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let index = usize::try_from(index)
            .unwrap_or_else(|_| runtime_error("invalid closure capture index"));
        let Value::Function(function) = (unsafe { value_ref(function) }) else {
            runtime_error("closure capture access expected a function value");
        };
        let environment = function
            .closure_environment
            .as_ref()
            .unwrap_or_else(|| runtime_error("function has no closure environment"));
        let value = environment
            .capture_value(index)
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        boxed_value(value)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_function_bind_defaults(
    function: *mut OpaqueValue,
    args: *mut i64,
    arg_count: i64,
    transfer_defaults: i64,
) {
    task_runtime_boundary(|| {
        let function = match unsafe { value_ref(function) } {
            Value::Function(function) => function,
            other => runtime_error(format!(
                "indirect call expected a function value, found `{}`",
                value_type_name(&other)
            )),
        };
        // Lambda parameters cannot declare defaults. Hidden captures belong to
        // the closure environment and must never be exposed to the ordinary
        // declaration binder.
        if function.closure_environment.is_some() {
            return;
        }
        let binder_ptr = function
            .direct_default_binder
            .unwrap_or_else(|| runtime_error("direct function value has no native default binder"));
        let binder: unsafe extern "C-unwind" fn(*mut i64, usize, i64) =
            unsafe { std::mem::transmute(binder_ptr as usize) };
        let arg_count = usize::try_from(arg_count)
            .unwrap_or_else(|_| runtime_error("invalid indirect-call arg count"));
        unsafe { binder(args, arg_count, transfer_defaults) };
    })
}

struct DirectClosureCallBuffer {
    handles: Vec<i64>,
    public_args: *mut i64,
    capture_count: usize,
    public_count: usize,
    copy_writebacks: bool,
}

impl DirectClosureCallBuffer {
    fn copy_public_writebacks(&mut self) {
        self.copy_writebacks = true;
    }
}

impl Drop for DirectClosureCallBuffer {
    fn drop(&mut self) {
        if self.copy_writebacks {
            for index in 0..self.public_count {
                unsafe {
                    *self.public_args.add(index) = self.handles[self.capture_count + index];
                }
                self.handles[self.capture_count + index] = 0;
            }
        }
        // On an unwind every still-live public argument is released here and
        // no mutable writeback is installed. On normal return the thunk has
        // cleared consumed slots, while the loop above transferred only its
        // public writebacks back to the caller.
        for handle in self.handles.drain(..).filter(|handle| *handle != 0) {
            let value = handle as *mut OpaqueValue;
            unregister_direct_owned_value(value);
            unsafe { release_untracked_value(value) };
        }
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_function_call(
    function: *mut OpaqueValue,
    args: *mut i64,
    arg_count: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let function = match unsafe { value_ref(function) } {
            Value::Function(function) => function,
            other => runtime_error(format!(
                "indirect call expected a function value, found `{}`",
                value_type_name(&other)
            )),
        };
        let thunk_ptr = function
            .direct_thunk
            .unwrap_or_else(|| runtime_error("direct function value has no native thunk"));
        let thunk: NativeThunk = unsafe { std::mem::transmute(thunk_ptr as usize) };
        let arg_count = usize::try_from(arg_count)
            .unwrap_or_else(|_| runtime_error("invalid indirect-call arg count"));
        let Some(environment) = &function.closure_environment else {
            return unsafe { thunk(args, arg_count) };
        };
        let captures = environment
            .arguments(&function.name)
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let capture_count = captures.len();
        let mutable_capture_indices = captures
            .iter()
            .enumerate()
            .filter_map(|(index, capture)| capture.mutable.then_some(index))
            .collect::<Vec<_>>();
        let mut buffer = DirectClosureCallBuffer {
            handles: Vec::with_capacity(capture_count + arg_count),
            public_args: args,
            capture_count,
            public_count: arg_count,
            copy_writebacks: false,
        };
        buffer.handles.extend(
            captures
                .into_iter()
                .map(|capture| boxed_value(capture.value) as i64),
        );
        for index in 0..arg_count {
            let value = unsafe { *args.add(index) };
            buffer.handles.push(value);
            unsafe { *args.add(index) = 0 };
        }
        let result = unsafe { thunk(buffer.handles.as_mut_ptr(), buffer.handles.len()) };
        for index in mutable_capture_indices {
            let handle = buffer.handles[index] as *mut OpaqueValue;
            let value = unsafe { value_ref(handle) };
            let value = try_clone_array_containing_value(&value)
                .unwrap_or_else(|error| runtime_diagnostic_error(error));
            environment
                .write_back_mutable(index, value)
                .unwrap_or_else(|error| runtime_diagnostic_error(error));
        }
        buffer.copy_public_writebacks();
        result
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_function_default_binder(function: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(function) } {
        Value::Function(function) => function
            .direct_default_binder
            .unwrap_or_else(|| runtime_error("direct function value has no native default binder")),
        other => runtime_error(format!(
            "indirect call expected a function value, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_function_thunk(function: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(function) } {
        Value::Function(function) => function
            .direct_thunk
            .unwrap_or_else(|| runtime_error("direct function value has no native thunk")),
        other => runtime_error(format!(
            "indirect call expected a function value, found `{}`",
            value_type_name(other)
        )),
    })
}

#[derive(Copy, Clone)]
enum DirectModuleConstantState {
    Initializing,
    Ready(usize),
    Failed,
}

static DIRECT_MODULE_CONSTANTS: OnceLock<Mutex<HashMap<String, DirectModuleConstantState>>> =
    OnceLock::new();
static DIRECT_MODULE_CONSTANT_ORDER: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn direct_module_constants() -> &'static Mutex<HashMap<String, DirectModuleConstantState>> {
    DIRECT_MODULE_CONSTANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn direct_module_constant_order() -> &'static Mutex<Vec<String>> {
    DIRECT_MODULE_CONSTANT_ORDER.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_direct_module_constants(
) -> std::sync::MutexGuard<'static, HashMap<String, DirectModuleConstantState>> {
    direct_module_constants()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn clear_direct_module_constants() {
    let mut states = lock_direct_module_constants();
    let mut order = direct_module_constant_order()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for key in order.drain(..).rev() {
        if let Some(DirectModuleConstantState::Ready(address)) = states.remove(&key) {
            unsafe { release_untracked_value(address as *mut OpaqueValue) };
        }
    }
    states.clear();
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_module_constant(
    key_ptr: *const u8,
    key_len: usize,
    initializer_thunk: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = decode_bytes(key_ptr, key_len);
        {
            let states = lock_direct_module_constants();
            match states.get(&key).copied() {
                Some(DirectModuleConstantState::Ready(address)) => {
                    let value = address as *mut OpaqueValue;
                    unsafe { retain_untracked_value(value) };
                    register_direct_owned_value(value);
                    return value;
                }
                Some(DirectModuleConstantState::Initializing) => runtime_diagnostic_error(
                    Diagnostic::coded(
                        "AU4001",
                        format!("module constant `{key}` was read while its module was still initializing"),
                    ),
                ),
                Some(DirectModuleConstantState::Failed) => runtime_diagnostic_error(
                    Diagnostic::coded(
                        "AU4001",
                        format!("module constant `{key}` previously failed to initialize"),
                    ),
                ),
                None => {}
            }
        }
        if initializer_thunk == 0 {
            runtime_error(format!(
                "module constant `{key}` has a null initializer thunk"
            ));
        }
        lock_direct_module_constants().insert(key.clone(), DirectModuleConstantState::Initializing);
        struct FailedInitialization(String);
        impl Drop for FailedInitialization {
            fn drop(&mut self) {
                let mut states = lock_direct_module_constants();
                if matches!(
                    states.get(&self.0),
                    Some(DirectModuleConstantState::Initializing)
                ) {
                    states.insert(self.0.clone(), DirectModuleConstantState::Failed);
                }
            }
        }
        let guard = FailedInitialization(key.clone());
        let thunk: NativeThunk = unsafe { std::mem::transmute(initializer_thunk as usize) };
        let value = unsafe { thunk(std::ptr::null(), 0) };
        if value.is_null() {
            runtime_error(format!(
                "module constant `{key}` initializer returned a null value"
            ));
        }
        // The registry owns one untracked reference until runtime shutdown;
        // the thunk's tracked reference remains the caller's read result.
        unsafe { retain_untracked_value(value) };
        lock_direct_module_constants()
            .insert(key, DirectModuleConstantState::Ready(value as usize));
        direct_module_constant_order()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(guard.0.clone());
        std::mem::forget(guard);
        value
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_box_unit() -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::Unit))
}

#[cfg_attr(not(coverage), no_mangle)]
/// # Safety
///
/// `value` must be either null or a live `OpaqueValue` pointer allocated by the Aura direct
/// runtime. Callers must only retain pointers whose storage is still owned by the current process.
pub unsafe extern "C-unwind" fn aura_direct_retain_value(
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        unsafe {
            retain_untracked_value(value);
        }
        register_direct_owned_value(value);
        value
    })
}

#[cfg_attr(not(coverage), no_mangle)]
/// # Safety
///
/// `value` must be either null or a live `OpaqueValue` pointer allocated by the Aura direct
/// runtime. Each successful retain/release pair must be balanced according to the direct-runtime
/// ownership contract.
pub unsafe extern "C-unwind" fn aura_direct_release_value(value: *mut OpaqueValue) {
    task_runtime_boundary(|| {
        unregister_direct_owned_value(value);
        unsafe {
            release_untracked_value(value);
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_literal(
    ptr: *const u8,
    len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::String(decode_bytes(ptr, len))))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_stringify_value(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let rendered = unsafe { value_ref(value) }.render();
        boxed_value(Value::String(rendered))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_format_value(
    value: *mut OpaqueValue,
    spec_ptr: *const u8,
    spec_len: usize,
    type_ptr: *const u8,
    type_len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let spec = decode_bytes(spec_ptr, spec_len);
        let value_type = Type::named(decode_bytes(type_ptr, type_len));
        let value = unsafe { value_ref(value) };
        match format_runtime_value(&value, &value_type, &spec) {
            Ok(rendered) => boxed_value(Value::String(rendered)),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_duration_literal(low: i64, high: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = ((high as i128) << 64) | (low as u64 as i128);
        boxed_value(Value::Duration(value))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_duration_from_i64(
    value: i64,
    unit_nanoseconds: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let unit_nanoseconds = match i128::from(unit_nanoseconds) {
            crate::runtime_value::NANOS_PER_MILLISECOND
            | crate::runtime_value::NANOS_PER_SECOND
            | crate::runtime_value::NANOS_PER_MINUTE => i128::from(unit_nanoseconds),
            other => runtime_error(format!("unknown Duration constructor unit `{other}`")),
        };
        let nanoseconds = i128::from(value)
            .checked_mul(unit_nanoseconds)
            .unwrap_or_else(|| runtime_error("duration overflow"));
        boxed_value(Value::Duration(nanoseconds))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_duration_to_float(
    value: *mut OpaqueValue,
    unit_nanoseconds: i64,
) -> f64 {
    task_runtime_boundary(|| {
        let Value::Duration(nanoseconds) = (unsafe { value_ref(value) }) else {
            runtime_error(format!(
                "expected `Duration`, found `{}`",
                value_type_name(unsafe { value_ref(value) })
            ));
        };
        match i128::from(unit_nanoseconds) {
            crate::runtime_value::NANOS_PER_MILLISECOND => {
                crate::runtime_value::duration_to_milliseconds(nanoseconds)
            }
            crate::runtime_value::NANOS_PER_SECOND => {
                crate::runtime_value::duration_to_seconds(nanoseconds)
            }
            other => runtime_error(format!("unknown Duration conversion unit `{other}`")),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_len(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::String(text) => match i64::try_from(text.chars().count()) {
            Ok(length) => length,
            Err(_) => runtime_error("string length does not fit in the direct runtime range"),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_byte_len(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::String(text) => match i64::try_from(text.len()) {
            Ok(length) => length,
            Err(_) => runtime_error("string byte length does not fit in the direct runtime range"),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_slice(
    value: *mut OpaqueValue,
    start: i64,
    has_start: i64,
    end: i64,
    has_end: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result = unsafe {
            with_value(value, |value| match value {
                Value::String(text) => slice_string_owned(
                    text,
                    (has_start != 0).then_some(i128::from(start)),
                    (has_end != 0).then_some(i128::from(end)),
                ),
                other => runtime_error(format!(
                    "expected `str`, found `{}`",
                    value_type_name(other)
                )),
            })
        };
        match result {
            Ok(slice) => boxed_value(Value::String(slice)),
            Err(mut error) => {
                error.span = runtime_span(line, column);
                runtime_diagnostic_error(error)
            }
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_contains(
    value: *mut OpaqueValue,
    needle: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let Value::String(needle) = (unsafe { take_value(needle) }) else {
            runtime_error("`contains` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => i64::from(text.contains(&needle)),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_starts_with(
    value: *mut OpaqueValue,
    prefix: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let Value::String(prefix) = (unsafe { take_value(prefix) }) else {
            runtime_error("`starts_with` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => i64::from(text.starts_with(&prefix)),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_ends_with(
    value: *mut OpaqueValue,
    suffix: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let Value::String(suffix) = (unsafe { take_value(suffix) }) else {
            runtime_error("`ends_with` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => i64::from(text.ends_with(&suffix)),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_split(
    value: *mut OpaqueValue,
    separator: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let Value::String(separator) = (unsafe { take_value(separator) }) else {
            runtime_error("`split` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => boxed_value(Value::Vec(VecValue {
                element_type: Type::named("str"),
                elements: text
                    .split(&separator)
                    .map(|part| Value::String(part.to_string()))
                    .collect(),
            })),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_replace(
    value: *mut OpaqueValue,
    from: *mut OpaqueValue,
    to: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let Value::String(from) = (unsafe { take_value(from) }) else {
            runtime_error("`replace` requires `str` for `from`");
        };
        let Value::String(to) = (unsafe { take_value(to) }) else {
            runtime_error("`replace` requires `str` for `to`");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => boxed_value(Value::String(text.replace(&from, &to))),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_to_lower(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::String(text) => boxed_value(Value::String(text.to_lowercase())),
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_to_upper(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::String(text) => boxed_value(Value::String(text.to_uppercase())),
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_strip_prefix(
    value: *mut OpaqueValue,
    prefix: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let Value::String(prefix) = (unsafe { take_value(prefix) }) else {
            runtime_error("`strip_prefix` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => boxed_value(
                text.strip_prefix(&prefix)
                    .map(|rest| option_some(Value::String(rest.to_string())))
                    .unwrap_or_else(option_none),
            ),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_strip_suffix(
    value: *mut OpaqueValue,
    suffix: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let Value::String(suffix) = (unsafe { take_value(suffix) }) else {
            runtime_error("`strip_suffix` requires a `str` argument");
        };
        match unsafe { value_ref(value) } {
            Value::String(text) => boxed_value(
                text.strip_suffix(&suffix)
                    .map(|rest| option_some(Value::String(rest.to_string())))
                    .unwrap_or_else(option_none),
            ),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_trim(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::String(text) => boxed_value(Value::String(text.trim().to_string())),
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_string_join(
    separator: *mut OpaqueValue,
    parts: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let Value::Vec(parts) = (unsafe { take_value(parts) }) else {
            runtime_error("`join` requires `list[str]`");
        };
        match unsafe { value_ref(separator) } {
            Value::String(separator) => {
                let mut rendered_parts = Vec::new();
                for value in parts.elements {
                    let Value::String(part) = value else {
                        runtime_error("`join` requires `list[str]`");
                    };
                    rendered_parts.push(part);
                }
                boxed_value(Value::String(rendered_parts.join(&separator)))
            }
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_abs(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        match value {
            Value::Int(value) => match value.representation() {
                IntegerRepresentation::Signed(signed) if signed < 0 => {
                    if signed == i128::MIN {
                        runtime_error("`abs(...)` overflowed the signed integer range");
                    }
                    boxed_value(Value::Int(
                        value
                            .checked_neg()
                            .expect("the int128 minimum was rejected before negation"),
                    ))
                }
                IntegerRepresentation::Signed(_) | IntegerRepresentation::Unsigned(_) => {
                    boxed_value(Value::Int(value))
                }
            },
            Value::Float(value) => boxed_value(Value::Float(value.abs())),
            other => runtime_error(format!(
                "`abs(...)` expects an integer or float value, found `{}`",
                value_type_name(&other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_min(
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let left = unsafe { take_value(left) };
        let right = unsafe { take_value(right) };
        let value = match (&left, &right) {
            (Value::Int(left_value), Value::Int(right_value)) => {
                if left_value <= right_value {
                    left
                } else {
                    right
                }
            }
            (Value::Float(left_value), Value::Float(right_value)) => {
                if left_value <= right_value {
                    left
                } else {
                    right
                }
            }
            _ => runtime_error("`min(...)` expects matching numeric arguments"),
        };
        boxed_value(value)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_max(
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let left = unsafe { take_value(left) };
        let right = unsafe { take_value(right) };
        let value = match (&left, &right) {
            (Value::Int(left_value), Value::Int(right_value)) => {
                if left_value >= right_value {
                    left
                } else {
                    right
                }
            }
            (Value::Float(left_value), Value::Float(right_value)) => {
                if left_value >= right_value {
                    left
                } else {
                    right
                }
            }
            _ => runtime_error("`max(...)` expects matching numeric arguments"),
        };
        boxed_value(value)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_sqrt(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        match value {
            Value::Float(value) => boxed_value(Value::Float(value.sqrt())),
            other => runtime_error(format!(
                "`sqrt(...)` expects `float32` or `float64`, found `{}`",
                value_type_name(&other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_round(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        let rounded =
            round_numeric_value(&value).unwrap_or_else(|error| runtime_diagnostic_error(error));
        boxed_value(rounded)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_divmod(
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let explicit_type =
            unsafe { effective_runtime_type_name(left) }.map(|name| runtime_type_from_name(&name));
        let left = unsafe { take_value(left) };
        let right = unsafe { take_value(right) };
        let operand_type = explicit_type.unwrap_or_else(|| inferred_collection_type(&left));
        let pair = divmod_numeric_values(&left, &right, &operand_type)
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        boxed_value(pair)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_parse_int32(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        match value {
            Value::String(text) => match text.parse::<i32>() {
                Ok(value) => boxed_value(result_ok(Value::Int(IntegerValue::from_signed(
                    value as i128,
                )))),
                Err(error) => boxed_value(result_err(Value::String(error.to_string()))),
            },
            other => runtime_error(format!(
                "`parse_int32(...)` expects `str`, found `{}`",
                value_type_name(&other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_parse_int64(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        match value {
            Value::String(text) => match text.parse::<i64>() {
                Ok(value) => boxed_value(result_ok(Value::Int(IntegerValue::from_signed(
                    value as i128,
                )))),
                Err(error) => boxed_value(result_err(Value::String(error.to_string()))),
            },
            other => runtime_error(format!(
                "`parse_int64(...)` expects `str`, found `{}`",
                value_type_name(&other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_parse_float64(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        match value {
            Value::String(text) => match text.parse::<f64>() {
                Ok(value) if value.is_finite() => boxed_value(result_ok(Value::Float(value))),
                Ok(_) => boxed_value(result_err(Value::String(
                    "float must be finite".to_string(),
                ))),
                Err(error) => boxed_value(result_err(Value::String(error.to_string()))),
            },
            other => runtime_error(format!(
                "`parse_float64(...)` expects `str`, found `{}`",
                value_type_name(&other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_range_new(start: i64, end: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::Range(RangeValue {
            start: start as i128,
            end: end as i128,
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_range_current(range: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(range) } {
        Value::Range(range) => match i64::try_from(range.start) {
            Ok(start) => start,
            Err(_) => runtime_error("range start is outside host i64 bounds"),
        },
        other => runtime_error(format!(
            "expected `Range`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_range_end(range: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(range) } {
        Value::Range(range) => match i64::try_from(range.end) {
            Ok(end) => end,
            Err(_) => runtime_error("range end is outside host i64 bounds"),
        },
        other => runtime_error(format!(
            "expected `Range`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_range_advance(range: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(range) } {
        Value::Range(range) => boxed_value(Value::Range(RangeValue {
            start: range.start + 1,
            end: range.end,
        })),
        other => runtime_error(format!(
            "expected `Range`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_rng_new(seed: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::Rng(RngValue::from_seed(seed))))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_rng_next_int(rng: *mut OpaqueValue, lo: i64, hi: i64) -> i64 {
    task_runtime_boundary(|| {
        let rng = match unsafe { value_ref(rng) } {
            Value::Rng(rng) => rng,
            other => runtime_error(format!(
                "expected `random.Rng`, found `{}`",
                value_type_name(other)
            )),
        };
        rng.next_int(lo, hi)
            .unwrap_or_else(|_| direct_invalid_random_bounds(lo, hi))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_rng_next_float(rng: *mut OpaqueValue) -> f64 {
    task_runtime_boundary(|| match unsafe { value_ref(rng) } {
        Value::Rng(rng) => rng.next_float(),
        other => runtime_error(format!(
            "expected `random.Rng`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_rng_shuffle(rng: *mut OpaqueValue, values: *mut OpaqueValue) {
    task_runtime_boundary(|| {
        let rng = match unsafe { value_ref(rng) } {
            Value::Rng(rng) => rng,
            other => runtime_error(format!(
                "expected `random.Rng`, found `{}`",
                value_type_name(other)
            )),
        };
        with_vector_mut(values, |vector| rng.shuffle(&mut vector.elements));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_random_secure_int(lo: i64, hi: i64) -> i64 {
    task_runtime_boundary(|| match randomness::secure_int(lo, hi) {
        Ok(value) => value,
        Err(error) => direct_random_resource_error(error, Some((lo, hi))),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_random_secure_bytes(count: i64) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        if count < 0 {
            runtime_diagnostic_error(Diagnostic::coded(
                "AU4003",
                format!(
                    "`random.secure_bytes(n)` requires a non-negative byte count, found `{count}`"
                ),
            ));
        }
        // Every supported Aura release target is 64-bit, so every
        // non-negative int64 count is representable as usize. Allocation
        // failure remains reported by secure_bytes itself.
        let count = count as usize;
        match randomness::secure_bytes(count) {
            Ok(bytes) => boxed_value(bytes_vec_value(bytes)),
            Err(error) => direct_random_resource_error(error, None),
        }
    })
}

fn direct_invalid_random_bounds(lo: i64, hi: i64) -> ! {
    runtime_diagnostic_error(Diagnostic::coded(
        "AU4003",
        format!("random bounds require `lo < hi`, found `{lo} >= {hi}`"),
    ))
}

fn direct_random_resource_error(error: SecureRandomError, bounds: Option<(i64, i64)>) -> ! {
    match error {
        SecureRandomError::InvalidRange => match bounds {
            Some((lo, hi)) => direct_invalid_random_bounds(lo, hi),
            None => runtime_diagnostic_error(Diagnostic::coded(
                "AU4003",
                "random bounds require `lo < hi`",
            )),
        },
        error @ SecureRandomError::RequestExceedsCeiling { .. } => {
            runtime_diagnostic_error(Diagnostic::coded("AU4005", error.to_string()))
        }
        SecureRandomError::Allocation(error) => runtime_diagnostic_error(Diagnostic::coded(
            "AU4005",
            format!("secure random allocation failed: {error}"),
        )),
        SecureRandomError::Entropy(error) => runtime_diagnostic_error(Diagnostic::coded(
            "AU4005",
            format!("operating-system random source failed: {error}"),
        )),
    }
}

fn with_vector<T>(ptr: *mut OpaqueValue, read: impl FnOnce(&VecValue) -> T) -> T {
    unsafe {
        with_value(ptr, |value| match value {
            Value::Vec(vector) => read(vector),
            other => runtime_error(format!(
                "expected `list`, found `{}`",
                value_type_name(other)
            )),
        })
    }
}

fn with_vector_mut<T>(ptr: *mut OpaqueValue, write: impl FnOnce(&mut VecValue) -> T) -> T {
    let result = unsafe {
        value_mut(ptr, |value| match value {
            Value::Vec(vector) => Ok(write(vector)),
            other => Err(value_type_name(other)),
        })
    };
    match result {
        Ok(value) => value,
        Err(found) => runtime_error(format!("expected `list`, found `{found}`")),
    }
}

fn with_map<T>(ptr: *mut OpaqueValue, read: impl FnOnce(&MapValue) -> T) -> T {
    unsafe {
        with_value(ptr, |value| match value {
            Value::Map(map) => read(map),
            other => runtime_error(format!(
                "expected `dict`, found `{}`",
                value_type_name(other)
            )),
        })
    }
}

fn with_map_mut<T>(ptr: *mut OpaqueValue, write: impl FnOnce(&mut MapValue) -> T) -> T {
    let result = unsafe {
        value_mut(ptr, |value| match value {
            Value::Map(map) => Ok(write(map)),
            other => Err(value_type_name(other)),
        })
    };
    match result {
        Ok(value) => value,
        Err(found) => runtime_error(format!("expected `dict`, found `{found}`")),
    }
}

fn with_set<T>(ptr: *mut OpaqueValue, read: impl FnOnce(&SetValue) -> T) -> T {
    unsafe {
        with_value(ptr, |value| match value {
            Value::Set(set) => read(set),
            other => runtime_error(format!(
                "expected `set`, found `{}`",
                value_type_name(other)
            )),
        })
    }
}

fn with_set_mut<T>(ptr: *mut OpaqueValue, write: impl FnOnce(&mut SetValue) -> T) -> T {
    let result = unsafe {
        value_mut(ptr, |value| match value {
            Value::Set(set) => Ok(write(set)),
            other => Err(value_type_name(other)),
        })
    };
    match result {
        Ok(value) => value,
        Err(found) => runtime_error(format!("expected `set`, found `{found}`")),
    }
}

fn normalize_vec_index(index: i64, len: usize) -> Option<usize> {
    // Rust's supported pointer widths fit losslessly in i128, so this conversion
    // has no runtime failure case to defend or cover.
    let len = len as i128;
    let normalized = if index < 0 {
        len + i128::from(index)
    } else {
        i128::from(index)
    };
    usize::try_from(normalized).ok()
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_empty() -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::Vec(VecValue {
            element_type: Type::named("Unknown"),
            elements: Vec::new(),
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_len(vec: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| {
        match i64::try_from(with_vector(vec, |vector| vector.elements.len())) {
            Ok(length) => length,
            Err(_) => runtime_error("list length does not fit in the direct runtime range"),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_is_empty(vec: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| i64::from(with_vector(vec, |vector| vector.elements.is_empty())))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_slice(
    vec: *mut OpaqueValue,
    start: i64,
    has_start: i64,
    end: i64,
    has_end: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result = with_vector(vec, |vector| {
            slice_vec_owned(
                vector,
                (has_start != 0).then_some(i128::from(start)),
                (has_end != 0).then_some(i128::from(end)),
            )
        });
        match result {
            Ok(slice) => boxed_value(Value::Vec(slice)),
            Err(mut error) => {
                error.span = runtime_span(line, column);
                runtime_diagnostic_error(error)
            }
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_push_in_place(
    vec: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        let inferred = inferred_collection_type(&value);
        with_vector_mut(vec, |vector| {
            if vector.element_type == Type::named("Unknown") && inferred != Type::named("Unknown") {
                vector.element_type = inferred;
            }
            vector.elements.push(value);
        });
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_pop_in_place(vec: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = with_vector_mut(vec, |vector| vector.elements.pop());
        match value {
            Some(value) => boxed_value(option_some(value)),
            None => boxed_value(option_none()),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_get(
    vec: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = with_vector(vec, |vector| {
            normalize_vec_index(index, vector.elements.len())
                .and_then(|index| vector.elements.get(index))
                .map(try_clone_array_containing_value)
                .transpose()
        });
        let value = direct_array_result(value, 0, 0);
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_set_in_place(
    vec: *mut OpaqueValue,
    index: i64,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let mut replacement = Some(unsafe { consume_owned_value(value) });
        let (previous, len) = with_vector_mut(vec, |vector| {
            let len = vector.elements.len();
            let previous = normalize_vec_index(index, len)
                .filter(|normalized| *normalized < len)
                .map(|normalized| {
                    std::mem::replace(
                        &mut vector.elements[normalized],
                        replacement
                            .take()
                            .expect("the replacement is consumed once"),
                    )
                });
            (previous, len)
        });
        let previous = previous.unwrap_or_else(|| {
            runtime_error(format!(
                "list set index `{index}` is out of bounds for length `{len}`"
            ))
        });
        boxed_value(previous)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_remove_in_place(
    vec: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let previous = with_vector_mut(vec, |vector| {
            let Some(normalized) = normalize_vec_index(index, vector.elements.len())
                .filter(|normalized| *normalized < vector.elements.len())
            else {
                return Err(vector.elements.len());
            };
            Ok(vector.elements.remove(normalized))
        });
        let previous = previous.unwrap_or_else(|len| {
            runtime_error(format!(
                "list remove index `{index}` is out of bounds for length `{len}`"
            ))
        });
        boxed_value(option_some(previous))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_swap_in_place(
    vec: *mut OpaqueValue,
    first: i64,
    second: i64,
) -> i64 {
    task_runtime_boundary(|| {
        let result = with_vector_mut(vec, |vector| {
            let normalized_first = normalize_vec_index(first, vector.elements.len());
            let normalized_second = normalize_vec_index(second, vector.elements.len());
            let (Some(normalized_first), Some(normalized_second)) = (
                normalized_first.filter(|index| *index < vector.elements.len()),
                normalized_second.filter(|index| *index < vector.elements.len()),
            ) else {
                return Err(vector.elements.len());
            };
            vector.elements.swap(normalized_first, normalized_second);
            Ok(())
        });
        result.unwrap_or_else(|len| {
            runtime_error(format!(
                "list swap indices `{first}` and `{second}` are out of bounds for length `{len}`"
            ))
        });
        1
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_contains(
    vec: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let needle = unsafe { take_value(value) };
        i64::from(with_vector(vec, |vector| vector.elements.contains(&needle)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_insert_in_place(
    vec: *mut OpaqueValue,
    index: i64,
    value: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        with_vector_mut(vec, |vector| {
            let len = vector.elements.len();
            let normalized = if index < 0 {
                usize::try_from((len as i128 + i128::from(index)).max(0)).unwrap_or(0)
            } else {
                usize::try_from(index).unwrap_or(usize::MAX).min(len)
            };
            vector.elements.insert(normalized, value);
        });
        1
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_clear_in_place(vec: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        with_vector_mut(vec, |vector| vector.elements.clear());
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_reverse_in_place(
    vec: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        with_vector_mut(vec, |vector| vector.elements.reverse());
        boxed_value(Value::Unit)
    })
}

/// Shared direct-runtime implementation for canonical collection operations
/// whose result shape is naturally represented as an owned Aura value.
/// `arg` is an opaque value for value-search operations, while `scalar` is
/// used for positions and capacity requests.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_collection_operation(
    collection: *mut OpaqueValue,
    arg: *mut OpaqueValue,
    scalar: i64,
    opcode: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match opcode {
        0 => {
            let value = with_vector_mut(collection, |vector| {
                let Some(index) = normalize_vec_index(scalar, vector.elements.len())
                    .filter(|index| *index < vector.elements.len())
                else {
                    return Err(vector.elements.len());
                };
                Ok(vector.elements.remove(index))
            });
            let value = value.unwrap_or_else(|len| {
                runtime_diagnostic_error(Diagnostic::coded(
                    "AU4003",
                    format!("list pop index `{scalar}` is out of bounds for length `{len}`"),
                ))
            });
            boxed_value(value)
        }
        1..=3 => {
            let needle = unsafe { value_ref(arg) };
            if opcode == 3 {
                let count = with_vector(collection, |vector| {
                    vector
                        .elements
                        .iter()
                        .filter(|candidate| **candidate == needle)
                        .count()
                });
                return boxed_value(Value::Int(IntegerValue::from_literal(count as u128)));
            }
            let index = with_vector(collection, |vector| {
                vector
                    .elements
                    .iter()
                    .position(|candidate| *candidate == needle)
            });
            let Some(index) = index else {
                runtime_diagnostic_error(
                    Diagnostic::coded("AU4008", "collection value was not found").with_help(
                        if opcode == 1 {
                            "check `value in values` before removing when absence is expected"
                        } else {
                            "check `value in values` before searching when absence is expected"
                        },
                    ),
                );
            };
            if opcode == 1 {
                with_vector_mut(collection, |vector| {
                    vector.elements.remove(index);
                });
                boxed_value(Value::Unit)
            } else {
                boxed_value(Value::Int(IntegerValue::from_literal(index as u128)))
            }
        }
        4 => {
            enum ReserveFailure {
                NotCollection,
                Allocation,
            }

            let additional = usize::try_from(scalar).unwrap_or_else(|_| {
                runtime_diagnostic_error(Diagnostic::coded(
                    "AU4003",
                    "collection capacity cannot be negative",
                ))
            });
            let result = unsafe {
                value_mut(collection, |value| match value {
                    Value::Vec(vector) => vector
                        .elements
                        .try_reserve(additional)
                        .map_err(|_| ReserveFailure::Allocation),
                    Value::Map(map) => map
                        .entries
                        .try_reserve(additional)
                        .map_err(|_| ReserveFailure::Allocation),
                    Value::Set(set) => set
                        .elements
                        .try_reserve(additional)
                        .map_err(|_| ReserveFailure::Allocation),
                    _ => Err(ReserveFailure::NotCollection),
                })
            };
            match result {
                Ok(()) => {}
                Err(ReserveFailure::NotCollection) => {
                    runtime_error("reserve requires a collection")
                }
                Err(ReserveFailure::Allocation) => runtime_diagnostic_error(Diagnostic::coded(
                    "AU4005",
                    "collection capacity allocation failed",
                )),
            }
            boxed_value(Value::Unit)
        }
        5 | 6 => {
            let needle = unsafe { value_ref(arg) };
            let removed = with_set_mut(collection, |set| {
                set.elements
                    .iter()
                    .position(|candidate| *candidate == needle)
                    .map(|index| set.elements.remove(index))
                    .is_some()
            });
            if opcode == 5 && !removed {
                runtime_diagnostic_error(
                    Diagnostic::coded("AU4008", "collection value was not found").with_help(
                        "check `value in values` before removing when absence is expected",
                    ),
                );
            }
            boxed_value(Value::Unit)
        }
        7 => {
            with_set_mut(collection, |set| set.elements.clear());
            boxed_value(Value::Unit)
        }
        _ => runtime_error("unknown direct collection operation"),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_extend_in_place(
    vec: *mut OpaqueValue,
    other: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let other = unsafe { consume_owned_value(other) };
        let Value::Vec(other) = other else {
            runtime_error("`extend` requires another `list[T]` value");
        };
        with_vector_mut(vec, |vector| vector.elements.extend(other.elements));
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_index(
    vec: *mut OpaqueValue,
    index: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let (value, len) = with_vector(vec, |vector| {
            (
                normalize_vec_index(index, vector.elements.len())
                    .and_then(|normalized| vector.elements.get(normalized))
                    .map(try_clone_array_containing_value),
                vector.elements.len(),
            )
        });
        let Some(value) = value else {
            match runtime_span(line, column) {
                Some(span) => runtime_error_at(
                    span,
                    format!(
                        "list index `{}` is out of bounds for length `{}`",
                        index, len
                    ),
                ),
                None => runtime_error(format!(
                    "list index `{}` is out of bounds for length `{}`",
                    index, len
                )),
            }
        };
        boxed_value(direct_array_result(value, line, column))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_index_option(
    vec: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = with_vector(vec, |vector| {
            normalize_vec_index(index, vector.elements.len())
                .and_then(|normalized| vector.elements.get(normalized))
                .map(try_clone_array_containing_value)
                .transpose()
        });
        let value = direct_array_result(value, 0, 0);
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_take_index_in_place(
    vec: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = with_vector_mut(vec, |vector| {
            normalize_vec_index(index, vector.elements.len())
                .filter(|normalized| *normalized < vector.elements.len())
                .map(|normalized| vector.elements.remove(normalized))
        });
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_vec_set_index_in_place(
    vec: *mut OpaqueValue,
    index: i64,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        let result = with_vector_mut(vec, |vector| {
            let normalized = match normalize_vec_index(index, vector.elements.len())
                .filter(|normalized| *normalized < vector.elements.len())
            {
                Some(normalized) => normalized,
                None => return Err(vector.elements.len()),
            };
            vector.elements[normalized] = value;
            Ok(())
        });
        if let Err(len) = result {
            match runtime_span(line, column) {
                Some(span) => runtime_error_at(
                    span,
                    format!(
                        "list index `{}` is out of bounds for length `{}`",
                        index, len
                    ),
                ),
                None => runtime_error(format!(
                    "list index `{}` is out of bounds for length `{}`",
                    index, len
                )),
            }
        }
        boxed_value(Value::Unit)
    })
}

fn with_array<T>(ptr: *mut OpaqueValue, read: impl FnOnce(&ArrayValue) -> T) -> T {
    unsafe {
        with_value(ptr, |value| match value {
            Value::Array(array) => read(array),
            other => runtime_error(format!(
                "expected `Array`, found `{}`",
                value_type_name(other)
            )),
        })
    }
}

fn with_array_mut<T>(ptr: *mut OpaqueValue, write: impl FnOnce(&mut ArrayValue) -> T) -> T {
    unsafe {
        value_mut(ptr, |value| match value {
            Value::Array(array) => write(array),
            other => runtime_error(format!(
                "expected `Array`, found `{}`",
                value_type_name(other)
            )),
        })
    }
}

fn direct_array_result<T>(result: std::result::Result<T, Diagnostic>, line: i64, column: i64) -> T {
    result.unwrap_or_else(|mut error| {
        if error.span.is_none() {
            error.span = runtime_span(line, column);
        }
        runtime_diagnostic_error(error)
    })
}

fn direct_array_dtype(code: i64) -> std::result::Result<ArrayDType, Diagnostic> {
    ArrayDType::from_code(code).ok_or_else(|| {
        Diagnostic::coded(
            "AU4001",
            format!("direct Array ABI received invalid dtype code `{code}`"),
        )
    })
}

fn direct_array_operation(code: i64) -> std::result::Result<ArrayBinaryOp, Diagnostic> {
    ArrayBinaryOp::from_code(code).ok_or_else(|| {
        Diagnostic::coded(
            "AU4001",
            format!("direct Array ABI received invalid binary operation code `{code}`"),
        )
    })
}

fn direct_array_arithmetic_mode(
    code: i64,
) -> std::result::Result<IntegerArithmeticMode, Diagnostic> {
    IntegerArithmeticMode::from_code(code).ok_or_else(|| {
        Diagnostic::coded(
            "AU4001",
            format!("direct Array ABI received invalid arithmetic mode code `{code}`"),
        )
    })
}

fn direct_array_reduction(code: i64) -> std::result::Result<ArrayReduction, Diagnostic> {
    ArrayReduction::from_code(code).ok_or_else(|| {
        Diagnostic::coded(
            "AU4001",
            format!("direct Array ABI received invalid reduction code `{code}`"),
        )
    })
}

fn direct_array_shape(shape: *mut OpaqueValue) -> std::result::Result<Box<[usize]>, Diagnostic> {
    with_vector(shape, |shape| {
        if shape.element_type != Type::named("int64") {
            return Err(Diagnostic::coded(
                "AU4007",
                format!(
                    "array shape requires `list[int64]`, found `list[{}]`",
                    shape.element_type
                ),
            ));
        }
        let mut dimensions = try_array_buffer(shape.elements.len(), "Array shape")?;
        for (axis, dimension) in shape.elements.iter().enumerate() {
            let Value::Int(dimension) = dimension else {
                return Err(Diagnostic::coded(
                    "AU4007",
                    format!("array shape axis {axis} is not an int64 value"),
                ));
            };
            if dimension.runtime_kind() != Some(IntegerKind::Int64) {
                return Err(Diagnostic::coded(
                    "AU4007",
                    format!("array shape axis {axis} is not an int64 value"),
                ));
            }
            // `IntegerValue::with_runtime_kind` only installs `int64` after
            // validating the signed bounds, so an int64 runtime value always
            // has an exact i128 representation.
            let dimension = match dimension.representation() {
                IntegerRepresentation::Signed(value) => value,
                IntegerRepresentation::Unsigned(value) => value as i128,
            };
            dimensions.push(usize::try_from(dimension).map_err(|_| {
                Diagnostic::coded(
                    "AU4007",
                    format!("Array shape axis {axis} cannot be negative, found {dimension}"),
                )
            })?);
        }
        Ok(dimensions.into_boxed_slice())
    })
}

fn direct_array_coordinates(
    coordinates: *mut OpaqueValue,
) -> std::result::Result<Box<[i64]>, Diagnostic> {
    unsafe {
        with_value(coordinates, |coordinates| {
            let (element_types_are_int64, elements): (bool, &[Value]) = match coordinates {
                Value::Int(coordinate) => {
                    if coordinate.runtime_kind() != Some(IntegerKind::Int64) {
                        return Err(Diagnostic::coded(
                            "AU4007",
                            "array coordinates require int64 values",
                        ));
                    }
                    let value = match coordinate.representation() {
                        IntegerRepresentation::Signed(value) => value as i64,
                        IntegerRepresentation::Unsigned(value) => value as i64,
                    };
                    return Ok(vec![value].into_boxed_slice());
                }
                Value::Vec(coordinates) => (
                    coordinates.element_type == Type::named("int64"),
                    &coordinates.elements,
                ),
                Value::Tuple(coordinates) => (
                    coordinates
                        .element_types
                        .iter()
                        .all(|ty| *ty == Type::named("int64")),
                    &coordinates.elements,
                ),
                other => {
                    return Err(Diagnostic::coded(
                        "AU4007",
                        format!(
                            "array coordinates require `list[int64]` or an int64 tuple, found `{}`",
                            value_type_name(other)
                        ),
                    ))
                }
            };
            if !element_types_are_int64 {
                return Err(Diagnostic::coded(
                    "AU4007",
                    "array coordinates require int64 values",
                ));
            }
            elements
                .iter()
                .enumerate()
                .map(|(axis, coordinate)| {
                    let Value::Int(coordinate) = coordinate else {
                        return Err(Diagnostic::coded(
                            "AU4007",
                            format!("array coordinate on axis {axis} is not an int64 value"),
                        ));
                    };
                    if coordinate.runtime_kind() != Some(IntegerKind::Int64) {
                        return Err(Diagnostic::coded(
                            "AU4007",
                            format!("array coordinate on axis {axis} is not an int64 value"),
                        ));
                    }
                    Ok(match coordinate.representation() {
                        IntegerRepresentation::Signed(value) => value as i64,
                        IntegerRepresentation::Unsigned(value) => value as i64,
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map(Vec::into_boxed_slice)
        })
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_zeros(
    dtype: i64,
    shape: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let dtype = direct_array_result(direct_array_dtype(dtype), line, column);
        let shape = direct_array_result(direct_array_shape(shape), line, column);
        let array = direct_array_result(ArrayValue::zeros(dtype, shape), line, column);
        boxed_value(Value::Array(array))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_full(
    dtype: i64,
    shape: *mut OpaqueValue,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let dtype = direct_array_result(direct_array_dtype(dtype), line, column);
        let shape = direct_array_result(direct_array_shape(shape), line, column);
        let array = unsafe {
            with_value(value, |value| {
                direct_array_result(ArrayValue::full(dtype, shape, value), line, column)
            })
        };
        boxed_value(Value::Array(array))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_from_vec(
    dtype: i64,
    values: *mut OpaqueValue,
    shape: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let dtype = direct_array_result(direct_array_dtype(dtype), line, column);
        let shape = direct_array_result(direct_array_shape(shape), line, column);
        let array = with_vector(values, |values| {
            if values.element_type != Type::named(dtype.runtime_type_name()) {
                return Err(Diagnostic::coded(
                    "AU4007",
                    format!(
                        "Array[{}].from_list requires `list[{}]`, found `list[{}]`",
                        dtype.runtime_type_name(),
                        dtype.runtime_type_name(),
                        values.element_type
                    ),
                ));
            }
            ArrayValue::from_vec(values, Some(&shape))
        });
        boxed_value(Value::Array(direct_array_result(array, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_clone(
    array: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let cloned = with_array(array, ArrayValue::try_clone);
        boxed_value(Value::Array(direct_array_result(cloned, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_shape(array: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let shape = with_array(array, ArrayValue::shape_value);
        boxed_value(direct_array_result(shape, 0, 0))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_len(array: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| {
        i64::try_from(with_array(array, ArrayValue::len))
            .unwrap_or_else(|_| runtime_error("array length does not fit in int64"))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_get(
    array: *mut OpaqueValue,
    coordinates: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let coordinates = direct_array_result(direct_array_coordinates(coordinates), line, column);
        let value = with_array(array, |array| array.get_optional(&coordinates));
        match direct_array_result(value, line, column) {
            Some(value) => boxed_value(option_some(value)),
            None => boxed_value(option_none()),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_set_in_place(
    array: *mut OpaqueValue,
    coordinates: *mut OpaqueValue,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let coordinates = direct_array_result(direct_array_coordinates(coordinates), line, column);
        let value = unsafe { value_ref(value) };
        let previous = with_array_mut(array, |array| array.set(&coordinates, value));
        boxed_value(option_some(direct_array_result(previous, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_fill_in_place(
    array: *mut OpaqueValue,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { value_ref(value) };
        let result = with_array_mut(array, |array| array.fill(value));
        direct_array_result(result, line, column);
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_index(
    array: *mut OpaqueValue,
    coordinates: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let coordinates = direct_array_result(direct_array_coordinates(coordinates), line, column);
        let value = with_array(array, |array| array.get(&coordinates));
        boxed_value(direct_array_result(value, line, column))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_set_index_in_place(
    array: *mut OpaqueValue,
    coordinates: *mut OpaqueValue,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let coordinates = direct_array_result(direct_array_coordinates(coordinates), line, column);
        let value = unsafe { value_ref(value) };
        let result = with_array_mut(array, |array| array.set(&coordinates, value));
        let _ = direct_array_result(result, line, column);
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_slice(
    array: *mut OpaqueValue,
    start: i64,
    has_start: i64,
    end: i64,
    has_end: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let slice = with_array(array, |array| {
            array.slice_first_axis(
                (has_start != 0).then_some(i128::from(start)),
                (has_end != 0).then_some(i128::from(end)),
            )
        });
        boxed_value(Value::Array(direct_array_result(slice, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_binary(
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
    scalar_left: i64,
    operation: i64,
    arithmetic_mode: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let operation = direct_array_result(direct_array_operation(operation), line, column);
        let arithmetic_mode =
            direct_array_result(direct_array_arithmetic_mode(arithmetic_mode), line, column);
        let evaluate = |left_value: &Value, right_value: &Value| {
            match (left_value, right_value) {
                (Value::Array(left), Value::Array(right)) if scalar_left == 0 => {
                    left.binary(right, operation, arithmetic_mode)
                }
                (Value::Array(array), scalar) if scalar_left == 0 => {
                    array.scalar_binary(scalar, false, operation, arithmetic_mode)
                }
                (scalar, Value::Array(array)) if scalar_left != 0 => {
                    array.scalar_binary(scalar, true, operation, arithmetic_mode)
                }
                (left, right) => Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "direct Array ABI received inconsistent operands `{}` and `{}` with scalar-left flag `{scalar_left}`",
                        value_type_name(left),
                        value_type_name(right)
                    ),
                )),
            }
        };
        let result = unsafe {
            if left == right {
                with_value(left, |value| evaluate(value, value))
            } else {
                with_value(left, |left_value| {
                    with_value(right, |right_value| evaluate(left_value, right_value))
                })
            }
        };
        boxed_value(Value::Array(direct_array_result(result, line, column)))
    })
}

fn direct_array_map_result_buffer(len: usize, line: i64, column: i64) -> Vec<Value> {
    direct_array_result(try_array_buffer(len, "Array.map result"), line, column)
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_map(
    array: *mut OpaqueValue,
    function: *mut OpaqueValue,
    result_dtype: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result_dtype = direct_array_result(direct_array_dtype(result_dtype), line, column);
        let (shape, source_len) = with_array(array, |array| {
            let mut shape = direct_array_result(
                try_array_buffer(array.shape.len(), "Array.map shape"),
                line,
                column,
            );
            shape.extend_from_slice(&array.shape);
            (shape.into_boxed_slice(), array.len())
        });
        let mut mapped = direct_array_map_result_buffer(source_len, line, column);
        for index in 0..source_len {
            let source_value = with_array(array, |array| array.value_at_flat(index));
            let mut arguments = [boxed_value(source_value) as i64];
            let result = aura_direct_function_call(function, arguments.as_mut_ptr(), 1);
            mapped.push(unsafe { consume_owned_value(result) });
        }
        let array = ArrayValue::from_values(
            &Type::named(result_dtype.runtime_type_name()),
            shape,
            mapped,
        );
        boxed_value(Value::Array(direct_array_result(array, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_array_reduce(
    array: *mut OpaqueValue,
    reduction: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let reduction = direct_array_result(direct_array_reduction(reduction), line, column);
        let result = with_array(array, |array| array.reduce(reduction));
        boxed_value(direct_array_result(result, line, column))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_empty() -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::Map(MapValue {
            key_type: Type::named("Unknown"),
            value_type: Type::named("Unknown"),
            entries: Vec::new(),
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_len(map: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(
        || match i64::try_from(with_map(map, |map| map.entries.len())) {
            Ok(length) => length,
            Err(_) => runtime_error("dict length does not fit in the direct runtime range"),
        },
    )
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_is_empty(map: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| i64::from(with_map(map, |map| map.entries.is_empty())))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_get(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = unsafe { take_value(key) };
        let value = with_map(map, |map| {
            map.entries
                .iter()
                .find(|(candidate_key, _)| *candidate_key == key)
                .map(|(_, value)| try_clone_array_containing_value(value))
                .transpose()
        });
        let value = direct_array_result(value, 0, 0);
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_set_in_place(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = unsafe { consume_owned_value(key) };
        let value = unsafe { consume_owned_value(value) };
        let inferred_key_type = inferred_collection_type(&key);
        let inferred_value_type = inferred_collection_type(&value);
        let previous = with_map_mut(map, |map| {
            if map.key_type == Type::named("Unknown") && inferred_key_type != Type::named("Unknown")
            {
                map.key_type = inferred_key_type.clone();
            }
            if map.value_type == Type::named("Unknown")
                && inferred_value_type != Type::named("Unknown")
            {
                map.value_type = inferred_value_type.clone();
            }
            if let Some(index) = map
                .entries
                .iter()
                .position(|(candidate_key, _)| *candidate_key == key)
            {
                Some(std::mem::replace(&mut map.entries[index].1, value))
            } else {
                map.entries.push((key, value));
                None
            }
        });
        boxed_value(previous.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_remove_in_place(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = unsafe { take_value(key) };
        let previous = with_map_mut(map, |map| {
            if let Some(index) = map
                .entries
                .iter()
                .position(|(candidate_key, _)| *candidate_key == key)
            {
                Some(map.entries.remove(index).1)
            } else {
                None
            }
        });
        boxed_value(previous.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_contains_key(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let key = unsafe { take_value(key) };
        i64::from(with_map(map, |map| {
            map.entries
                .iter()
                .any(|(candidate_key, _)| *candidate_key == key)
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_keys(map: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result = with_map(map, |map| {
            let mut elements =
                try_array_buffer(map.entries.len(), "Array-containing Map keys copy")?;
            for (key, _) in &map.entries {
                elements.push(try_clone_array_containing_value(key)?);
            }
            Ok((map.key_type.clone(), elements))
        });
        let (key_type, elements) = direct_array_result(result, 0, 0);
        boxed_value(Value::Vec(VecValue {
            element_type: key_type,
            elements,
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_values(map: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result = with_map(map, |map| {
            let mut elements =
                try_array_buffer(map.entries.len(), "Array-containing Map values copy")?;
            for (_, value) in &map.entries {
                elements.push(try_clone_array_containing_value(value)?);
            }
            Ok((map.value_type.clone(), elements))
        });
        let (value_type, elements) = direct_array_result(result, 0, 0);
        boxed_value(Value::Vec(VecValue {
            element_type: value_type,
            elements,
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_items(map: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let result = with_map(map, |map| {
            let element_type = Type::Tuple(vec![map.key_type.clone(), map.value_type.clone()]);
            let mut elements =
                try_array_buffer(map.entries.len(), "Array-containing Map items copy")?;
            for (key, value) in &map.entries {
                elements.push(Value::Tuple(TupleValue {
                    element_types: vec![map.key_type.clone(), map.value_type.clone()],
                    elements: vec![
                        try_clone_array_containing_value(key)?,
                        try_clone_array_containing_value(value)?,
                    ],
                }));
            }
            Ok((element_type, elements))
        });
        let (element_type, elements) = direct_array_result(result, 0, 0);
        boxed_value(Value::Vec(VecValue {
            element_type,
            elements,
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_index(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = unsafe { take_value(key) };
        let value = with_map(map, |map| {
            map.entries
                .iter()
                .find(|(candidate_key, _)| *candidate_key == key)
                .map(|(_, value)| try_clone_array_containing_value(value))
        });
        let Some(value) = value else {
            let message = format!("dict key `{}` was not present", key.render());
            match runtime_span(line, column) {
                Some(span) => {
                    runtime_diagnostic_error(Diagnostic::coded_at("AU4003", span, message))
                }
                None => runtime_diagnostic_error(Diagnostic::coded("AU4003", message)),
            }
        };
        boxed_value(direct_array_result(value, line, column))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_set_index_in_place(
    map: *mut OpaqueValue,
    key: *mut OpaqueValue,
    value: *mut OpaqueValue,
    _line: i64,
    _column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let key = unsafe { consume_owned_value(key) };
        let value = unsafe { consume_owned_value(value) };
        with_map_mut(map, |map| {
            if let Some(index) = map
                .entries
                .iter()
                .position(|(candidate_key, _)| *candidate_key == key)
            {
                map.entries[index].1 = value;
            } else {
                map.entries.push((key, value));
            }
        });
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_clear_in_place(map: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        with_map_mut(map, |map| map.entries.clear());
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_map_extend_in_place(
    map: *mut OpaqueValue,
    other: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let other = unsafe { consume_owned_value(other) };
        let Value::Map(other) = other else {
            runtime_error("`update` requires another `dict[K, V]` value");
        };
        with_map_mut(map, |map| {
            for (key, value) in other.entries {
                if let Some(index) = map
                    .entries
                    .iter()
                    .position(|(candidate_key, _)| *candidate_key == key)
                {
                    map.entries[index].1 = value;
                } else {
                    map.entries.push((key, value));
                }
            }
        });
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_empty() -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::Set(SetValue {
            element_type: Type::named("Unknown"),
            elements: Vec::new(),
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_len(set: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(
        || match i64::try_from(with_set(set, |set| set.elements.len())) {
            Ok(length) => length,
            Err(_) => runtime_error("set length does not fit in the direct runtime range"),
        },
    )
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_is_empty(set: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| i64::from(with_set(set, |set| set.elements.is_empty())))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_contains(
    set: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let needle = unsafe { take_value(value) };
        i64::from(with_set(set, |set| set.elements.contains(&needle)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_insert_in_place(
    set: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        let inferred = inferred_collection_type(&value);
        let inserted = with_set_mut(set, |set| {
            if set.element_type == Type::named("Unknown") && inferred != Type::named("Unknown") {
                set.element_type = inferred.clone();
            }
            if set.elements.contains(&value) {
                false
            } else {
                set.elements.push(value);
                true
            }
        });
        i64::from(inserted)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_remove_in_place(
    set: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| {
        let value = unsafe { take_value(value) };
        let removed = with_set_mut(set, |set| {
            if let Some(index) = set
                .elements
                .iter()
                .position(|candidate| *candidate == value)
            {
                set.elements.remove(index);
                true
            } else {
                false
            }
        });
        i64::from(removed)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_index_option(
    set: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        if index < 0 {
            runtime_error(format!("list index `{}` cannot be negative", index));
        }
        // Every supported Aura release target is 64-bit, so a validated
        // non-negative int64 index always fits usize.
        let index = index as usize;
        let value = with_set(set, |set| {
            set.elements
                .get(index)
                .map(try_clone_array_containing_value)
                .transpose()
        });
        let value = direct_array_result(value, 0, 0);
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_take_index_in_place(
    set: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        if index < 0 {
            runtime_error(format!("set index `{}` cannot be negative", index));
        }
        // Every supported Aura release target is 64-bit, so a validated
        // non-negative int64 index always fits usize.
        let index = index as usize;
        let value = with_set_mut(set, |set| {
            (index < set.elements.len()).then(|| set.elements.remove(index))
        });
        boxed_value(value.map(option_some).unwrap_or_else(option_none))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_clone_value(value: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let runtime_type_name = unsafe { explicit_runtime_type_name(value) };
        let cloned = unsafe { with_value(value, try_clone_array_containing_value) };
        boxed_value_with_type(direct_array_result(cloned, 0, 0), runtime_type_name)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tag_value_type(
    value: *mut OpaqueValue,
    type_ptr: *const u8,
    type_len: usize,
) {
    task_runtime_boundary(|| unsafe {
        set_explicit_runtime_type_name(value, decode_bytes(type_ptr, type_len));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unbox_i64(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Int(value) => match value.as_i128().and_then(|value| i64::try_from(value).ok()) {
            Some(value) => value,
            None => runtime_error("direct backend expected an integer that fits in host i64"),
        },
        other => runtime_error(format!(
            "direct backend expected `int32`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unbox_int64(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Int(value) => match value.as_i128().and_then(|value| i64::try_from(value).ok()) {
            Some(value) => value,
            None => runtime_error(format!("integer value `{value}` does not fit in `int64`")),
        },
        other => runtime_error(format!(
            "direct backend expected `int64`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_integer_to_float(value: *mut OpaqueValue) -> f64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Int(value) => value.to_f64(),
        other => runtime_error(format!(
            "direct backend expected an integer, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_integer_width_binary(
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
    operation: i64,
    arithmetic_mode: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let operation_name = direct_array_result(
            match operation {
                0 => Ok("add"),
                1 => Ok("sub"),
                2 => Ok("mul"),
                3 => Ok("shl"),
                4 => Ok("shr"),
                other => Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "direct integer width-arithmetic ABI received invalid operation code `{other}`"
                    ),
                )),
            },
            line,
            column,
        );
        let mode_name = direct_array_result(
            match arithmetic_mode {
                1 => Ok("wrapping"),
                2 => Ok("saturating"),
                other => Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "direct integer width-arithmetic ABI received invalid mode code `{other}`"
                    ),
                )),
            },
            line,
            column,
        );
        let read_integer = |value: *mut OpaqueValue, label: &str| unsafe {
            with_value(value, |value| {
                match value {
                Value::Int(value) => Ok(*value),
                other => Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "direct integer width-arithmetic ABI expected an integer {label} operand, found `{}`",
                        value_type_name(other)
                    ),
                )),
            }
            })
        };
        let left = direct_array_result(read_integer(left, "left"), line, column);
        let right = direct_array_result(read_integer(right, "right"), line, column);
        let mismatch = || {
            Diagnostic::coded(
                "AU4001",
                format!(
                    "`{mode_name}_{operation_name}` expects matching fixed-width integer operands"
                ),
            )
        };
        let result = match (arithmetic_mode, operation) {
            (1, 0) => left.wrapping_add(right).ok_or_else(mismatch),
            (1, 1) => left.wrapping_sub(right).ok_or_else(mismatch),
            (1, 2) => left.wrapping_mul(right).ok_or_else(mismatch),
            (2, 0) => left.saturating_add(right).ok_or_else(mismatch),
            (2, 1) => left.saturating_sub(right).ok_or_else(mismatch),
            (2, 2) => left.saturating_mul(right).ok_or_else(mismatch),
            (1, 3) => left
                .wrapping_shl(right)
                .map_err(native_integer_shift_diagnostic),
            (1, 4) => left
                .wrapping_shr(right)
                .map_err(native_integer_shift_diagnostic),
            (2, 3) => left
                .saturating_shl(right)
                .map_err(native_integer_shift_diagnostic),
            (2, 4) => left
                .saturating_shr(right)
                .map_err(native_integer_shift_diagnostic),
            _ => unreachable!("operation and mode codes were validated"),
        };
        boxed_value(Value::Int(direct_array_result(result, line, column)))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unbox_u64(value: *mut OpaqueValue) -> u64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Int(value) => match value.as_i128().and_then(|value| u64::try_from(value).ok()) {
            Some(value) => value,
            None => runtime_error("direct backend expected an integer that fits in host u64"),
        },
        other => runtime_error(format!(
            "direct backend expected `uint64`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unbox_f64(value: *mut OpaqueValue) -> f64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Float(value) => value,
        other => runtime_error(format!(
            "direct backend expected `float64`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unbox_bool(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Bool(value) => i64::from(value),
        other => runtime_error(format!(
            "direct backend expected `bool`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_print_value(value: *mut OpaqueValue) {
    task_runtime_boundary(|| {
        let mut rendered = unsafe { with_value(value, Value::render) };
        rendered.push('\n');
        write_stdout(&rendered);
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_value_as_condition(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::Bool(value) => i64::from(value),
        Value::Int(value) => i64::from(!value.is_zero()),
        Value::Unit => 0,
        other => runtime_error(format!(
            "direct backend cannot use `{}` as a branch condition",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unary_value(
    op: i32,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let op = match op {
            0 => UnaryOp::Neg,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            other => runtime_error(format!("unknown unary opcode `{}`", other)),
        };
        match eval_unary_value(unsafe { take_value(value) }, op) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_error(error.message),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unary_value_at(
    op: i32,
    value: *mut OpaqueValue,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let op = match op {
            0 => UnaryOp::Neg,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            other => runtime_error(format!("unknown unary opcode `{}`", other)),
        };
        match eval_unary_value(unsafe { take_value(value) }, op) {
            Ok(value) => boxed_value(value),
            Err(error) => match runtime_span(line, column) {
                Some(span) => runtime_error_at(span, error.message),
                None => runtime_error(error.message),
            },
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_binary_value(
    op: i32,
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let op = match op {
            0 => BinaryOp::Add,
            1 => BinaryOp::Sub,
            2 => BinaryOp::Mul,
            3 => BinaryOp::Div,
            4 => BinaryOp::Mod,
            5 => BinaryOp::Eq,
            6 => BinaryOp::NotEq,
            7 => BinaryOp::Less,
            8 => BinaryOp::LessEq,
            9 => BinaryOp::Greater,
            10 => BinaryOp::GreaterEq,
            11 => BinaryOp::And,
            12 => BinaryOp::Or,
            13 => BinaryOp::FloorDiv,
            14 => BinaryOp::Pow,
            15 => BinaryOp::BitAnd,
            16 => BinaryOp::BitOr,
            17 => BinaryOp::BitXor,
            18 => BinaryOp::Shl,
            19 => BinaryOp::Shr,
            other => runtime_error(format!("unknown binary opcode `{}`", other)),
        };
        match eval_binary_value(
            unsafe { take_value(left) },
            unsafe { take_value(right) },
            op,
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_binary_value_at(
    op: i32,
    left: *mut OpaqueValue,
    right: *mut OpaqueValue,
    float_width: i64,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let op = match op {
            0 => BinaryOp::Add,
            1 => BinaryOp::Sub,
            2 => BinaryOp::Mul,
            3 => BinaryOp::Div,
            4 => BinaryOp::Mod,
            5 => BinaryOp::Eq,
            6 => BinaryOp::NotEq,
            7 => BinaryOp::Less,
            8 => BinaryOp::LessEq,
            9 => BinaryOp::Greater,
            10 => BinaryOp::GreaterEq,
            11 => BinaryOp::And,
            12 => BinaryOp::Or,
            13 => BinaryOp::FloorDiv,
            14 => BinaryOp::Pow,
            15 => BinaryOp::BitAnd,
            16 => BinaryOp::BitOr,
            17 => BinaryOp::BitXor,
            18 => BinaryOp::Shl,
            19 => BinaryOp::Shr,
            other => runtime_error(format!("unknown binary opcode `{}`", other)),
        };
        let float_width = match float_width {
            32 => FloatPowerWidth::Float32,
            0 | 64 => FloatPowerWidth::Float64,
            other => runtime_error(format!("unknown direct floating width `{other}`")),
        };
        match eval_binary_value_with_float_width(
            unsafe { take_value(left) },
            unsafe { take_value(right) },
            op,
            float_width,
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error_at(error, runtime_span(line, column)),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cast_value(
    value: *mut OpaqueValue,
    target_ptr: *const u8,
    target_len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let target = Type::named(decode_bytes(target_ptr, target_len));
        match cast_numeric_value(unsafe { take_value(value) }, &target, None) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_error(error.message),
        }
    })
}

fn direct_integer_cast_source(value: u64, unsigned: i64) -> Value {
    match unsigned {
        0 => Value::Int(IntegerValue::from_signed((value as i64) as i128)),
        1 => Value::Int(IntegerValue::from_literal(u128::from(value))),
        other => runtime_error(format!("unknown direct integer source kind `{other}`")),
    }
}

fn direct_integer_cast_target(kind: i64) -> Type {
    Type::named(match kind {
        0 => "int32",
        1 => "int64",
        2 => "uint64",
        other => runtime_error(format!("unknown direct integer target kind `{other}`")),
    })
}

fn direct_float_cast_target(kind: i64) -> Type {
    Type::named(match kind {
        0 => "float32",
        1 => "float64",
        other => runtime_error(format!("unknown direct float target kind `{other}`")),
    })
}

fn direct_cast_failure(error: Diagnostic, line: i64, column: i64) -> ! {
    match runtime_span(line, column) {
        Some(span) => runtime_error_at(span, error.message),
        None => runtime_error(error.message),
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cast_integer_to_integer(
    value: u64,
    source_unsigned: i64,
    target_kind: i64,
    line: i64,
    column: i64,
) -> u64 {
    task_runtime_boundary(|| {
        let target = direct_integer_cast_target(target_kind);
        match cast_numeric_value(
            direct_integer_cast_source(value, source_unsigned),
            &target,
            None,
        ) {
            Ok(Value::Int(value)) => match value.representation() {
                IntegerRepresentation::Signed(value) => (value as i64) as u64,
                IntegerRepresentation::Unsigned(value) => value as u64,
            },
            Ok(other) => runtime_error(format!(
                "direct integer cast unexpectedly produced `{}`",
                value_type_name(&other)
            )),
            Err(error) => direct_cast_failure(error, line, column),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cast_integer_to_float(
    value: u64,
    source_unsigned: i64,
    target_kind: i64,
    line: i64,
    column: i64,
) -> f64 {
    task_runtime_boundary(|| {
        let target = direct_float_cast_target(target_kind);
        match cast_numeric_value(
            direct_integer_cast_source(value, source_unsigned),
            &target,
            None,
        ) {
            Ok(Value::Float(value)) => value,
            Ok(other) => runtime_error(format!(
                "direct float cast unexpectedly produced `{}`",
                value_type_name(&other)
            )),
            Err(error) => direct_cast_failure(error, line, column),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cast_float_to_integer(
    value: f64,
    target_kind: i64,
    line: i64,
    column: i64,
) -> u64 {
    task_runtime_boundary(|| {
        let target = direct_integer_cast_target(target_kind);
        match cast_numeric_value(Value::Float(value), &target, None) {
            Ok(Value::Int(value)) => match value.representation() {
                IntegerRepresentation::Signed(value) => (value as i64) as u64,
                IntegerRepresentation::Unsigned(value) => value as u64,
            },
            Ok(other) => runtime_error(format!(
                "direct integer cast unexpectedly produced `{}`",
                value_type_name(&other)
            )),
            Err(error) => direct_cast_failure(error, line, column),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cast_value_at(
    value: *mut OpaqueValue,
    target_ptr: *const u8,
    target_len: usize,
    line: i64,
    column: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let target = Type::named(decode_bytes(target_ptr, target_len));
        match cast_numeric_value(unsafe { take_value(value) }, &target, None) {
            Ok(value) => boxed_value(value),
            Err(error) => match runtime_span(line, column) {
                Some(span) => runtime_error_at(span, error.message),
                None => runtime_error(error.message),
            },
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_value_type_matches(
    value: *mut OpaqueValue,
    type_ptr: *const u8,
    type_len: usize,
) -> i64 {
    task_runtime_boundary(|| {
        let expected = decode_bytes(type_ptr, type_len);
        let explicit_type = unsafe { effective_runtime_type_name(value) };
        let actual = unsafe { value_ref(value) };
        if let Some(pattern) = canonical_runtime_type_from_name(&expected) {
            let actual_type = explicit_type
                .as_deref()
                .map(runtime_type_from_name)
                .unwrap_or_else(|| inferred_collection_type(&actual));
            return i64::from(runtime_type_pattern_matches(
                &pattern,
                &actual_type,
                &mut BTreeMap::new(),
            ));
        }
        if expected.contains('?') {
            let pattern = runtime_type_pattern_from_name(&expected);
            if let Some(actual_type) = explicit_type.as_deref() {
                return i64::from(runtime_type_pattern_matches(
                    &pattern,
                    &runtime_type_from_name(actual_type),
                    &mut BTreeMap::new(),
                ));
            }
            let untagged_outer_wildcard = match &pattern {
                Type::Named(name, args) => {
                    value_type_name(&actual) == *name
                        && args.iter().all(|arg| matches!(arg, Type::TypeParam(_)))
                }
                Type::Tuple(_) => match &actual {
                    Value::Tuple(tuple) => runtime_type_pattern_matches(
                        &pattern,
                        &Type::Tuple(tuple.element_types.clone()),
                        &mut BTreeMap::new(),
                    ),
                    _ => false,
                },
                Type::Function { .. }
                | Type::Closure { .. }
                | Type::Unit
                | Type::Module(_)
                | Type::TypeParam(_) => false,
            };
            return i64::from(untagged_outer_wildcard);
        }
        let explicit_matches = explicit_type.as_deref() == Some(expected.as_str());
        let structural_matches = match &actual {
            Value::Instance(instance) => {
                nominal_runtime_base_name(&instance.class_name) == expected
            }
            Value::EnumVariant(variant) => {
                nominal_runtime_base_name(&variant.enum_name) == expected
            }
            Value::String(_) => expected == "str",
            Value::Tuple(tuple) => {
                expected == "tuple"
                    || Type::Tuple(tuple.element_types.clone()).to_string() == expected
            }
            Value::Vec(_) => expected == "list",
            Value::Array(array) => {
                expected == "Array"
                    || expected == format!("Array[{}]", array.dtype().runtime_type_name())
            }
            Value::Set(_) => expected == "set",
            Value::Map(_) => expected == "dict",
            Value::Channel(_) => expected == "Queue",
            Value::Task(_) => expected == "Task",
            Value::TaskGroup(_) => expected == "TaskGroup",
            Value::Function(function) => function.signature.to_string() == expected,
            Value::FfiHandle(handle) => handle.type_name() == expected,
            Value::File(_) => expected == "fs.File",
            Value::TcpListener(_) => expected == "net.TcpListener",
            Value::TcpStream(_) => expected == "net.TcpStream",
            Value::UdpSocket(_) => expected == "net.UdpSocket",
            Value::UdpDatagram(_) => expected == "net.UdpDatagram",
            Value::HttpListener(_) => expected == "net.HttpListener",
            Value::HttpExchange(_) => expected == "net.HttpExchange",
            Value::HttpResponse(_) => expected == "net.HttpResponse",
            Value::WebSocketListener(_) => expected == "net.WebSocketListener",
            Value::WebSocket(_) => expected == "net.WebSocket",
            Value::UnixListener(_) => expected == "net.UnixListener",
            Value::UnixStream(_) => expected == "net.UnixStream",
            Value::TlsListener(_) => expected == "net.TlsListener",
            Value::TlsStream(_) => expected == "net.TlsStream",
            Value::ProcessChild(_) => expected == "process.Child",
            Value::ProcessPipe(_) => expected == "process.Pipe",
            Value::ProcessCompleted(_) => expected == "process.Completed",
            Value::ProcessSupervisor(_) => expected == "process.Supervisor",
            Value::Duration(_) => expected == "Duration",
            Value::Rng(_) => expected == "random.Rng",
            Value::Range(_) => expected == "Range",
            Value::Bool(_) => expected == "bool",
            Value::Float(_) => expected == "float64" || expected == "float32",
            Value::Int(value) => value.runtime_type_name().map_or_else(
                || expected.starts_with("int") || expected.starts_with("uint"),
                |actual| actual == expected,
            ),
            Value::Unit => expected == "None",
            Value::ModuleNamespace(_) => expected.starts_with("module "),
        };
        i64::from(explicit_matches || structural_matches)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_value_has_runtime_type(value: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| i64::from(unsafe { effective_runtime_type_name(value) }.is_some()))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_enum_variant(
    enum_ptr: *const u8,
    enum_len: usize,
    variant_ptr: *const u8,
    variant_len: usize,
    payloads_ptr: *mut i64,
    payload_count: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let payload_count = usize::try_from(payload_count)
            .unwrap_or_else(|_| runtime_error("invalid enum payload count"));
        boxed_value(Value::EnumVariant(EnumVariantValue {
            enum_name: decode_bytes(enum_ptr, enum_len),
            variant_name: decode_bytes(variant_ptr, variant_len),
            payloads: if payload_count == 0 {
                Vec::new()
            } else {
                unsafe { consume_owned_opaque_buffer(payloads_ptr, payload_count) }
            },
        }))
    })
}

/// Constructs a tuple by consuming every owned opaque handle in `elements_ptr`.
///
/// The caller must allocate the buffer with `aura_direct_arg_buffer_new` and
/// fill it with `aura_direct_arg_buffer_store_owned`. Tuple values use the
/// same single opaque-handle ABI as other heterogeneous aggregate values.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tuple_new(
    elements_ptr: *mut i64,
    element_count: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let element_count = usize::try_from(element_count)
            .unwrap_or_else(|_| runtime_error("invalid tuple element count"));
        if element_count > 0 && elements_ptr.is_null() {
            runtime_error("direct runtime received a null tuple element buffer");
        }
        let elements = if element_count == 0 {
            Vec::new()
        } else {
            unsafe { consume_owned_opaque_buffer_for(elements_ptr, element_count, "tuple element") }
        };
        let element_types = elements.iter().map(inferred_collection_type).collect();
        boxed_value(Value::Tuple(TupleValue {
            element_types,
            elements,
        }))
    })
}

/// Returns an independently owned clone of a tuple element.
///
/// Semantic checking must restrict indexed projection to Copy element types.
/// Consuming extraction is a separate internal-only destructuring ABI and must
/// never be selected for user-visible indexed access.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tuple_element(
    value: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let index =
            usize::try_from(index).unwrap_or_else(|_| runtime_error("invalid tuple element index"));
        unsafe {
            with_value(value, |value| {
                let Value::Tuple(tuple) = value else {
                    runtime_error(format!(
                        "expected tuple value, found `{}`",
                        value_type_name(value)
                    ));
                };
                if matches!(tuple.elements.get(index), Some(Value::Unit))
                    && !matches!(tuple.element_types.get(index), Some(Type::Unit))
                {
                    runtime_error(format!(
                        "tuple element at index {} has already been moved",
                        index
                    ));
                }
                tuple
                    .elements
                    .get(index)
                    .cloned()
                    .map(boxed_value)
                    .unwrap_or_else(|| {
                        runtime_error(format!(
                            "tuple of length {} has no element at index {}",
                            tuple.elements.len(),
                            index
                        ))
                    })
            })
        }
    })
}

/// Destructively transfers one element from a tuple that is already owned by a
/// private MIR destructuring temporary.
///
/// This ABI must never back user-visible indexed access. Native codegen gates
/// it to compiler-generated `%t...` places after lowering has moved the whole
/// source tuple into that private temporary.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tuple_take_element(
    value: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let index =
            usize::try_from(index).unwrap_or_else(|_| runtime_error("invalid tuple element index"));
        let element = unsafe {
            value_mut(value, |value| {
                let Value::Tuple(tuple) = value else {
                    runtime_error(format!(
                        "expected tuple value, found `{}`",
                        value_type_name(value)
                    ));
                };
                if matches!(tuple.elements.get(index), Some(Value::Unit))
                    && !matches!(tuple.element_types.get(index), Some(Type::Unit))
                {
                    runtime_error(format!(
                        "tuple element at index {} has already been moved",
                        index
                    ));
                }
                let length = tuple.elements.len();
                let element = tuple.elements.get_mut(index).unwrap_or_else(|| {
                    runtime_error(format!(
                        "tuple of length {} has no element at index {}",
                        length, index
                    ))
                });
                std::mem::replace(element, Value::Unit)
            })
        };
        boxed_value(element)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_variant_matches(
    value: *mut OpaqueValue,
    enum_ptr: *const u8,
    enum_len: usize,
    variant_ptr: *const u8,
    variant_len: usize,
) -> i64 {
    task_runtime_boundary(|| {
        let expected_enum = decode_bytes(enum_ptr, enum_len);
        let expected_variant = decode_bytes(variant_ptr, variant_len);
        unsafe {
            with_value(value, |value| match value {
                Value::EnumVariant(variant) => i64::from(
                    nominal_runtime_base_name(&variant.enum_name) == expected_enum
                        && variant.variant_name == expected_variant,
                ),
                _ => 0,
            })
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_variant_payload(
    value: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(value) } {
        Value::EnumVariant(variant) => match variant.payloads.get(index.max(0) as usize) {
            Some(payload) => boxed_value(payload.clone()),
            None => runtime_error(format!(
                "enum variant `{}.{}` does not carry a payload at index {}",
                nominal_runtime_base_name(&variant.enum_name),
                variant.variant_name,
                index
            )),
        },
        other => runtime_error(format!(
            "expected enum value, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_variant_take_payload(
    value: *mut OpaqueValue,
    index: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let index =
            usize::try_from(index).unwrap_or_else(|_| runtime_error("invalid enum payload index"));
        let payload = unsafe {
            value_mut(value, |value| {
                let Value::EnumVariant(variant) = value else {
                    runtime_error(format!(
                        "expected enum value, found `{}`",
                        value_type_name(value)
                    ));
                };
                let payload = variant.payloads.get_mut(index).unwrap_or_else(|| {
                    runtime_error(format!(
                        "enum variant `{}.{}` does not carry a payload at index {}",
                        nominal_runtime_base_name(&variant.enum_name),
                        variant.variant_name,
                        index
                    ))
                });
                std::mem::replace(payload, Value::Unit)
            })
        };
        boxed_value(payload)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_new(
    class_ptr: *const u8,
    class_len: usize,
    names_ptr: *const *const u8,
    lens_ptr: *const usize,
    values_ptr: *const *mut OpaqueValue,
    count: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let class_name = decode_bytes(class_ptr, class_len);
        let names = unsafe { slice::from_raw_parts(names_ptr, count) };
        let lens = unsafe { slice::from_raw_parts(lens_ptr, count) };
        let values = unsafe { slice::from_raw_parts(values_ptr, count) };
        let mut fields = BTreeMap::new();
        for index in 0..count {
            let name = decode_bytes(names[index], lens[index]);
            fields.insert(name, unsafe { take_value(values[index]) });
        }
        boxed_value(Value::Instance(InstanceValue { class_name, fields }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_empty(
    class_ptr: *const u8,
    class_len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::Instance(InstanceValue {
            class_name: decode_bytes(class_ptr, class_len),
            fields: BTreeMap::new(),
        }))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_get_field(
    value: *mut OpaqueValue,
    field_ptr: *const u8,
    field_len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let field = decode_bytes(field_ptr, field_len);
        let cloned = unsafe {
            with_value(value, |value| {
                let Value::Instance(instance) = value else {
                    runtime_error(format!(
                        "cannot access field `{}` on non-instance `{}`",
                        field,
                        value_type_name(value)
                    ));
                };
                instance
                    .fields
                    .get(&field)
                    .map(try_clone_array_containing_value)
                    .unwrap_or_else(|| {
                        runtime_error(format!(
                            "class `{}` has no field `{}`",
                            instance.class_name, field
                        ))
                    })
            })
        };
        boxed_value(direct_array_result(cloned, 0, 0))
    })
}

fn take_direct_instance_field(
    value: &mut Value,
    segments: &[&str],
    full_path: &str,
) -> std::result::Result<Value, String> {
    let Value::Instance(instance) = value else {
        return Err(format!(
            "cannot move field `{full_path}` from non-instance `{}`",
            value_type_name(value)
        ));
    };
    let Some((field, rest)) = segments.split_first() else {
        return Err("direct runtime received an empty instance field path".to_string());
    };
    if rest.is_empty() {
        return instance.fields.remove(*field).ok_or_else(|| {
            format!(
                "class `{}` has no field `{}` in move path `{full_path}`",
                instance.class_name, field
            )
        });
    }
    let nested = instance.fields.get_mut(*field).ok_or_else(|| {
        format!(
            "class `{}` has no field `{}` in move path `{full_path}`",
            instance.class_name, field
        )
    })?;
    take_direct_instance_field(nested, rest, full_path)
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_take_field(
    value: *mut OpaqueValue,
    field_ptr: *const u8,
    field_len: usize,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let field_path = decode_bytes(field_ptr, field_len);
        let segments = field_path.split('.').collect::<Vec<_>>();
        if segments.iter().any(|segment| segment.is_empty()) {
            runtime_error(format!("invalid instance move path `{field_path}`"));
        }
        let moved = unsafe {
            value_mut(value, |value| {
                take_direct_instance_field(value, &segments, &field_path)
            })
        }
        .unwrap_or_else(|message| runtime_error(message));
        boxed_value(moved)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_set_field(
    value: *mut OpaqueValue,
    field_ptr: *const u8,
    field_len: usize,
    new_value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let field = decode_bytes(field_ptr, field_len);
        let updated = unsafe { with_value(value, try_clone_array_containing_value) };
        let Value::Instance(mut updated) = direct_array_result(updated, 0, 0) else {
            let other = unsafe { with_value(value, |value| value_type_name(value)) };
            runtime_error(format!(
                "cannot assign field `{}` on non-instance `{}`",
                field, other
            ));
        };
        let new_value = unsafe { with_value(new_value, try_clone_array_containing_value) };
        updated
            .fields
            .insert(field, direct_array_result(new_value, 0, 0));
        boxed_value(Value::Instance(updated))
    })
}

fn set_direct_instance_field_owned(
    value: &mut Value,
    segments: &[&str],
    full_path: &str,
    new_value: Value,
) -> std::result::Result<(), String> {
    let Some((projection, rest)) = segments.split_first() else {
        return Err("direct runtime received an empty instance assignment path".to_string());
    };
    match value {
        Value::Instance(instance) => {
            if rest.is_empty() {
                instance.fields.insert((*projection).to_string(), new_value);
                return Ok(());
            }
            let nested = instance.fields.get_mut(*projection).ok_or_else(|| {
                format!(
                    "class `{}` has no field `{}` in assignment path `{full_path}`",
                    instance.class_name, projection
                )
            })?;
            set_direct_instance_field_owned(nested, rest, full_path, new_value)
        }
        Value::Tuple(tuple) => {
            let index = projection.parse::<usize>().map_err(|_| {
                format!(
                    "tuple projection `{projection}` is not a fixed position in assignment path `{full_path}`"
                )
            })?;
            let tuple_len = tuple.elements.len();
            let nested = tuple.elements.get_mut(index).ok_or_else(|| {
                format!(
                    "tuple of length {} has no element at index {index} in assignment path `{full_path}`",
                    tuple_len
                )
            })?;
            if rest.is_empty() {
                *nested = new_value;
                Ok(())
            } else {
                set_direct_instance_field_owned(nested, rest, full_path, new_value)
            }
        }
        other => Err(format!(
            "cannot assign field `{full_path}` on non-instance `{}`",
            value_type_name(other)
        )),
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_instance_set_field_owned(
    value: *mut OpaqueValue,
    field_ptr: *const u8,
    field_len: usize,
    new_value: *mut OpaqueValue,
) {
    task_runtime_boundary(|| {
        let field_path = decode_bytes(field_ptr, field_len);
        let segments = field_path.split('.').collect::<Vec<_>>();
        if segments.iter().any(|segment| segment.is_empty()) {
            runtime_error(format!("invalid instance assignment path `{field_path}`"));
        }
        let new_value = unsafe { consume_owned_value(new_value) };
        unsafe {
            value_mut(value, |value| {
                set_direct_instance_field_owned(value, &segments, &field_path, new_value)
            })
        }
        .unwrap_or_else(|message| runtime_error(message));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_arg_buffer_new(count: i64) -> *mut i64 {
    task_runtime_boundary(|| {
        let count = match usize::try_from(count) {
            Ok(count) => count,
            Err(_) => runtime_error("invalid arg buffer size"),
        };
        let mut values = vec![0i64; count].into_boxed_slice();
        let ptr = values.as_mut_ptr();
        Box::leak(values);
        ptr
    })
}

/// Registers a newly allocated task-argument buffer with the current direct
/// runtime cleanup stack until ownership is handed to the scheduler.
///
/// Defaults and later task-start operands can trap after the raw buffer has
/// been allocated. Keeping this zero-thunk registration live makes those
/// exits release both the allocation and every retained opaque argument.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_arg_buffer_guard(buffer: *mut i64, count: i64) -> i64 {
    task_runtime_boundary(|| {
        let count = usize::try_from(count)
            .unwrap_or_else(|_| runtime_error("invalid guarded task arg buffer size"));
        push_direct_cleanup_registration(0, buffer, count)
    })
}

/// Detaches a guarded task-argument buffer immediately before the task runtime
/// reconstructs it as external scheduler-owned state.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_arg_buffer_disarm(id: i64) {
    task_runtime_boundary(|| {
        let mut registration = take_direct_cleanup_registration(id)
            .unwrap_or_else(|| runtime_error("unknown guarded task arg buffer"));
        if registration.thunk_ptr != 0 {
            runtime_error("task arg buffer guard id referred to an ordinary cleanup");
        }
        registration.args = std::ptr::null_mut();
        registration.arg_count = 0;
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_host_builtin(
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *mut i64,
    arg_count: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let name = decode_bytes(name_ptr, name_len);
        let arg_count = usize::try_from(arg_count)
            .unwrap_or_else(|_| runtime_error("invalid host builtin argument count"));
        if is_dynamic_json_host_builtin(&name) || is_dynamic_bytes_host_builtin(&name) {
            let args = unsafe { DirectHostArgBuffer::from_raw(args_ptr, arg_count) };
            let result = if is_dynamic_json_host_builtin(&name) {
                evaluate_direct_json_host_builtin(&name, &args)
            } else {
                evaluate_direct_bytes_host_builtin(&name, &args)
            };
            drop(args);
            return match result {
                Ok(value) => boxed_value(value),
                Err(error) => runtime_diagnostic_error(error),
            };
        }
        let args = unsafe { consume_opaque_buffer(args_ptr, arg_count) };
        match evaluate_host_builtin(&name, args) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_arg_buffer_store(buffer: *mut i64, index: i64, value: i64) {
    task_runtime_boundary(|| {
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(_) => runtime_error("invalid arg index"),
        };
        unsafe {
            let previous = *buffer.add(index);
            if previous != 0 {
                release_untracked_value(previous as *mut OpaqueValue);
            }
            if value != 0 {
                retain_untracked_value(value as *mut OpaqueValue);
            }
            *buffer.add(index) = value;
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_arg_buffer_store_owned(
    buffer: *mut i64,
    index: i64,
    value: i64,
) {
    task_runtime_boundary(|| {
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(_) => runtime_error("invalid owned arg index"),
        };
        unsafe {
            let previous = *buffer.add(index);
            if previous != 0 {
                release_untracked_value(previous as *mut OpaqueValue);
            }
            if value != 0 {
                unregister_direct_owned_value(value as *mut OpaqueValue);
            }
            *buffer.add(index) = value;
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_register_cleanup(
    thunk_ptr: i64,
    args: *mut i64,
    arg_count: i64,
) -> i64 {
    task_runtime_boundary(|| {
        let arg_count = match usize::try_from(arg_count) {
            Ok(arg_count) => arg_count,
            Err(_) => runtime_error("invalid cleanup arg count"),
        };
        if thunk_ptr == 0 {
            unsafe {
                release_direct_cleanup_args(args, arg_count);
            }
            runtime_error("invalid cleanup thunk pointer");
        }
        push_direct_cleanup_registration(thunk_ptr, args, arg_count)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unregister_cleanup(id: i64) {
    task_runtime_boundary(|| {
        drop(take_direct_cleanup_registration(id));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_refresh_cleanup(
    active: i64,
    id: i64,
    thunk_ptr: i64,
    args: *mut i64,
    arg_count: i64,
) -> i64 {
    task_runtime_boundary(|| {
        let arg_count = match usize::try_from(arg_count) {
            Ok(arg_count) => arg_count,
            Err(_) => runtime_error("invalid cleanup arg count"),
        };
        if active == 0 {
            drop(take_direct_cleanup_registration(id));
            unsafe {
                release_direct_cleanup_args(args, arg_count);
            }
            return 0;
        }
        if thunk_ptr == 0 {
            unsafe {
                release_direct_cleanup_args(args, arg_count);
            }
            drop(take_direct_cleanup_registration(id));
            runtime_error("invalid cleanup thunk pointer");
        }
        drop(take_direct_cleanup_registration(id));
        push_direct_cleanup_registration(thunk_ptr, args, arg_count)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_new(capacity: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        if capacity.is_null() {
            return boxed_value(Value::Channel(ChannelValue::new()));
        }
        let capacity = expect_i32_value(
            unsafe { value_ref(capacity) }.borrow(),
            "Queue(capacity=...)",
        );
        if capacity <= 0 {
            runtime_error("`Queue(capacity=...)` expects a positive `int32`");
        }
        boxed_value(Value::Channel(ChannelValue::with_capacity(
            capacity as usize,
        )))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_group_new() -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        boxed_value(Value::TaskGroup(TaskGroupValue::new(
            &current_cancellation(),
        )))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_cancelled() -> i64 {
    task_runtime_boundary(|| {
        if poll_cancellation(&current_cancellation()) {
            1
        } else {
            0
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_yield_now() {
    task_runtime_boundary(yield_now_with_runtime_scheduler)
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_set_returned_view_projection(
    projection_ptr: *const u8,
    projection_len: usize,
) {
    task_runtime_boundary(|| {
        let projection = decode_bytes(projection_ptr, projection_len);
        with_direct_task_runtime_state(|state| {
            state.returned_view_projection = Some(projection);
        });
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_take_returned_view_projection(
    projections_ptr: *const u8,
    projections_len: usize,
) -> i64 {
    task_runtime_boundary(|| {
        let projections = unsafe { slice::from_raw_parts(projections_ptr, projections_len) };
        let selected =
            with_direct_task_runtime_state(|state| state.returned_view_projection.take())
                .unwrap_or_else(|| {
                    runtime_error("direct returned view has no transferred projection")
                });
        projections
            .split(|byte| *byte == 0)
            .position(|projection| projection == selected.as_bytes())
            .and_then(|index| i64::try_from(index).ok())
            .unwrap_or_else(|| {
                runtime_error("direct returned view selected an undeclared projection")
            })
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_send(
    channel: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => {
                match channel.send_with_cancellation(value, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(SendValueError::Closed(value)) => {
                        boxed_value(result_err(send_error_closed(*value)))
                    }
                    Err(SendValueError::Cancelled(value)) => {
                        boxed_value(result_err(send_error_cancelled(*value)))
                    }
                    Err(SendValueError::TimedOut(value)) => {
                        boxed_value(result_err(send_error_timed_out(*value)))
                    }
                    Err(SendValueError::Full(value)) => {
                        boxed_value(result_err(send_error_full(*value)))
                    }
                }
            }
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_send_timeout_value(
    channel: *mut OpaqueValue,
    value: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        let timeout = duration_from_ptr(duration, "put(timeout=...)");
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => match direct_timer_result_or_trap(channel.send_with_timeout(
                value,
                Some(timeout),
                Some(&current_cancellation()),
            )) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(SendValueError::Closed(value)) => {
                    boxed_value(result_err(send_error_closed(*value)))
                }
                Err(SendValueError::Cancelled(value)) => {
                    boxed_value(result_err(send_error_cancelled(*value)))
                }
                Err(SendValueError::TimedOut(value)) => {
                    boxed_value(result_err(send_error_timed_out(*value)))
                }
                Err(SendValueError::Full(value)) => {
                    boxed_value(result_err(send_error_full(*value)))
                }
            },
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_try_send(
    channel: *mut OpaqueValue,
    value: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let value = unsafe { consume_owned_value(value) };
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => match channel.try_send_result(value) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(SendValueError::Closed(value)) => {
                    boxed_value(result_err(send_error_closed(*value)))
                }
                Err(SendValueError::TimedOut(value)) => {
                    boxed_value(result_err(send_error_timed_out(*value)))
                }
                Err(SendValueError::Cancelled(value)) => {
                    boxed_value(result_err(send_error_cancelled(*value)))
                }
                Err(SendValueError::Full(value)) => {
                    boxed_value(result_err(send_error_full(*value)))
                }
            },
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv(channel: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(channel) } {
        Value::Channel(channel) => boxed_value(
            match direct_timer_result_or_trap(
                channel.recv_result_with_cancellation(None, Some(&current_cancellation())),
            ) {
                RecvValueResult::Value(value) => queue_receive_item(value),
                RecvValueResult::Closed => queue_receive_closed(),
                RecvValueResult::TimedOut => queue_receive_timed_out(),
                RecvValueResult::Cancelled => queue_receive_cancelled(),
            },
        ),
        other => runtime_error(format!(
            "expected `Queue`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_in_task_group(
    channel: *mut OpaqueValue,
    task_group: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let channel_value = unsafe { value_ref(channel) };
        let task_group_value = unsafe { value_ref(task_group) };
        match (channel_value, task_group_value) {
            (Value::Channel(channel), Value::TaskGroup(group)) => boxed_value(
                match recv_for_task_group_iteration(&channel, &current_cancellation(), &group) {
                    RecvValueResult::Value(value) => queue_receive_item(value),
                    RecvValueResult::Closed => queue_receive_closed(),
                    RecvValueResult::TimedOut => queue_receive_timed_out(),
                    RecvValueResult::Cancelled => queue_receive_cancelled(),
                },
            ),
            (Value::Channel(_), other) => runtime_error(format!(
                "expected `TaskGroup`, found `{}`",
                value_type_name(other)
            )),
            (other, _) => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_with_registered_producers(
    channel: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(channel) } {
        Value::Channel(channel) => boxed_value(
            match recv_for_registered_producers_iteration(&channel, &current_cancellation()) {
                RecvValueResult::Value(value) => queue_receive_item(value),
                RecvValueResult::Closed => queue_receive_closed(),
                RecvValueResult::TimedOut => queue_receive_timed_out(),
                RecvValueResult::Cancelled => queue_receive_cancelled(),
            },
        ),
        other => runtime_error(format!(
            "expected `Queue`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_timeout_value(
    channel: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "get(timeout=...)");
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => {
                boxed_value(
                    match direct_timer_result_or_trap(channel.recv_result_with_cancellation(
                        Some(timeout),
                        Some(&current_cancellation()),
                    )) {
                        RecvValueResult::Value(value) => queue_receive_item(value),
                        RecvValueResult::Closed => queue_receive_closed(),
                        RecvValueResult::TimedOut => queue_receive_timed_out(),
                        RecvValueResult::Cancelled => queue_receive_cancelled(),
                    },
                )
            }
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_or_none(
    channel: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let cancellation = current_cancellation();
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => boxed_value(
                match if cancellation.is_cancelled() {
                    RecvValueResult::Cancelled
                } else {
                    match channel.try_recv() {
                        crate::runtime_value::TryRecvResult::Value(value) => {
                            RecvValueResult::Value(value)
                        }
                        crate::runtime_value::TryRecvResult::Closed => RecvValueResult::Closed,
                        crate::runtime_value::TryRecvResult::Empty => RecvValueResult::TimedOut,
                    }
                } {
                    RecvValueResult::Value(value) => option_some(value),
                    RecvValueResult::Closed
                    | RecvValueResult::TimedOut
                    | RecvValueResult::Cancelled => option_none(),
                },
            ),
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_or_none_timeout_value(
    channel: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "get_or_none(timeout=...)");
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => {
                boxed_value(
                    match direct_timer_result_or_trap(channel.recv_result_with_cancellation(
                        Some(timeout),
                        Some(&current_cancellation()),
                    )) {
                        RecvValueResult::Value(value) => option_some(value),
                        RecvValueResult::Closed
                        | RecvValueResult::TimedOut
                        | RecvValueResult::Cancelled => option_none(),
                    },
                )
            }
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_or_value(
    channel: *mut OpaqueValue,
    default: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let cancellation = current_cancellation();
        let default = unsafe { consume_owned_value(default) };
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => boxed_value(
                match if cancellation.is_cancelled() {
                    RecvValueResult::Cancelled
                } else {
                    match channel.try_recv() {
                        crate::runtime_value::TryRecvResult::Value(value) => {
                            RecvValueResult::Value(value)
                        }
                        crate::runtime_value::TryRecvResult::Closed => RecvValueResult::Closed,
                        crate::runtime_value::TryRecvResult::Empty => RecvValueResult::TimedOut,
                    }
                } {
                    RecvValueResult::Value(value) => value,
                    RecvValueResult::Closed
                    | RecvValueResult::TimedOut
                    | RecvValueResult::Cancelled => default,
                },
            ),
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_recv_or_value_timeout_value(
    channel: *mut OpaqueValue,
    default: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let default = unsafe { consume_owned_value(default) };
        let timeout = duration_from_ptr(duration, "get_or(timeout=...)");
        match unsafe { value_ref(channel) } {
            Value::Channel(channel) => {
                boxed_value(
                    match direct_timer_result_or_trap(channel.recv_result_with_cancellation(
                        Some(timeout),
                        Some(&current_cancellation()),
                    )) {
                        RecvValueResult::Value(value) => value,
                        RecvValueResult::Closed
                        | RecvValueResult::TimedOut
                        | RecvValueResult::Cancelled => default,
                    },
                )
            }
            other => runtime_error(format!(
                "expected `Queue`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_channel_close(channel: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(channel) } {
        Value::Channel(channel) => {
            channel.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `Queue`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join(task: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(task) } {
        Value::Task(task) => {
            if let Err(error) = task.claim_result_observation() {
                runtime_diagnostic_error(error);
            }
            match direct_timer_result_or_trap(
                task.wait_result_with_cancellation_observed(None, Some(&current_cancellation())),
            ) {
                TaskWaitStatus::Ready(result) => match result {
                    Ok(value) => boxed_value(task_result_ready(value)),
                    Err(error) => boxed_value(task_result_error(error.message)),
                },
                TaskWaitStatus::TimedOut => boxed_value(task_result_timed_out()),
                TaskWaitStatus::Cancelled => boxed_value(task_result_cancelled()),
            }
        }
        other => runtime_error(format!(
            "expected `Task`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join_timeout_value(
    task: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "result(timeout=...)");
        match unsafe { value_ref(task) } {
            Value::Task(task) => {
                if let Err(error) = task.claim_result_observation() {
                    runtime_diagnostic_error(error);
                }
                match direct_timer_result_or_trap(task.wait_result_with_cancellation_observed(
                    Some(timeout),
                    Some(&current_cancellation()),
                )) {
                    TaskWaitStatus::Ready(result) => match result {
                        Ok(value) => boxed_value(task_result_ready(value)),
                        Err(error) => boxed_value(task_result_error(error.message)),
                    },
                    TaskWaitStatus::TimedOut => boxed_value(task_result_timed_out()),
                    TaskWaitStatus::Cancelled => boxed_value(task_result_cancelled()),
                }
            }
            other => runtime_error(format!(
                "expected `Task`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join_or_none(task: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let cancellation = current_cancellation();
        match unsafe { value_ref(task) } {
            Value::Task(task) => {
                if let Err(error) = task.claim_result_observation() {
                    runtime_diagnostic_error(error);
                }
                match if cancellation.is_cancelled() {
                    TaskWaitStatus::Cancelled
                } else if let Some(result) = task.completed_result_observed() {
                    match result {
                        crate::runtime_value::TaskExecutionResult::Ready(result) => {
                            TaskWaitStatus::Ready(result)
                        }
                        crate::runtime_value::TaskExecutionResult::Cancelled => {
                            TaskWaitStatus::Cancelled
                        }
                    }
                } else {
                    TaskWaitStatus::TimedOut
                } {
                    TaskWaitStatus::Ready(result) => match result {
                        Ok(value) => boxed_value(option_some(value)),
                        Err(_) => boxed_value(option_none()),
                    },
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => {
                        boxed_value(option_none())
                    }
                }
            }
            other => runtime_error(format!(
                "expected `Task`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join_or_none_timeout_value(
    task: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "result_or_none(timeout=...)");
        match unsafe { value_ref(task) } {
            Value::Task(task) => {
                if let Err(error) = task.claim_result_observation() {
                    runtime_diagnostic_error(error);
                }
                match direct_timer_result_or_trap(task.wait_result_with_cancellation_observed(
                    Some(timeout),
                    Some(&current_cancellation()),
                )) {
                    TaskWaitStatus::Ready(result) => match result {
                        Ok(value) => boxed_value(option_some(value)),
                        Err(_) => boxed_value(option_none()),
                    },
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => {
                        boxed_value(option_none())
                    }
                }
            }
            other => runtime_error(format!(
                "expected `Task`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join_or_value(
    task: *mut OpaqueValue,
    default: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let cancellation = current_cancellation();
        let default = unsafe { consume_owned_value(default) };
        match unsafe { value_ref(task) } {
            Value::Task(task) => {
                if let Err(error) = task.claim_result_observation() {
                    runtime_diagnostic_error(error);
                }
                match if cancellation.is_cancelled() {
                    TaskWaitStatus::Cancelled
                } else if let Some(result) = task.completed_result_observed() {
                    match result {
                        crate::runtime_value::TaskExecutionResult::Ready(result) => {
                            TaskWaitStatus::Ready(result)
                        }
                        crate::runtime_value::TaskExecutionResult::Cancelled => {
                            TaskWaitStatus::Cancelled
                        }
                    }
                } else {
                    TaskWaitStatus::TimedOut
                } {
                    TaskWaitStatus::Ready(result) => match result {
                        Ok(value) => boxed_value(value),
                        Err(_) => boxed_value(default),
                    },
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => boxed_value(default),
                }
            }
            other => runtime_error(format!(
                "expected `Task`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_join_or_value_timeout_value(
    task: *mut OpaqueValue,
    default: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let default = unsafe { consume_owned_value(default) };
        let timeout = duration_from_ptr(duration, "result_or(timeout=...)");
        match unsafe { value_ref(task) } {
            Value::Task(task) => {
                if let Err(error) = task.claim_result_observation() {
                    runtime_diagnostic_error(error);
                }
                match direct_timer_result_or_trap(task.wait_result_with_cancellation_observed(
                    Some(timeout),
                    Some(&current_cancellation()),
                )) {
                    TaskWaitStatus::Ready(result) => match result {
                        Ok(value) => boxed_value(value),
                        Err(_) => boxed_value(default),
                    },
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => boxed_value(default),
                }
            }
            other => runtime_error(format!(
                "expected `Task`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

fn expect_task_vec(value: &Value, context: &str) -> Vec<TaskValue> {
    match value {
        Value::Vec(vector) => vector
            .elements
            .iter()
            .map(|value| match value {
                Value::Task(task) => task.clone(),
                other => runtime_error(format!(
                    "expected `{}` tasks to be `Task`, found `{}`",
                    context,
                    value_type_name(other)
                )),
            })
            .collect(),
        other => runtime_error(format!(
            "expected `{}` to receive `list[Task]`, found `{}`",
            context,
            value_type_name(other)
        )),
    }
}

fn wait_any_tasks(
    tasks: Vec<TaskValue>,
    timeout: Option<StdDuration>,
) -> Result<Value, Diagnostic> {
    claim_task_result_observations(&tasks)?;
    let deadline = checked_timeout_deadline_at(timeout, Instant::now(), "wait_any(timeout=...)")?;
    let cancellation = current_cancellation();
    if tasks.is_empty() {
        return if poll_cancellation(&cancellation) {
            Ok(wait_any_cancelled())
        } else {
            Ok(wait_any_timed_out())
        };
    }
    loop {
        for (index, task) in tasks.iter().enumerate() {
            if let Some(result) = task.completed_result_observed() {
                let index = i64::try_from(index)
                    .map_err(|_| Diagnostic::new("wait_any result index exceeds int64 range"))?;
                return match result {
                    crate::runtime_value::TaskExecutionResult::Ready(result) => match result {
                        Ok(value) => Ok(wait_any_ready(index, value)),
                        Err(error) => Ok(wait_any_error(index, error.message)),
                    },
                    crate::runtime_value::TaskExecutionResult::Cancelled => {
                        Ok(wait_any_cancelled())
                    }
                };
            }
        }

        match wait_for_runtime_scheduler(
            Vec::new(),
            false,
            Vec::new(),
            tasks.clone(),
            deadline,
            Some(&cancellation),
        ) {
            RuntimeSchedulerWakeReason::Ready => {}
            RuntimeSchedulerWakeReason::TimedOut => return Ok(wait_any_timed_out()),
            RuntimeSchedulerWakeReason::Cancelled => return Ok(wait_any_cancelled()),
        }
    }
}

fn wait_all_tasks(
    tasks: Vec<TaskValue>,
    timeout: Option<StdDuration>,
) -> Result<Value, Diagnostic> {
    claim_task_result_observations(&tasks)?;
    let deadline = checked_timeout_deadline_at(timeout, Instant::now(), "wait_all(timeout=...)")?;
    let cancellation = current_cancellation();
    let mut results = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.into_iter().enumerate() {
        let remaining = deadline.and_then(|deadline| {
            deadline
                .checked_duration_since(Instant::now())
                .or(Some(StdDuration::from_millis(0)))
        });
        match direct_timer_result_or_trap(
            task.wait_result_with_cancellation_observed(remaining, Some(&cancellation)),
        ) {
            TaskWaitStatus::Ready(result) => match result {
                Ok(value) => results.push(value),
                Err(error) => {
                    let index = i64::try_from(index).map_err(|_| {
                        Diagnostic::new("wait_all result index exceeds int64 range")
                    })?;
                    return Ok(wait_all_error(index, error.message));
                }
            },
            TaskWaitStatus::TimedOut => return Ok(wait_all_timed_out()),
            TaskWaitStatus::Cancelled => return Ok(wait_all_cancelled()),
        }
    }
    Ok(wait_all_ready(results))
}

fn checked_timeout_deadline_at(
    timeout: Option<StdDuration>,
    now: Instant,
    label: &str,
) -> Result<Option<Instant>, Diagnostic> {
    timeout
        .map(|timeout| {
            now.checked_add(timeout).ok_or_else(|| {
                Diagnostic::coded("AU4001", format!("{label} exceeds the host deadline range"))
            })
        })
        .transpose()
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_wait_any(tasks: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        match wait_any_tasks(
            expect_task_vec(unsafe { &value_ref(tasks) }, "wait_any"),
            None,
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_wait_any_timeout_value(
    tasks: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "wait_any(timeout=...)");
        match wait_any_tasks(
            expect_task_vec(unsafe { &value_ref(tasks) }, "wait_any"),
            Some(timeout),
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_wait_all(tasks: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        match wait_all_tasks(
            expect_task_vec(unsafe { &value_ref(tasks) }, "wait_all"),
            None,
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_wait_all_timeout_value(
    tasks: *mut OpaqueValue,
    duration: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "wait_all(timeout=...)");
        match wait_all_tasks(
            expect_task_vec(unsafe { &value_ref(tasks) }, "wait_all"),
            Some(timeout),
        ) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

fn validate_direct_select_tuple_metadata(sources: &TupleValue) -> Result<(), Diagnostic> {
    if sources.element_types.len() != sources.elements.len() {
        return Err(Diagnostic::coded(
            "AU4001",
            format!(
                "direct `select` ABI tuple metadata has {} element types for {} source{}",
                sources.element_types.len(),
                sources.elements.len(),
                if sources.elements.len() == 1 { "" } else { "s" }
            ),
        ));
    }

    let mut queue_payload: Option<(usize, Type)> = None;
    let mut task_result: Option<(usize, Type)> = None;
    for (index, (declared, value)) in sources
        .element_types
        .iter()
        .zip(&sources.elements)
        .enumerate()
    {
        match value {
            Value::Channel(_) => {
                let Type::Named(name, type_args) = declared else {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                };
                let [payload] = type_args.as_slice() else {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                };
                if name != "Queue" {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                }
                if let Some((previous_index, previous)) = &queue_payload {
                    if previous != payload {
                        return Err(Diagnostic::coded(
                            "AU4001",
                            format!(
                                "direct `select` ABI Queue sources must share one payload type; \
                                 source {} uses `{}` but source {} uses `{}`",
                                previous_index, previous, index, payload
                            ),
                        ));
                    }
                } else {
                    queue_payload = Some((index, payload.clone()));
                }
            }
            Value::Task(_) => {
                let Type::Named(name, type_args) = declared else {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                };
                let [result] = type_args.as_slice() else {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                };
                if name != "Task" {
                    return Err(direct_select_metadata_kind_error(index, declared, value));
                }
                if let Some((previous_index, previous)) = &task_result {
                    if previous != result {
                        return Err(Diagnostic::coded(
                            "AU4001",
                            format!(
                                "direct `select` ABI Task sources must share one result type; \
                                 source {} uses `{}` but source {} uses `{}`",
                                previous_index, previous, index, result
                            ),
                        ));
                    }
                } else {
                    task_result = Some((index, result.clone()));
                }
            }
            Value::Duration(_)
                if matches!(
                    declared,
                    Type::Named(name, type_args)
                        if name == "Duration" && type_args.is_empty()
                ) => {}
            Value::Duration(_) => {
                return Err(direct_select_metadata_kind_error(index, declared, value));
            }
            // The shared primitive owns the canonical invalid-descriptor
            // diagnostic for values that are not select sources.
            _ => {}
        }
    }
    Ok(())
}

fn direct_select_metadata_kind_error(index: usize, declared: &Type, value: &Value) -> Diagnostic {
    Diagnostic::coded(
        "AU4001",
        format!(
            "direct `select` ABI source {} is tagged `{}` but contains `{}`",
            index,
            declared,
            value_type_name(value)
        ),
    )
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_select(sources: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let sources = unsafe { consume_owned_value(sources) };
        let Value::Tuple(sources) = sources else {
            runtime_diagnostic_error(Diagnostic::coded(
                "AU4001",
                format!(
                    "direct `select` ABI expected an owned tuple of Queue, Task, or Duration \
                     sources, found `{}`",
                    value_type_name(&sources)
                ),
            ));
        };
        if let Err(error) = validate_direct_select_tuple_metadata(&sources) {
            runtime_diagnostic_error(error);
        }
        match select_runtime_values(sources.elements, Some(&current_cancellation())) {
            Ok(value) => boxed_value(value),
            Err(error) => runtime_diagnostic_error(error),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_group_cancel(
    group: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(group) } {
        Value::TaskGroup(group) => {
            group.cancel();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `TaskGroup`, found `{}`",
            value_type_name(other)
        )),
    })
}

fn close_task_group(group: &TaskGroupValue, cancel_before: bool) {
    let tasks = group.drain_tasks();
    let cancellation = current_cancellation();
    let mut cancel_group = cancel_before;
    if !cancel_group && task_group_cleanup_should_cancel(&tasks, &cancellation) {
        cancel_group = true;
    }
    if cancel_group {
        group.cancel();
    }
    for task in tasks {
        match direct_timer_result_or_trap(
            task.wait_result_with_cancellation(None, Some(&cancellation)),
        ) {
            TaskWaitStatus::Ready(_) => {
                if let Some(error) = task.unobserved_error() {
                    runtime_diagnostic_error(error);
                }
            }
            TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => {}
        }
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_task_group_close(
    group: *mut OpaqueValue,
    cancel_before: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(group) } {
        Value::TaskGroup(group) => {
            close_task_group(&group, cancel_before != 0);
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `TaskGroup`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_close_value(
    value: *mut OpaqueValue,
    cancel_before: i64,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        match unsafe { value_ref(value) } {
            Value::Channel(channel) => channel.close(),
            Value::TaskGroup(group) => close_task_group(&group, cancel_before != 0),
            Value::File(file) => file.close(),
            Value::TcpListener(listener) => listener.close(),
            Value::TcpStream(stream) => stream.close(),
            Value::UdpSocket(socket) => socket.close(),
            Value::HttpListener(listener) => listener.close(),
            Value::WebSocket(socket) => {
                let _ = socket.close();
            }
            Value::ProcessChild(child) => child.close(),
            Value::ProcessPipe(pipe) => pipe.close(),
            Value::ProcessSupervisor(supervisor) => supervisor.close(),
            Value::UnixListener(listener) => listener.close(),
            Value::UnixStream(stream) => stream.close(),
            Value::TlsListener(listener) => listener.close(),
            Value::TlsStream(stream) => stream.close(),
            _ => {}
        }
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_io_write(text: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(text) } {
        Value::String(text) => match write_stdout_result(&text) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_io_flush() -> *mut OpaqueValue {
    task_runtime_boundary(|| match flush_stdout_result() {
        Ok(()) => boxed_value(result_ok(Value::Unit)),
        Err(error) => boxed_value(result_err(io_error(error))),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_io_read_line() -> *mut OpaqueValue {
    task_runtime_boundary(|| match io_read_line() {
        Ok(Some(line)) => boxed_value(result_ok(option_some(Value::String(line)))),
        Ok(None) => boxed_value(result_ok(option_none())),
        Err(error) => boxed_value(result_err(io_error(error))),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_exists(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => boxed_value(Value::Bool(std::path::Path::new(&path).exists())),
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_read_to_string(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match run_blocking_io(
            move || {
                let bytes = read_file_limited(&path, "fs.read_to_string")?;
                String::from_utf8(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            },
            Some(&current_cancellation()),
        ) {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_read_bytes(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match run_blocking_io(
            move || read_file_limited(&path, "fs.read_bytes"),
            Some(&current_cancellation()),
        ) {
            Ok(bytes) => boxed_value(result_ok(bytes_vec_value(bytes))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_write_string(
    path: *mut OpaqueValue,
    text: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let path = match unsafe { value_ref(path) } {
            Value::String(path) => path.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        let text = match unsafe { value_ref(text) } {
            Value::String(text) => text.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        match run_blocking_io(
            move || std::fs::write(path, text),
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_write_bytes(
    path: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let path = expect_string_value(&unsafe { value_ref(path) }, "fs.write_bytes(...)");
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "fs.write_bytes(...)");
        match run_blocking_io(
            move || std::fs::write(path, bytes),
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_append_string(
    path: *mut OpaqueValue,
    text: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let path = match unsafe { value_ref(path) } {
            Value::String(path) => path.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        let text = match unsafe { value_ref(text) } {
            Value::String(text) => text.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        match run_blocking_io(
            move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(text.as_bytes()))
            },
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_append_bytes(
    path: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let path = expect_string_value(&unsafe { value_ref(path) }, "fs.append_bytes(...)");
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "fs.append_bytes(...)");
        match run_blocking_io(
            move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(&bytes))
            },
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_create_dir(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match run_blocking_io(
            move || crate::runtime_value::create_dir_once(path),
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_read_dir(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match run_blocking_io(
            move || {
                let mut names = std::fs::read_dir(path)?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>();
                names.sort();
                Ok(names)
            },
            Some(&current_cancellation()),
        ) {
            Ok(names) => boxed_value(result_ok(Value::Vec(VecValue {
                element_type: Type::named("str"),
                elements: names.into_iter().map(Value::String).collect(),
            }))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_remove_file(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match run_blocking_io(
            move || crate::runtime_value::remove_file_checked(path),
            Some(&current_cancellation()),
        ) {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_open(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match FileValue::open(&path) {
            Ok(file) => boxed_value(result_ok(Value::File(file))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_create(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match FileValue::create(&path) {
            Ok(file) => boxed_value(result_ok(Value::File(file))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fs_append(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match FileValue::append(&path) {
            Ok(file) => boxed_value(result_ok(Value::File(file))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_read_all(file: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(file) } {
        Value::File(file) => match file.read_all() {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `fs.File`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_read_bytes(file: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(file) } {
        Value::File(file) => match file.read_bytes() {
            Ok(bytes) => boxed_value(result_ok(bytes_vec_value(bytes))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `fs.File`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_write_all(
    file: *mut OpaqueValue,
    text: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = match unsafe { value_ref(text) } {
            Value::String(text) => text.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        match unsafe { value_ref(file) } {
            Value::File(file) => match file.write_all(&text) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `fs.File`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_write_bytes(
    file: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "write_bytes(...)");
        match unsafe { value_ref(file) } {
            Value::File(file) => match file.write_bytes(&bytes) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `fs.File`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_flush(file: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(file) } {
        Value::File(file) => match file.flush() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `fs.File`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_file_close(file: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(file) } {
        Value::File(file) => {
            file.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `fs.File`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_inherit() -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(process_stdio_inherit()))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_null() -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(process_stdio_null()))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe() -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(process_stdio_pipe()))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor() -> *mut OpaqueValue {
    task_runtime_boundary(|| boxed_value(Value::ProcessSupervisor(ProcessSupervisorValue::new())))
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_start(
    command: *mut OpaqueValue,
    cwd: *mut OpaqueValue,
    env: *mut OpaqueValue,
    stdin: *mut OpaqueValue,
    stdout: *mut OpaqueValue,
    stderr: *mut OpaqueValue,
    group: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let command = expect_command_vec(&unsafe { value_ref(command) }, "process.start(...)");
        if command.is_empty() {
            return boxed_value(result_err(process_error_no_command()));
        }
        let cwd = expect_optional_string_value(&unsafe { value_ref(cwd) }, "process.start(...)");
        let env = expect_headers_map(&unsafe { value_ref(env) }, "process.start(...)");
        let stdin = decode_process_stdio(&unsafe { value_ref(stdin) }, "process.start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stdout = decode_process_stdio(&unsafe { value_ref(stdout) }, "process.start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stderr = decode_process_stdio(&unsafe { value_ref(stderr) }, "process.start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let group = expect_bool_value(&unsafe { value_ref(group) }, "process.start(...)");
        match ProcessChildValue::spawn(command, cwd, env, stdin, stdout, stderr, group) {
            Ok(child) => boxed_value(result_ok(Value::ProcessChild(child))),
            Err(error) => boxed_value(result_err(process_error_spawn(error.to_string()))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_run(
    command: *mut OpaqueValue,
    cwd: *mut OpaqueValue,
    env: *mut OpaqueValue,
    stdin: *mut OpaqueValue,
    stdout: *mut OpaqueValue,
    stderr: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
    group: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let command = expect_command_vec(&unsafe { value_ref(command) }, "process.run(...)");
        if command.is_empty() {
            return boxed_value(result_err(process_error_no_command()));
        }
        let cwd = expect_optional_string_value(&unsafe { value_ref(cwd) }, "process.run(...)");
        let env = expect_headers_map(&unsafe { value_ref(env) }, "process.run(...)");
        let stdin = decode_process_stdio(&unsafe { value_ref(stdin) }, "process.run(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stdout = decode_process_stdio(&unsafe { value_ref(stdout) }, "process.run(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stderr = decode_process_stdio(&unsafe { value_ref(stderr) }, "process.run(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let timeout = process_timeout_or_return!(timeout, "process.run(...)");
        let group = expect_bool_value(&unsafe { value_ref(group) }, "process.run(...)");

        let child = match ProcessChildValue::spawn(command, cwd, env, stdin, stdout, stderr, group)
        {
            Ok(child) => child,
            Err(error) => return boxed_value(result_err(process_error_spawn(error.to_string()))),
        };

        let cancellation = current_cancellation();
        let stdout_task = child
            .stdout()
            .map(|pipe| {
                let capture_cancellation = cancellation.clone();
                spawn_lightweight_task_with_cancellation(capture_cancellation.clone(), move || {
                    match pipe.read_all_bytes(Some(&capture_cancellation)) {
                        Ok(bytes) => Ok(bytes_vec_value(bytes)),
                        Err(error) => Err(Diagnostic::new(format!(
                            "process stdout capture failed: {}",
                            error
                        ))),
                    }
                })
            })
            .transpose()
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stderr_task = child
            .stderr()
            .map(|pipe| {
                let capture_cancellation = cancellation.clone();
                spawn_lightweight_task_with_cancellation(capture_cancellation.clone(), move || {
                    match pipe.read_all_bytes(Some(&capture_cancellation)) {
                        Ok(bytes) => Ok(bytes_vec_value(bytes)),
                        Err(error) => Err(Diagnostic::new(format!(
                            "process stderr capture failed: {}",
                            error
                        ))),
                    }
                })
            })
            .transpose()
            .unwrap_or_else(|error| runtime_diagnostic_error(error));

        let status = match child.wait(timeout, Some(&cancellation)) {
            ProcessChildWaitStatus::Exited(status) => status,
            ProcessChildWaitStatus::TimedOut => {
                child.close();
                return boxed_value(result_err(process_error_timed_out()));
            }
            ProcessChildWaitStatus::Cancelled => {
                child.close();
                return boxed_value(result_err(process_error_cancelled()));
            }
            ProcessChildWaitStatus::Failed(error) => {
                child.close();
                return boxed_value(result_err(process_error_from_io(error)));
            }
        };
        let stdout = await_process_capture_task(stdout_task, "stdout");
        let stderr = await_process_capture_task(stderr_task, "stderr");
        boxed_value(result_ok(Value::ProcessCompleted(
            ProcessCompletedValue::new(
                crate::runtime_value::process_exit_status(status),
                stdout,
                stderr,
            ),
        )))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_stdin(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => boxed_value(
            child
                .stdin()
                .map(Value::ProcessPipe)
                .map(option_some)
                .unwrap_or_else(option_none),
        ),
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_stdout(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => boxed_value(
            child
                .stdout()
                .map(Value::ProcessPipe)
                .map(option_some)
                .unwrap_or_else(option_none),
        ),
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_stderr(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => boxed_value(
            child
                .stderr()
                .map(Value::ProcessPipe)
                .map(option_some)
                .unwrap_or_else(option_none),
        ),
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_wait(
    child: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = match process_optional_timeout_result_from_ptr(timeout, "wait(timeout=...)") {
            Ok(timeout) => timeout,
            Err(error) => return boxed_value(process_wait_failed(process_error_from_io(error))),
        };
        match unsafe { value_ref(child) } {
            Value::ProcessChild(child) => {
                boxed_value(match child.wait(timeout, Some(&current_cancellation())) {
                    ProcessChildWaitStatus::Exited(status) => process_wait_exited(status),
                    ProcessChildWaitStatus::TimedOut => process_wait_timed_out(),
                    ProcessChildWaitStatus::Cancelled => process_wait_cancelled(),
                    ProcessChildWaitStatus::Failed(error) => {
                        process_wait_failed(process_error_from_io(error))
                    }
                })
            }
            other => runtime_error(format!(
                "expected `process.Child`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_wait_or_none(
    child: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = process_timeout_or_return!(timeout, "wait_or_none(timeout=...)");
        match unsafe { value_ref(child) } {
            Value::ProcessChild(child) => {
                match child.wait_or_none(timeout, Some(&current_cancellation())) {
                    Ok(Some(status)) => {
                        boxed_value(result_ok(option_some(process_exit_status(status))))
                    }
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(error)),
                }
            }
            other => runtime_error(format!(
                "expected `process.Child`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_wait_ok(
    child: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = process_timeout_or_return!(timeout, "wait_ok(timeout=...)");
        match unsafe { value_ref(child) } {
            Value::ProcessChild(child) => {
                match child.wait_ok(timeout, Some(&current_cancellation())) {
                    Ok(status) => boxed_value(result_ok(process_exit_status(status))),
                    Err(error) => boxed_value(result_err(error)),
                }
            }
            other => runtime_error(format!(
                "expected `process.Child`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_kill(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => match child.kill() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(process_error_from_io(error))),
        },
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_terminate(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => match child.terminate() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(process_error_from_io(error))),
        },
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_child_close(
    child: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(child) } {
        Value::ProcessChild(child) => {
            child.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `process.Child`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_read_all(
    pipe: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(pipe) } {
        Value::ProcessPipe(pipe) => match pipe.read_all(Some(&current_cancellation())) {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(process_error_from_io(error))),
        },
        other => runtime_error(format!(
            "expected `process.Pipe`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_read_line(
    pipe: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = process_timeout_or_return!(timeout, "read_line(timeout=...)");
        match unsafe { value_ref(pipe) } {
            Value::ProcessPipe(pipe) => {
                match pipe.read_line(timeout, Some(&current_cancellation())) {
                    Ok(Some(text)) => boxed_value(result_ok(option_some(Value::String(text)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(process_error_from_io(error))),
                }
            }
            other => runtime_error(format!(
                "expected `process.Pipe`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_read_bytes(
    pipe: *mut OpaqueValue,
    max_bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let count = expect_i32_value(&unsafe { value_ref(max_bytes) }, "read_bytes(...)");
        let count = usize::try_from(count).unwrap_or_else(|_| {
            runtime_error("`read_bytes(...)` expects a non-negative `max_bytes`")
        });
        let timeout = process_timeout_or_return!(timeout, "read_bytes(timeout=...)");
        match unsafe { value_ref(pipe) } {
            Value::ProcessPipe(pipe) => {
                match pipe.read_bytes(count, timeout, Some(&current_cancellation())) {
                    Ok(Some(bytes)) => boxed_value(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(process_error_from_io(error))),
                }
            }
            other => runtime_error(format!(
                "expected `process.Pipe`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_write_all(
    pipe: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = expect_string_value(&unsafe { value_ref(text) }, "write_all(...)");
        let timeout = process_timeout_or_return!(timeout, "write_all(timeout=...)");
        match unsafe { value_ref(pipe) } {
            Value::ProcessPipe(pipe) => {
                match pipe.write_all(&text, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(process_error_from_io(error))),
                }
            }
            other => runtime_error(format!(
                "expected `process.Pipe`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_write_bytes(
    pipe: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "write_bytes(...)");
        let timeout = process_timeout_or_return!(timeout, "write_bytes(timeout=...)");
        match unsafe { value_ref(pipe) } {
            Value::ProcessPipe(pipe) => {
                match pipe.write_bytes(&bytes, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(process_error_from_io(error))),
                }
            }
            other => runtime_error(format!(
                "expected `process.Pipe`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_flush(
    pipe: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(pipe) } {
        Value::ProcessPipe(pipe) => match pipe.flush() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(process_error_from_io(error))),
        },
        other => runtime_error(format!(
            "expected `process.Pipe`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_pipe_close(
    pipe: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(pipe) } {
        Value::ProcessPipe(pipe) => {
            pipe.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `process.Pipe`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_status(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => boxed_value(completed.status()),
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_success(completed: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => i64::from(completed.success()),
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_stdout(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => match completed.stdout() {
            Ok(stdout) => boxed_value(Value::String(stdout)),
            Err(error) => runtime_diagnostic_error(Diagnostic::coded("AU4005", error.to_string())),
        },
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_stderr(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => match completed.stderr() {
            Ok(stderr) => boxed_value(Value::String(stderr)),
            Err(error) => runtime_diagnostic_error(Diagnostic::coded("AU4005", error.to_string())),
        },
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_stdout_bytes(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => {
            boxed_value(bytes_vec_value(completed.stdout_bytes()))
        }
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_stderr_bytes(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => {
            boxed_value(bytes_vec_value(completed.stderr_bytes()))
        }
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_completed_check(
    completed: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(completed) } {
        Value::ProcessCompleted(completed) => match completed.check() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(error)),
        },
        other => runtime_error(format!(
            "expected `process.Completed`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_start(
    supervisor: *mut OpaqueValue,
    name: *mut OpaqueValue,
    command: *mut OpaqueValue,
    cwd: *mut OpaqueValue,
    env: *mut OpaqueValue,
    stdin: *mut OpaqueValue,
    stdout: *mut OpaqueValue,
    stderr: *mut OpaqueValue,
    restart: *mut OpaqueValue,
    backoff: *mut OpaqueValue,
    max_restarts: *mut OpaqueValue,
    group: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let name = unsafe { consume_owned_value(name) };
        let command = unsafe { consume_owned_value(command) };
        let cwd = unsafe { consume_owned_value(cwd) };
        let env = unsafe { consume_owned_value(env) };
        let stdin = unsafe { consume_owned_value(stdin) };
        let stdout = unsafe { consume_owned_value(stdout) };
        let stderr = unsafe { consume_owned_value(stderr) };
        let restart = unsafe { consume_owned_value(restart) };
        let backoff = unsafe { consume_owned_value(backoff) };
        let max_restarts = unsafe { consume_owned_value(max_restarts) };
        let group = unsafe { consume_owned_value(group) };

        let name = expect_string_value(&name, "start(...)");
        let command = expect_command_vec(&command, "start(...)");
        let cwd = expect_optional_string_value(&cwd, "start(...)");
        let env = expect_headers_map(&env, "start(...)");
        let stdin = decode_process_stdio(&stdin, "start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stdout = decode_process_stdio(&stdout, "start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let stderr = decode_process_stdio(&stderr, "start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let restart = decode_process_restart_policy(&restart, "start(...)")
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        let backoff = match duration_result_from_value(&backoff, "start(...)") {
            Ok(backoff) => backoff,
            Err(error) => return boxed_value(result_err(process_error_from_io(error))),
        };
        let max_restarts = supervisor_max_restarts_from_value(&max_restarts, "start(...)");
        let group = expect_bool_value(&group, "start(...)");
        match unsafe { value_ref(supervisor) } {
            Value::ProcessSupervisor(supervisor) => match supervisor.start(
                name,
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
            ) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(error)),
            },
            other => runtime_error(format!(
                "expected `process.Supervisor`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_wait(
    supervisor: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = match process_optional_timeout_result_from_ptr(timeout, "wait(timeout=...)") {
            Ok(timeout) => timeout,
            Err(error) => {
                return boxed_value(process_supervisor_wait_event(
                    process_supervisor_event_failed(
                        "<supervisor>".to_string(),
                        process_error_from_io(error),
                        IntegerValue::from_signed(0),
                    ),
                ));
            }
        };
        match unsafe { value_ref(supervisor) } {
            Value::ProcessSupervisor(supervisor) => boxed_value(
                match supervisor.wait(timeout, Some(&current_cancellation())) {
                    ProcessSupervisorWaitStatus::Event(event) => {
                        process_supervisor_wait_event(event)
                    }
                    ProcessSupervisorWaitStatus::TimedOut => process_supervisor_wait_timed_out(),
                    ProcessSupervisorWaitStatus::Cancelled => process_supervisor_wait_cancelled(),
                },
            ),
            other => runtime_error(format!(
                "expected `process.Supervisor`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_wait_or_none(
    supervisor: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = process_timeout_or_return!(timeout, "wait_or_none(timeout=...)");
        match unsafe { value_ref(supervisor) } {
            Value::ProcessSupervisor(supervisor) => {
                match supervisor.wait_or_none(timeout, Some(&current_cancellation())) {
                    Ok(Some(event)) => boxed_value(result_ok(option_some(event))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(error)),
                }
            }
            other => runtime_error(format!(
                "expected `process.Supervisor`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_stop(
    supervisor: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(supervisor) } {
        Value::ProcessSupervisor(supervisor) => match supervisor.stop() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(error)),
        },
        other => runtime_error(format!(
            "expected `process.Supervisor`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_is_empty(
    supervisor: *mut OpaqueValue,
) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(supervisor) } {
        Value::ProcessSupervisor(supervisor) => i64::from(supervisor.is_empty()),
        other => runtime_error(format!(
            "expected `process.Supervisor`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_process_supervisor_close(
    supervisor: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(supervisor) } {
        Value::ProcessSupervisor(supervisor) => {
            supervisor.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `process.Supervisor`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_connect(address: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(address) } {
        Value::String(address) => {
            match TcpStreamValue::connect(&address, None, Some(&current_cancellation())) {
                Ok(stream) => boxed_value(result_ok(Value::TcpStream(stream))),
                Err(error) => boxed_value(result_err(io_error(error))),
            }
        }
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_connect_timeout(
    address: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "net.connect_timeout(...)");
        match unsafe { value_ref(address) } {
            Value::String(address) => {
                match TcpStreamValue::connect(&address, timeout, Some(&current_cancellation())) {
                    Ok(stream) => boxed_value(result_ok(Value::TcpStream(stream))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_listen(address: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(address) } {
        Value::String(address) => match TcpListenerValue::bind(&address) {
            Ok(listener) => boxed_value(result_ok(Value::TcpListener(listener))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_udp_bind(address: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(address) } {
        Value::String(address) => match UdpSocketValue::bind(&address) {
            Ok(socket) => boxed_value(result_ok(Value::UdpSocket(socket))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_unix_listen(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => match UnixListenerValue::bind(&path) {
            Ok(listener) => boxed_value(result_ok(Value::UnixListener(listener))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_unix_connect(path: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(path) } {
        Value::String(path) => {
            match UnixStreamValue::connect(&path, None, Some(&current_cancellation())) {
                Ok(stream) => boxed_value(result_ok(Value::UnixStream(stream))),
                Err(error) => boxed_value(result_err(io_error(error))),
            }
        }
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_unix_connect_timeout(
    path: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "net.unix_connect_timeout(...)");
        match unsafe { value_ref(path) } {
            Value::String(path) => {
                match UnixStreamValue::connect(&path, timeout, Some(&current_cancellation())) {
                    Ok(stream) => boxed_value(result_ok(Value::UnixStream(stream))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_tls_listen(
    address: *mut OpaqueValue,
    cert_pem_path: *mut OpaqueValue,
    key_pem_path: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let address = expect_string_value(&unsafe { value_ref(address) }, "net.tls_listen(...)");
        let cert_pem_path =
            expect_string_value(&unsafe { value_ref(cert_pem_path) }, "net.tls_listen(...)");
        let key_pem_path =
            expect_string_value(&unsafe { value_ref(key_pem_path) }, "net.tls_listen(...)");
        match TlsListenerValue::bind(&address, &cert_pem_path, &key_pem_path) {
            Ok(listener) => boxed_value(result_ok(Value::TlsListener(listener))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_tls_connect(
    address: *mut OpaqueValue,
    server_name: *mut OpaqueValue,
    ca_pem_path: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let address = expect_string_value(&unsafe { value_ref(address) }, "net.tls_connect(...)");
        let server_name =
            expect_string_value(&unsafe { value_ref(server_name) }, "net.tls_connect(...)");
        let ca_pem_path =
            expect_string_value(&unsafe { value_ref(ca_pem_path) }, "net.tls_connect(...)");
        match TlsStreamValue::connect(
            &address,
            &server_name,
            Some(&ca_pem_path),
            None,
            Some(&current_cancellation()),
        ) {
            Ok(stream) => boxed_value(result_ok(Value::TlsStream(stream))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_tls_connect_timeout(
    address: *mut OpaqueValue,
    server_name: *mut OpaqueValue,
    ca_pem_path: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let address = expect_string_value(
            &unsafe { value_ref(address) },
            "net.tls_connect_timeout(...)",
        );
        let server_name = expect_string_value(
            &unsafe { value_ref(server_name) },
            "net.tls_connect_timeout(...)",
        );
        let ca_pem_path = expect_string_value(
            &unsafe { value_ref(ca_pem_path) },
            "net.tls_connect_timeout(...)",
        );
        let timeout = io_timeout_or_return!(timeout, "net.tls_connect_timeout(...)");
        match TlsStreamValue::connect(
            &address,
            &server_name,
            Some(&ca_pem_path),
            timeout,
            Some(&current_cancellation()),
        ) {
            Ok(stream) => boxed_value(result_ok(Value::TlsStream(stream))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_http_listen(
    address: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(address) } {
        Value::String(address) => match HttpListenerValue::bind(&address) {
            Ok(listener) => boxed_value(result_ok(Value::HttpListener(listener))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_http_request_text(
    method: *mut OpaqueValue,
    url: *mut OpaqueValue,
    body: *mut OpaqueValue,
    headers: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let method =
            expect_string_value(&unsafe { value_ref(method) }, "net.http_request_text(...)");
        let url = expect_string_value(&unsafe { value_ref(url) }, "net.http_request_text(...)");
        let body = expect_string_value(&unsafe { value_ref(body) }, "net.http_request_text(...)");
        let headers =
            expect_headers_map(&unsafe { value_ref(headers) }, "net.http_request_text(...)");
        match HttpResponseValue::request_text(
            &method,
            &url,
            &body,
            headers,
            None,
            Some(&current_cancellation()),
        ) {
            Ok(response) => boxed_value(result_ok(Value::HttpResponse(response))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_http_request_text_timeout(
    method: *mut OpaqueValue,
    url: *mut OpaqueValue,
    body: *mut OpaqueValue,
    headers: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let method = expect_string_value(
            &unsafe { value_ref(method) },
            "net.http_request_text_timeout(...)",
        );
        let url = expect_string_value(
            &unsafe { value_ref(url) },
            "net.http_request_text_timeout(...)",
        );
        let body = expect_string_value(
            &unsafe { value_ref(body) },
            "net.http_request_text_timeout(...)",
        );
        let headers = expect_headers_map(
            &unsafe { value_ref(headers) },
            "net.http_request_text_timeout(...)",
        );
        let timeout = io_timeout_or_return!(timeout, "net.http_request_text_timeout(...)");
        match HttpResponseValue::request_text(
            &method,
            &url,
            &body,
            headers,
            timeout,
            Some(&current_cancellation()),
        ) {
            Ok(response) => boxed_value(result_ok(Value::HttpResponse(response))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_http_request_bytes(
    method: *mut OpaqueValue,
    url: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    headers: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let method =
            expect_string_value(&unsafe { value_ref(method) }, "net.http_request_bytes(...)");
        let url = expect_string_value(&unsafe { value_ref(url) }, "net.http_request_bytes(...)");
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "net.http_request_bytes(...)");
        let headers = expect_headers_map(
            &unsafe { value_ref(headers) },
            "net.http_request_bytes(...)",
        );
        match HttpResponseValue::request_bytes(
            &method,
            &url,
            &bytes,
            headers,
            None,
            Some(&current_cancellation()),
        ) {
            Ok(response) => boxed_value(result_ok(Value::HttpResponse(response))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_http_request_bytes_timeout(
    method: *mut OpaqueValue,
    url: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    headers: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let method = expect_string_value(
            &unsafe { value_ref(method) },
            "net.http_request_bytes_timeout(...)",
        );
        let url = expect_string_value(
            &unsafe { value_ref(url) },
            "net.http_request_bytes_timeout(...)",
        );
        let bytes = expect_bytes_value(
            &unsafe { value_ref(bytes) },
            "net.http_request_bytes_timeout(...)",
        );
        let headers = expect_headers_map(
            &unsafe { value_ref(headers) },
            "net.http_request_bytes_timeout(...)",
        );
        let timeout = io_timeout_or_return!(timeout, "net.http_request_bytes_timeout(...)");
        match HttpResponseValue::request_bytes(
            &method,
            &url,
            &bytes,
            headers,
            timeout,
            Some(&current_cancellation()),
        ) {
            Ok(response) => boxed_value(result_ok(Value::HttpResponse(response))),
            Err(error) => boxed_value(result_err(io_error(error))),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_websocket_listen(
    address: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(address) } {
        Value::String(address) => match WebSocketListenerValue::bind(&address) {
            Ok(listener) => boxed_value(result_ok(Value::WebSocketListener(listener))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_websocket_connect(
    url: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(url) } {
        Value::String(url) => match WebSocketValue::connect(&url, None) {
            Ok(socket) => boxed_value(result_ok(Value::WebSocket(socket))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `str`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_net_websocket_connect_timeout(
    url: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "net.websocket_connect_timeout(...)");
        match unsafe { value_ref(url) } {
            Value::String(url) => match WebSocketValue::connect(&url, timeout) {
                Ok(socket) => boxed_value(result_ok(Value::WebSocket(socket))),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_listener_accept(
    listener: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "accept(timeout=...)");
        match unsafe { value_ref(listener) } {
            Value::TcpListener(listener) => {
                match listener.accept(timeout, Some(&current_cancellation())) {
                    Ok(stream) => boxed_value(result_ok(Value::TcpStream(stream))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpListener`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_listener_local_addr(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::TcpListener(listener) => match listener.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_listener_close(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::TcpListener(listener) => {
            listener.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.TcpListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_read_all(
    stream: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "read_all(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.read_all(timeout, Some(&current_cancellation())) {
                    Ok(text) => boxed_value(result_ok(Value::String(text))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_read_line(
    stream: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "read_line(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.read_line(timeout, Some(&current_cancellation())) {
                    Ok(Some(line)) => boxed_value(result_ok(option_some(Value::String(line)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_read_bytes(
    stream: *mut OpaqueValue,
    max_bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let max_bytes = expect_i32_value(&unsafe { value_ref(max_bytes) }, "read_bytes(...)");
        let max_bytes = usize::try_from(max_bytes).unwrap_or_else(|_| {
            runtime_error("`read_bytes(...)` requires a non-negative max_bytes")
        });
        let timeout = io_timeout_or_return!(timeout, "read_bytes(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.read_bytes(max_bytes, timeout, Some(&current_cancellation())) {
                    Ok(Some(bytes)) => boxed_value(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_read_exact(
    stream: *mut OpaqueValue,
    count: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let count = expect_i32_value(&unsafe { value_ref(count) }, "read_exact(...)");
        let count = usize::try_from(count)
            .unwrap_or_else(|_| runtime_error("`read_exact(...)` requires a non-negative count"));
        let timeout = io_timeout_or_return!(timeout, "read_exact(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.read_exact(count, timeout, Some(&current_cancellation())) {
                    Ok(bytes) => boxed_value(result_ok(bytes_vec_value(bytes))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_write_all(
    stream: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = match unsafe { value_ref(text) } {
            Value::String(text) => text.clone(),
            other => runtime_error(format!(
                "expected `str`, found `{}`",
                value_type_name(other)
            )),
        };
        let timeout = io_timeout_or_return!(timeout, "write_all(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.write_all(&text, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_write_bytes(
    stream: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "write_bytes(...)");
        let timeout = io_timeout_or_return!(timeout, "write_bytes(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TcpStream(stream) => {
                match stream.write_bytes(&bytes, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TcpStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_shutdown_read(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.shutdown_read() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_shutdown_write(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.shutdown_write() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_shutdown_both(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.shutdown_both() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_flush(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.flush() {
            Ok(()) => boxed_value(result_ok(Value::Unit)),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_local_addr(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_peer_addr(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => match stream.peer_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tcp_stream_close(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TcpStream(stream) => {
            stream.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.TcpStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_send_text(
    socket: *mut OpaqueValue,
    address: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let address = expect_string_value(&unsafe { value_ref(address) }, "send_text(...)");
        let text = expect_string_value(&unsafe { value_ref(text) }, "send_text(...)");
        let timeout = io_timeout_or_return!(timeout, "send_text(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::UdpSocket(socket) => {
                match socket.send_to_text(&address, &text, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UdpSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_send_bytes(
    socket: *mut OpaqueValue,
    address: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let address = expect_string_value(&unsafe { value_ref(address) }, "send_bytes(...)");
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "send_bytes(...)");
        let timeout = io_timeout_or_return!(timeout, "send_bytes(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::UdpSocket(socket) => {
                match socket.send_to_bytes(&address, &bytes, timeout, Some(&current_cancellation()))
                {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UdpSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_recv(
    socket: *mut OpaqueValue,
    max_bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let max_bytes = expect_i32_value(&unsafe { value_ref(max_bytes) }, "recv(...)");
        let max_bytes = usize::try_from(max_bytes)
            .unwrap_or_else(|_| runtime_error("`recv(...)` requires a non-negative max_bytes"));
        let timeout = io_timeout_or_return!(timeout, "recv(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::UdpSocket(socket) => {
                match socket.recv(max_bytes, timeout, Some(&current_cancellation())) {
                    Ok(Some(bytes)) => boxed_value(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UdpSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_recv_from(
    socket: *mut OpaqueValue,
    max_bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let max_bytes = expect_i32_value(&unsafe { value_ref(max_bytes) }, "recv_from(...)");
        let max_bytes = usize::try_from(max_bytes).unwrap_or_else(|_| {
            runtime_error("`recv_from(...)` requires a non-negative max_bytes")
        });
        let timeout = io_timeout_or_return!(timeout, "recv_from(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::UdpSocket(socket) => {
                match socket.recv_from(max_bytes, timeout, Some(&current_cancellation())) {
                    Ok(Some(datagram)) => {
                        boxed_value(result_ok(option_some(Value::UdpDatagram(datagram))))
                    }
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UdpSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_local_addr(
    socket: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(socket) } {
        Value::UdpSocket(socket) => match socket.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.UdpSocket`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_peer_addr(
    socket: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(socket) } {
        Value::UdpSocket(socket) => match socket.peer_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.UdpSocket`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_socket_close(
    socket: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(socket) } {
        Value::UdpSocket(socket) => {
            socket.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.UdpSocket`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_datagram_address(
    datagram: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(datagram) } {
        Value::UdpDatagram(datagram) => boxed_value(Value::String(datagram.address())),
        other => runtime_error(format!(
            "expected `net.UdpDatagram`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_datagram_bytes(
    datagram: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(datagram) } {
        Value::UdpDatagram(datagram) => boxed_value(bytes_vec_value(datagram.bytes())),
        other => runtime_error(format!(
            "expected `net.UdpDatagram`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_udp_datagram_text(
    datagram: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(datagram) } {
        Value::UdpDatagram(datagram) => match datagram.text() {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.UdpDatagram`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_listener_accept(
    listener: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "accept(timeout=...)");
        match unsafe { value_ref(listener) } {
            Value::HttpListener(listener) => {
                match listener.accept(timeout, Some(&current_cancellation())) {
                    Ok(exchange) => boxed_value(result_ok(Value::HttpExchange(exchange))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.HttpListener`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_listener_local_addr(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::HttpListener(listener) => match listener.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.HttpListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_listener_close(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::HttpListener(listener) => {
            listener.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.HttpListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_method(
    exchange: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(exchange) } {
        Value::HttpExchange(exchange) => boxed_value(Value::String(exchange.method())),
        other => runtime_error(format!(
            "expected `net.HttpExchange`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_path(
    exchange: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(exchange) } {
        Value::HttpExchange(exchange) => boxed_value(Value::String(exchange.path())),
        other => runtime_error(format!(
            "expected `net.HttpExchange`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_headers(
    exchange: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(exchange) } {
        Value::HttpExchange(exchange) => boxed_value(headers_map_value(exchange.headers())),
        other => runtime_error(format!(
            "expected `net.HttpExchange`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_body_text(
    exchange: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(exchange) } {
        Value::HttpExchange(exchange) => match exchange.body_text() {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.HttpExchange`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_body_bytes(
    exchange: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(exchange) } {
        Value::HttpExchange(exchange) => boxed_value(bytes_vec_value(exchange.body_bytes())),
        other => runtime_error(format!(
            "expected `net.HttpExchange`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_respond_text(
    exchange: *mut OpaqueValue,
    status: *mut OpaqueValue,
    text: *mut OpaqueValue,
    headers: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = unsafe { consume_owned_value(text) };
        let headers = unsafe { consume_owned_value(headers) };
        let status = expect_i32_value(&unsafe { value_ref(status) }, "respond_text(...)");
        let text = expect_string_value(&text, "respond_text(...)");
        let headers = expect_headers_map(&headers, "respond_text(...)");
        match unsafe { value_ref(exchange) } {
            Value::HttpExchange(exchange) => match exchange.respond_text(status, &text, headers) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.HttpExchange`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_exchange_respond_bytes(
    exchange: *mut OpaqueValue,
    status: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    headers: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let bytes = unsafe { consume_owned_value(bytes) };
        let headers = unsafe { consume_owned_value(headers) };
        let status = expect_i32_value(&unsafe { value_ref(status) }, "respond_bytes(...)");
        let bytes = expect_bytes_value(&bytes, "respond_bytes(...)");
        let headers = expect_headers_map(&headers, "respond_bytes(...)");
        match unsafe { value_ref(exchange) } {
            Value::HttpExchange(exchange) => {
                match exchange.respond_bytes(status, &bytes, headers) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.HttpExchange`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_response_status(response: *mut OpaqueValue) -> i64 {
    task_runtime_boundary(|| match unsafe { value_ref(response) } {
        Value::HttpResponse(response) => i64::from(response.status()),
        other => runtime_error(format!(
            "expected `net.HttpResponse`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_response_reason(
    response: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(response) } {
        Value::HttpResponse(response) => boxed_value(Value::String(response.reason())),
        other => runtime_error(format!(
            "expected `net.HttpResponse`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_response_headers(
    response: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(response) } {
        Value::HttpResponse(response) => boxed_value(headers_map_value(response.headers())),
        other => runtime_error(format!(
            "expected `net.HttpResponse`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_response_text(
    response: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(response) } {
        Value::HttpResponse(response) => match response.text() {
            Ok(text) => boxed_value(result_ok(Value::String(text))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.HttpResponse`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_http_response_bytes(
    response: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(response) } {
        Value::HttpResponse(response) => boxed_value(bytes_vec_value(response.bytes())),
        other => runtime_error(format!(
            "expected `net.HttpResponse`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_listener_accept(
    listener: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "accept(timeout=...)");
        match unsafe { value_ref(listener) } {
            Value::WebSocketListener(listener) => match listener.accept(timeout) {
                Ok(socket) => boxed_value(result_ok(Value::WebSocket(socket))),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.WebSocketListener`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_listener_local_addr(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::WebSocketListener(listener) => match listener.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.WebSocketListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_send_text(
    socket: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = expect_string_value(&unsafe { value_ref(text) }, "send_text(...)");
        let timeout = io_timeout_or_return!(timeout, "send_text(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::WebSocket(socket) => match socket.send_text(&text, timeout) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.WebSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_send_bytes(
    socket: *mut OpaqueValue,
    bytes: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let bytes = expect_bytes_value(&unsafe { value_ref(bytes) }, "send_bytes(...)");
        let timeout = io_timeout_or_return!(timeout, "send_bytes(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::WebSocket(socket) => match socket.send_bytes(&bytes, timeout) {
                Ok(()) => boxed_value(result_ok(Value::Unit)),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.WebSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_recv_text(
    socket: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "recv_text(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::WebSocket(socket) => match socket.recv_text(timeout) {
                Ok(Some(text)) => boxed_value(result_ok(option_some(Value::String(text)))),
                Ok(None) => boxed_value(result_ok(option_none())),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.WebSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_recv_bytes(
    socket: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "recv_bytes(timeout=...)");
        match unsafe { value_ref(socket) } {
            Value::WebSocket(socket) => match socket.recv_bytes(timeout) {
                Ok(Some(bytes)) => boxed_value(result_ok(option_some(bytes_vec_value(bytes)))),
                Ok(None) => boxed_value(result_ok(option_none())),
                Err(error) => boxed_value(result_err(io_error(error))),
            },
            other => runtime_error(format!(
                "expected `net.WebSocket`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_websocket_close(socket: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(socket) } {
        Value::WebSocket(socket) => {
            let _ = socket.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.WebSocket`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_listener_accept(
    listener: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "accept(timeout=...)");
        match unsafe { value_ref(listener) } {
            Value::UnixListener(listener) => {
                match listener.accept(timeout, Some(&current_cancellation())) {
                    Ok(stream) => boxed_value(result_ok(Value::UnixStream(stream))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UnixListener`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_listener_close(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::UnixListener(listener) => {
            listener.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.UnixListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_stream_read_line(
    stream: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "read_line(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::UnixStream(stream) => {
                match stream.read_line(timeout, Some(&current_cancellation())) {
                    Ok(Some(text)) => boxed_value(result_ok(option_some(Value::String(text)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UnixStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_stream_read_exact(
    stream: *mut OpaqueValue,
    count: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let count = expect_i32_value(&unsafe { value_ref(count) }, "read_exact(...)");
        let count = usize::try_from(count)
            .unwrap_or_else(|_| runtime_error("`read_exact(...)` requires a non-negative count"));
        let timeout = io_timeout_or_return!(timeout, "read_exact(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::UnixStream(stream) => {
                match stream.read_exact(count, timeout, Some(&current_cancellation())) {
                    Ok(bytes) => boxed_value(result_ok(bytes_vec_value(bytes))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UnixStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_stream_write_all(
    stream: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = expect_string_value(&unsafe { value_ref(text) }, "write_all(...)");
        let timeout = io_timeout_or_return!(timeout, "write_all(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::UnixStream(stream) => {
                match stream.write_all(&text, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.UnixStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_unix_stream_close(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::UnixStream(stream) => {
            stream.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.UnixStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_listener_accept(
    listener: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "accept(timeout=...)");
        match unsafe { value_ref(listener) } {
            Value::TlsListener(listener) => {
                match listener.accept(timeout, Some(&current_cancellation())) {
                    Ok(stream) => boxed_value(result_ok(Value::TlsStream(stream))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TlsListener`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_listener_local_addr(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::TlsListener(listener) => match listener.local_addr() {
            Ok(address) => boxed_value(result_ok(Value::String(address))),
            Err(error) => boxed_value(result_err(io_error(error))),
        },
        other => runtime_error(format!(
            "expected `net.TlsListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_listener_close(
    listener: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(listener) } {
        Value::TlsListener(listener) => {
            listener.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.TlsListener`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_stream_read_line(
    stream: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = io_timeout_or_return!(timeout, "read_line(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TlsStream(stream) => {
                match stream.read_line(timeout, Some(&current_cancellation())) {
                    Ok(Some(text)) => boxed_value(result_ok(option_some(Value::String(text)))),
                    Ok(None) => boxed_value(result_ok(option_none())),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TlsStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_stream_read_exact(
    stream: *mut OpaqueValue,
    count: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let count = expect_i32_value(&unsafe { value_ref(count) }, "read_exact(...)");
        let count = usize::try_from(count)
            .unwrap_or_else(|_| runtime_error("`read_exact(...)` requires a non-negative count"));
        let timeout = io_timeout_or_return!(timeout, "read_exact(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TlsStream(stream) => {
                match stream.read_exact(count, timeout, Some(&current_cancellation())) {
                    Ok(bytes) => boxed_value(result_ok(bytes_vec_value(bytes))),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TlsStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_stream_write_all(
    stream: *mut OpaqueValue,
    text: *mut OpaqueValue,
    timeout: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let text = expect_string_value(&unsafe { value_ref(text) }, "write_all(...)");
        let timeout = io_timeout_or_return!(timeout, "write_all(timeout=...)");
        match unsafe { value_ref(stream) } {
            Value::TlsStream(stream) => {
                match stream.write_all(&text, timeout, Some(&current_cancellation())) {
                    Ok(()) => boxed_value(result_ok(Value::Unit)),
                    Err(error) => boxed_value(result_err(io_error(error))),
                }
            }
            other => runtime_error(format!(
                "expected `net.TlsStream`, found `{}`",
                value_type_name(other)
            )),
        }
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_tls_stream_close(
    stream: *mut OpaqueValue,
) -> *mut OpaqueValue {
    task_runtime_boundary(|| match unsafe { value_ref(stream) } {
        Value::TlsStream(stream) => {
            stream.close();
            boxed_value(Value::Unit)
        }
        other => runtime_error(format!(
            "expected `net.TlsStream`, found `{}`",
            value_type_name(other)
        )),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_sleep_ms(duration: i64) {
    task_runtime_boundary(|| {
        checked_sleep_milliseconds_with(duration, |timeout| {
            sleep_with_runtime_scheduler(timeout, Some(&current_cancellation()))
        })
        .unwrap_or_else(|error| runtime_diagnostic_error(error));
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_sleep_value(duration: *mut OpaqueValue) -> *mut OpaqueValue {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "sleep(...)");
        checked_sleep_with_runtime_scheduler(timeout)
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
        boxed_value(Value::Unit)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_sleep_value_void(duration: *mut OpaqueValue) {
    task_runtime_boundary(|| {
        let timeout = duration_from_ptr(duration, "sleep(...)");
        checked_sleep_with_runtime_scheduler(timeout)
            .unwrap_or_else(|error| runtime_diagnostic_error(error));
    })
}

static DIRECT_MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_monotonic_time_ms() -> i64 {
    task_runtime_boundary(|| {
        let millis = DIRECT_MONOTONIC_EPOCH
            .get_or_init(Instant::now)
            .elapsed()
            .as_millis();
        i64::try_from(millis)
            .unwrap_or_else(|_| runtime_error("monotonic time does not fit in Aura `int64`"))
    })
}

fn checked_sleep_milliseconds_with<F>(duration: i64, sleep: F) -> Result<(), Diagnostic>
where
    F: FnOnce(StdDuration) -> io::Result<RuntimeSchedulerWakeReason>,
{
    let nanoseconds = i128::from(duration) * crate::runtime_value::NANOS_PER_MILLISECOND;
    let timeout = crate::runtime_value::duration_to_host_timer(nanoseconds, "sleep duration")
        .map_err(direct_timer_diagnostic)?;
    sleep(timeout).map_err(direct_timer_diagnostic)?;
    Ok(())
}

fn checked_sleep_with_runtime_scheduler(timeout: StdDuration) -> Result<(), Diagnostic> {
    sleep_with_runtime_scheduler(timeout, Some(&current_cancellation()))
        .map_err(direct_timer_diagnostic)?;
    Ok(())
}

unsafe fn release_abandoned_direct_task_args(args_address: usize) {
    let args = unsafe { Box::from_raw(args_address as *mut Vec<i64>) };
    for value in args.iter().copied() {
        if value != 0 {
            unsafe {
                release_untracked_value(value as *mut OpaqueValue);
            }
        }
    }
}

fn allocate_direct_task_claim_flag() -> usize {
    let address = Box::into_raw(Box::new(AtomicBool::new(false))) as usize;
    #[cfg(test)]
    DIRECT_TASK_CLAIM_FLAG_LIVE_COUNT.fetch_add(1, Ordering::SeqCst);
    address
}

unsafe fn mark_direct_task_args_claimed(claim_flag_address: usize) {
    unsafe { &*(claim_flag_address as *const AtomicBool) }.store(true, Ordering::Release);
}

unsafe fn direct_task_args_were_claimed(claim_flag_address: usize) -> bool {
    unsafe { &*(claim_flag_address as *const AtomicBool) }.load(Ordering::Acquire)
}

unsafe fn release_direct_task_claim_flag(claim_flag_address: usize) {
    unsafe {
        drop(Box::from_raw(claim_flag_address as *mut AtomicBool));
    }
    #[cfg(test)]
    assert!(
        DIRECT_TASK_CLAIM_FLAG_LIVE_COUNT
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_sub(1)
            })
            .is_ok(),
        "direct task claim-flag live count underflowed"
    );
}

unsafe fn release_direct_task_external_state(args_address: usize, claim_flag_address: usize) {
    unsafe {
        if direct_task_args_were_claimed(claim_flag_address) {
            drop(Box::from_raw(args_address as *mut Vec<i64>));
        } else {
            release_abandoned_direct_task_args(args_address);
        }
        release_direct_task_claim_flag(claim_flag_address);
    }
}

struct DirectTaskExternalStateGuard {
    args_address: usize,
    claim_flag_address: usize,
}

impl Drop for DirectTaskExternalStateGuard {
    fn drop(&mut self) {
        unsafe {
            release_direct_task_external_state(self.args_address, self.claim_flag_address);
        }
    }
}

#[cfg(test)]
fn direct_task_claim_flag_live_count() -> usize {
    DIRECT_TASK_CLAIM_FLAG_LIVE_COUNT.load(Ordering::SeqCst)
}

unsafe fn consume_direct_task_result(result_ptr: *mut OpaqueValue, result_is_copy: bool) -> Value {
    if result_is_copy {
        unsafe { consume_value(result_ptr) }
    } else {
        unsafe { consume_owned_value(result_ptr) }
    }
}

unsafe fn claim_direct_task_args(args: &[i64], claim_flag_address: usize) {
    // Build the complete ledger entry off-state first. If collecting panics,
    // the external-state guard still sees `claimed == false` and releases the
    // raw buffer references itself. Installing the finished map and flipping
    // the raw atomic flag are then allocation-free, so no partially claimed
    // prefix can exist.
    let mut owned_value_refs = BTreeMap::new();
    for argument in args.iter().copied().filter(|argument| *argument != 0) {
        let count = owned_value_refs.entry(argument as usize).or_insert(0usize);
        *count = count
            .checked_add(1)
            .expect("a live task argument vector cannot exceed usize reference capacity");
    }
    with_direct_task_runtime_state(|state| {
        assert!(
            state.ownership_tracking_active,
            "direct task arguments require an active ownership ledger"
        );
        assert!(
            state.owned_value_refs.is_empty(),
            "direct task ownership ledger must be empty before argument claim"
        );
        state.owned_value_refs = owned_value_refs;
    });
    unsafe {
        mark_direct_task_args_claimed(claim_flag_address);
    }
}

#[cfg(test)]
unsafe fn spawn_direct_task_with_external_state<R>(
    cancellation: CancellationContext,
    thunk: NativeThunk,
    args_address: usize,
    claim_flag_address: usize,
    result_is_copy: bool,
    stack_size: Option<usize>,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    R: FnOnce(&TaskValue),
{
    unsafe {
        spawn_direct_task_with_external_state_and_ancestry(
            DirectTaskSpawn {
                cancellation,
                thunk,
                args_address,
                claim_flag_address,
                result_is_copy,
                stack_size,
                task_ancestry: DirectTaskAncestry::default(),
            },
            register_before_submit,
        )
    }
}

struct DirectTaskSpawn {
    cancellation: CancellationContext,
    thunk: NativeThunk,
    args_address: usize,
    claim_flag_address: usize,
    result_is_copy: bool,
    stack_size: Option<usize>,
    task_ancestry: DirectTaskAncestry,
}

unsafe fn spawn_direct_task_with_external_state_and_ancestry<R>(
    spawn: DirectTaskSpawn,
    register_before_submit: R,
) -> std::result::Result<TaskValue, Diagnostic>
where
    R: FnOnce(&TaskValue),
{
    let DirectTaskSpawn {
        cancellation,
        thunk,
        args_address,
        claim_flag_address,
        result_is_copy,
        stack_size,
        task_ancestry,
    } = spawn;
    // Build the full state on the spawning task's stack. The child coroutine
    // receives only the ready box pointer, keeping the 272-byte state
    // construction and copy out of child scope installation.
    let task_runtime_state = PreparedDirectTaskRuntimeState::new(task_ancestry);
    let entry = move || {
        // This guard is created on the coroutine stack rather than captured by
        // the force-reset closure. Normal return and ordinary Rust unwinding
        // release the external state after the direct-runtime scope guard has
        // finished; a forced reset abandons it and delegates to the scheduler
        // cleanup below.
        let _external_state = DirectTaskExternalStateGuard {
            args_address,
            claim_flag_address,
        };
        with_direct_task_runtime_scope_with_state(task_runtime_state.into_state(), || {
            Ok(with_task_runtime_error_capture(|| {
                unsafe {
                    let args = &*(args_address as *const Vec<i64>);
                    claim_direct_task_args(args, claim_flag_address);
                }
                let args = unsafe { &*(args_address as *const Vec<i64>) };
                let result_ptr = unsafe { thunk(args.as_ptr(), args.len()) };
                unsafe { consume_direct_task_result(result_ptr, result_is_copy) }
            }))
        })
    };
    let forced_exit_cleanup = move || {
        discard_current_direct_task_runtime_state();
        unsafe {
            release_direct_task_external_state(args_address, claim_flag_address);
        }
    };
    let task = unsafe {
        spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack_and_result_repeatability_registered(
            cancellation,
            stack_size,
            result_is_copy,
            entry,
            forced_exit_cleanup,
            register_before_submit,
        )
    };
    match task {
        Ok(task) => Ok(task),
        Err(error) => {
            unsafe {
                release_direct_task_external_state(args_address, claim_flag_address);
            }
            Err(error)
        }
    }
}

#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_start_task_call(
    thunk_ptr: i64,
    args_ptr: *const i64,
    arg_count: i64,
    returns_handle: i64,
    task_group: *mut OpaqueValue,
    result_is_copy: i64,
    stack_size_present: i64,
    stack_size: i64,
) -> *mut OpaqueValue {
    unsafe {
        start_direct_task_call(DirectTaskCall {
            thunk_ptr,
            args_ptr,
            arg_count,
            returns_handle,
            task_group,
            result_is_copy,
            stack_size_present,
            stack_size,
            task_ancestry: DirectTaskAncestry::default(),
        })
    }
}

/// Starts a generated Aura task while attaching immutable source metadata.
///
/// # Safety
///
/// Every non-null metadata byte range must remain readable and unchanged until
/// the spawned task completes. UTF-8 is validated by the runtime before any
/// range is retained. Native codegen satisfies this private ABI contract with
/// immutable object-file data.
#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_start_task_call_with_frames(
    thunk_ptr: i64,
    args_ptr: *const i64,
    arg_count: i64,
    returns_handle: i64,
    task_group: *mut OpaqueValue,
    result_is_copy: i64,
    stack_size_present: i64,
    stack_size: i64,
    task_function_ptr: *const u8,
    task_function_len: usize,
    task_path_ptr: *const u8,
    task_path_len: usize,
    task_line: i64,
    task_column: i64,
    parent_function_ptr: *const u8,
    parent_function_len: usize,
    spawn_path_ptr: *const u8,
    spawn_path_len: usize,
    spawn_line: i64,
    spawn_column: i64,
) -> *mut OpaqueValue {
    let task_function =
        match unsafe { DirectFrameText::validate_static(task_function_ptr, task_function_len) } {
            Ok(task_function) => task_function,
            Err(()) => reject_invalid_direct_frame_utf8(),
        };
    let parent_function =
        match unsafe { DirectFrameText::validate_static(parent_function_ptr, parent_function_len) }
        {
            Ok(parent_function) => parent_function,
            Err(()) => reject_invalid_direct_frame_utf8(),
        };
    let task_path = if task_path_ptr.is_null() || task_path_len == 0 {
        None
    } else {
        match unsafe { DirectFrameText::validate_static(task_path_ptr, task_path_len) } {
            Ok(task_path) => Some(task_path),
            Err(()) => reject_invalid_direct_frame_utf8(),
        }
    };
    let spawn_path = if spawn_path_ptr.is_null() || spawn_path_len == 0 {
        None
    } else {
        match unsafe { DirectFrameText::validate_static(spawn_path_ptr, spawn_path_len) } {
            Ok(spawn_path) => Some(spawn_path),
            Err(()) => reject_invalid_direct_frame_utf8(),
        }
    };
    let task_ancestry = direct_runtime_compact_task_ancestry().prepend(DirectRuntimeTaskFrame {
        task_function,
        task_entry_span: DirectRuntimeSourceSpan::point(
            task_path,
            Span::new(
                usize::try_from(task_line).unwrap_or_default(),
                usize::try_from(task_column).unwrap_or_default(),
            ),
        ),
        parent_function,
        spawn_span: DirectRuntimeSourceSpan::point(
            spawn_path,
            Span::new(
                usize::try_from(spawn_line).unwrap_or_default(),
                usize::try_from(spawn_column).unwrap_or_default(),
            ),
        ),
    });
    unsafe {
        start_direct_task_call(DirectTaskCall {
            thunk_ptr,
            args_ptr,
            arg_count,
            returns_handle,
            task_group,
            result_is_copy,
            stack_size_present,
            stack_size,
            task_ancestry,
        })
    }
}

/// Starts a generated Aura task through a first-class function value.
///
/// Unlike the legacy thunk-only entry point, this ABI obtains the selected
/// function's name, source path, entry span, and native thunk from the value
/// itself. That keeps task ancestry accurate when the function is selected at
/// runtime rather than appearing as a static MIR function operand.
///
/// # Safety
///
/// `function` and `task_group` must be live Aura opaque values. `args_ptr`
/// must be the allocation returned by `aura_direct_arg_buffer_new` for
/// `arg_count` entries. Parent/spawn metadata byte ranges must remain readable
/// and unchanged until the spawned task completes.
#[cfg_attr(not(coverage), no_mangle)]
pub unsafe extern "C-unwind" fn aura_direct_start_task_function_with_frames(
    function: *mut OpaqueValue,
    args_ptr: *const i64,
    arg_count: i64,
    returns_handle: i64,
    task_group: *mut OpaqueValue,
    result_is_copy: i64,
    stack_size_present: i64,
    stack_size: i64,
    parent_function_ptr: *const u8,
    parent_function_len: usize,
    spawn_path_ptr: *const u8,
    spawn_path_len: usize,
    spawn_line: i64,
    spawn_column: i64,
) -> *mut OpaqueValue {
    let (thunk_ptr, task_function, task_path, task_entry_span, closure_captures) =
        match unsafe { value_ref(function) } {
            Value::Function(function) => {
                let captures = function
                    .closure_environment
                    .as_ref()
                    .map(|environment| {
                        environment
                            .arguments(&function.name)
                            .unwrap_or_else(|error| runtime_diagnostic_error(error))
                    })
                    .unwrap_or_default();
                (
                    function.direct_thunk.unwrap_or_else(|| {
                        runtime_error("direct function value has no native thunk")
                    }),
                    DirectFrameText::shared(function.name.clone()),
                    function.source_path.clone().map(DirectFrameText::shared),
                    function.entry_span,
                    captures,
                )
            }
            other => runtime_error(format!(
                "task starting expected a function value, found `{}`",
                value_type_name(other)
            )),
        };
    let parent_function =
        match unsafe { DirectFrameText::validate_static(parent_function_ptr, parent_function_len) }
        {
            Ok(parent_function) => parent_function,
            Err(()) => reject_invalid_direct_frame_utf8(),
        };
    let spawn_path = if spawn_path_ptr.is_null() || spawn_path_len == 0 {
        None
    } else {
        match unsafe { DirectFrameText::validate_static(spawn_path_ptr, spawn_path_len) } {
            Ok(spawn_path) => Some(spawn_path),
            Err(()) => reject_invalid_direct_frame_utf8(),
        }
    };
    let task_ancestry = direct_runtime_compact_task_ancestry().prepend(DirectRuntimeTaskFrame {
        task_function,
        task_entry_span: DirectRuntimeSourceSpan::point(task_path, task_entry_span),
        parent_function,
        spawn_span: DirectRuntimeSourceSpan::point(
            spawn_path,
            Span::new(
                usize::try_from(spawn_line).unwrap_or_default(),
                usize::try_from(spawn_column).unwrap_or_default(),
            ),
        ),
    });
    let (args_ptr, arg_count) = if closure_captures.is_empty() {
        (args_ptr, arg_count)
    } else {
        let public_count = usize::try_from(arg_count)
            .unwrap_or_else(|_| runtime_error("invalid task-start arg count"));
        let public = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                args_ptr as *mut i64,
                public_count,
            ))
            .into_vec()
        };
        let mut combined = PendingDirectTaskArgs::default();
        for capture in closure_captures {
            combined.push_value(capture.value);
        }
        combined.extend_raw(public);
        let combined_count = i64::try_from(combined.len())
            .unwrap_or_else(|_| runtime_error("closure task argument count exceeds i64"));
        let combined_ptr = combined.into_raw_buffer();
        (combined_ptr as *const i64, combined_count)
    };
    unsafe {
        start_direct_task_call(DirectTaskCall {
            thunk_ptr,
            args_ptr,
            arg_count,
            returns_handle,
            task_group,
            result_is_copy,
            stack_size_present,
            stack_size,
            task_ancestry,
        })
    }
}

#[derive(Default)]
struct PendingDirectTaskArgs {
    handles: Vec<i64>,
}

impl PendingDirectTaskArgs {
    fn push_value(&mut self, value: Value) {
        let handle = boxed_value(value);
        // The raw task buffer, not the spawning task's generated-frame
        // ownership ledger, owns this reference from this point onward. The
        // child claims it in its own ledger immediately before entering the
        // generated thunk.
        unregister_direct_owned_value(handle);
        self.handles.push(handle as i64);
    }

    fn extend_raw(&mut self, handles: Vec<i64>) {
        self.handles.extend(handles);
    }

    fn len(&self) -> usize {
        self.handles.len()
    }

    fn into_raw_buffer(mut self) -> *mut i64 {
        let handles = std::mem::take(&mut self.handles).into_boxed_slice();
        Box::into_raw(handles) as *mut i64
    }
}

impl Drop for PendingDirectTaskArgs {
    fn drop(&mut self) {
        for handle in self.handles.drain(..).filter(|handle| *handle != 0) {
            unsafe {
                release_untracked_value(handle as *mut OpaqueValue);
            }
        }
    }
}

struct DirectTaskCall {
    thunk_ptr: i64,
    args_ptr: *const i64,
    arg_count: i64,
    returns_handle: i64,
    task_group: *mut OpaqueValue,
    result_is_copy: i64,
    stack_size_present: i64,
    stack_size: i64,
    task_ancestry: DirectTaskAncestry,
}

unsafe fn start_direct_task_call(call: DirectTaskCall) -> *mut OpaqueValue {
    let DirectTaskCall {
        thunk_ptr,
        args_ptr,
        arg_count,
        returns_handle,
        task_group,
        result_is_copy,
        stack_size_present,
        stack_size,
        task_ancestry,
    } = call;
    task_runtime_boundary(|| {
        let thunk: NativeThunk = unsafe { std::mem::transmute(thunk_ptr as usize) };
        let arg_count = match usize::try_from(arg_count) {
            Ok(arg_count) => arg_count,
            Err(_) => runtime_error("invalid task-start arg count"),
        };
        let args = unsafe {
            let boxed = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                args_ptr as *mut i64,
                arg_count,
            ));
            boxed.into_vec()
        };
        // Establish ownership immediately after reconstructing the raw
        // argument buffer. Every later diagnostic exit must release both the
        // retained opaque arguments and this claim flag.
        let args_address = Box::into_raw(Box::new(args)) as usize;
        let claim_flag_address = allocate_direct_task_claim_flag();
        let external_state_guard = DirectTaskExternalStateGuard {
            args_address,
            claim_flag_address,
        };
        let args = unsafe { &*(args_address as *const Vec<i64>) };
        let mut queue_producers = Vec::new();
        for arg in args.iter().copied().filter(|arg| *arg != 0) {
            unsafe {
                with_value(arg as *mut OpaqueValue, |value| {
                    collect_queue_values(value, &mut queue_producers)
                });
            }
        }
        let group = if task_group.is_null() {
            runtime_error("task starting requires a `TaskGroup`")
        } else {
            match unsafe { value_ref(task_group) } {
                Value::TaskGroup(group) => group.clone(),
                other => runtime_error(format!(
                    "expected `TaskGroup`, found `{}`",
                    value_type_name(other)
                )),
            }
        };
        let cancellation = group.child_cancellation();
        let stack_size = match stack_size_present {
            0 => None,
            1 if !(crate::call::MIN_TASK_STACK_BYTES..=crate::call::MAX_TASK_STACK_BYTES)
                .contains(&stack_size) =>
            {
                runtime_diagnostic_error(Diagnostic::coded(
                    "AU4005",
                    format!(
                        "task stack size must be between {} and {} bytes, found {}",
                        crate::call::MIN_TASK_STACK_BYTES,
                        crate::call::MAX_TASK_STACK_BYTES,
                        stack_size
                    ),
                ))
            }
            1 => Some(usize::try_from(stack_size).unwrap_or_else(|_| {
                runtime_diagnostic_error(Diagnostic::coded(
                    "AU4005",
                    "task stack size does not fit this platform",
                ))
            })),
            _ => runtime_error("invalid task-start stack-presence flag"),
        };
        // A direct task can be abandoned while suspended inside generated
        // Cranelift frames. Keep its raw argument allocation outside the
        // coroutine stack so the scheduler can reclaim it without unwinding
        // through those frames.
        // The spawned task and its forced-exit cleanup now own the external
        // state. The spawn helper releases it itself when scheduling fails.
        std::mem::forget(external_state_guard);
        let task = unsafe {
            spawn_direct_task_with_external_state_and_ancestry(
                DirectTaskSpawn {
                    cancellation,
                    thunk,
                    args_address,
                    claim_flag_address,
                    result_is_copy: result_is_copy != 0,
                    stack_size,
                    task_ancestry,
                },
                |task| {
                    group.register_task(task.clone());
                    for queue in &queue_producers {
                        queue.register_producer_task(task);
                        queue.register_task_handle(task);
                    }
                },
            )
        };
        let task = match task {
            Ok(task) => task,
            Err(error) => runtime_diagnostic_error(error),
        };
        if returns_handle == 0 {
            return boxed_value(Value::Unit);
        }
        boxed_value(Value::Task(task))
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_sqrt_f64(value: f64) -> f64 {
    task_runtime_boundary(|| value.sqrt())
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_assert_fail(message: i64, line: i64, column: i64) -> ! {
    task_runtime_boundary(|| {
        let message = direct_assert_message(message);
        runtime_diagnostic_error(direct_assert_diagnostic(message, line, column))
    })
}

fn direct_assert_message(message: i64) -> String {
    if message == 0 {
        "assertion failed".to_string()
    } else {
        direct_assert_string(message, "message")
    }
}

fn direct_assert_string(value: i64, field: &str) -> String {
    if value == 0 {
        runtime_error(format!(
            "direct assertion {field} must be `str`, found null"
        ));
    }
    let value = unsafe {
        with_value(value as *mut OpaqueValue, |value| match value {
            Value::String(value) => Ok(value.clone()),
            other => Err(format!(
                "direct assertion {field} must be `str`, found `{}`",
                value_type_name(other)
            )),
        })
    };
    value.unwrap_or_else(|error| runtime_error(error))
}

fn direct_assert_diagnostic(message: String, line: i64, column: i64) -> Diagnostic {
    match runtime_span(line, column) {
        Some(span) => Diagnostic::coded_at("AU4001", span, message),
        None => Diagnostic::coded("AU4001", message),
    }
}

/// Private direct-backend assertion ABI for the compiler-proven two-operand
/// introspection shape. Every pointer is borrowed for the duration of this
/// call; the diagnostic owns only bounded string snapshots.
#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_assert_fail_detailed(
    message: i64,
    line: i64,
    column: i64,
    left_label: i64,
    left_type: i64,
    left_value: i64,
    right_label: i64,
    right_type: i64,
    right_value: i64,
) -> ! {
    task_runtime_boundary(|| {
        let message = direct_assert_message(message);
        let left_label = direct_assert_string(left_label, "left label");
        let left_type = direct_assert_string(left_type, "left type");
        let left_value = direct_assert_string(left_value, "left value");
        let right_label = direct_assert_string(right_label, "right label");
        let right_type = direct_assert_string(right_type, "right type");
        let right_value = direct_assert_string(right_value, "right value");
        let diagnostic = direct_assert_diagnostic(message, line, column)
            .with_assertion_operand(left_label, left_type, left_value)
            .with_assertion_operand(right_label, right_type, right_value);
        runtime_diagnostic_error(diagnostic)
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fail_division_by_zero(line: i64, column: i64) -> ! {
    task_runtime_boundary(|| match runtime_span(line, column) {
        Some(span) => runtime_error_at(span, "division by zero"),
        None => runtime_error("division by zero"),
    })
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fail_int32_overflow(value: i64, line: i64, column: i64) -> ! {
    task_runtime_boundary(|| {
        let message = int32_overflow_message(value);
        match runtime_span(line, column) {
            Some(span) => runtime_error_at(span, message),
            None => runtime_error(message),
        }
    })
}

fn wide_integer_overflow_message(kind: i64, op: i64, left: u64, right: u64) -> String {
    let (value, type_name) = match kind {
        0 => {
            let left = i128::from(left as i64);
            let right = i128::from(right as i64);
            let value = match op {
                0 => left + right,
                1 => left - right,
                2 => left * right,
                3 => left / right,
                other => runtime_error(format!("unknown signed overflow opcode `{other}`")),
            };
            (value.to_string(), "int64")
        }
        1 => {
            let left = u128::from(left);
            let right = u128::from(right);
            let value = match op {
                0 => (left + right).to_string(),
                1 if left >= right => (left - right).to_string(),
                1 => format!("-{}", right - left),
                2 => (left * right).to_string(),
                3 => (left / right).to_string(),
                other => runtime_error(format!("unknown unsigned overflow opcode `{other}`")),
            };
            (value, "uint64")
        }
        other => runtime_error(format!("unknown integer overflow kind `{other}`")),
    };
    format!("integer value `{value}` does not fit in `{type_name}`")
}

#[cfg_attr(not(coverage), no_mangle)]
pub extern "C-unwind" fn aura_direct_fail_integer_overflow(
    kind: i64,
    op: i64,
    left: u64,
    right: u64,
    line: i64,
    column: i64,
) -> ! {
    task_runtime_boundary(|| {
        let message = wide_integer_overflow_message(kind, op, left, right);
        match runtime_span(line, column) {
            Some(span) => runtime_error_at(span, message),
            None => runtime_error(message),
        }
    })
}

#[path = "native_runtime_tests.rs"]
mod tests;
