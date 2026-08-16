#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::slice;
use std::str;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use crate::ast::{BinaryOp, UnaryOp};
use crate::builtin_modules::host_builtin_metadata;
use crate::call::{BuiltinAssociatedFunction, BuiltinMember};
use crate::diag::{
    Diagnostic, Result, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame, Span,
};
use crate::ffi::{FfiError, FfiSignature, FfiType, FfiValue};
use crate::integer::{
    IntegerKind, IntegerPowerError, IntegerRepresentation, IntegerShiftError, IntegerValue,
};
use crate::json_codec;
use crate::mir::{
    CallTarget, Instruction, MirArg, MirClass, MirExternCall, MirExternParam, MirFormatPart,
    MirFunction, MirMethod, MirModule, MirParam, MirReceiverKind, MirTraitImpl, Operand, Rvalue,
    Terminator, MIR_LOOP_SAFEPOINT_INTERVAL,
};
use crate::randomness::{self, SecureRandomError};
use crate::runtime_value::{
    append_string_checked, cast_numeric_value, catch_lightweight_task_failure,
    claim_task_result_observations, clone_json_codec_source, concat_strings_checked,
    decode_process_restart_policy, decode_process_stdio, divmod_numeric_values,
    duration_to_host_timer, duration_to_milliseconds, duration_to_seconds,
    evaluate_bytes_host_builtin_ref, evaluate_host_builtin_with_program_args,
    evaluate_string_to_bytes_host_ref, float_floor_divmod, float_power, format_runtime_value,
    host_process_args, io_error, io_read_line, json_array_metadata_is_exact,
    json_dump_error_to_diagnostic, json_int_metadata_is_exact, json_object_metadata_is_exact,
    json_parse_owned_to_runtime, nominal_runtime_base_name, option_none, option_some,
    poll_cancellation, prepare_json_codec_source, process_error_cancelled, process_error_io,
    process_error_no_command, process_error_spawn, process_error_timed_out, process_exit_status,
    process_stdio_inherit, process_stdio_null, process_stdio_pipe, process_supervisor_event_failed,
    process_supervisor_wait_cancelled, process_supervisor_wait_event,
    process_supervisor_wait_timed_out, process_wait_cancelled, process_wait_exited,
    process_wait_failed, process_wait_timed_out, queue_receive_cancelled, queue_receive_closed,
    queue_receive_item, queue_receive_timed_out, read_file_limited,
    recv_for_registered_producers_iteration, recv_for_task_group_iteration, render_float,
    render_float32, result_err, result_ok, round_numeric_value, run_blocking_io,
    run_lightweight_root_task, runtime_value_to_json, select_runtime_values, send_error_cancelled,
    send_error_closed, send_error_full, send_error_timed_out, sleep_with_runtime_scheduler,
    slice_string_owned, slice_vec_owned, spawn_lightweight_task,
    spawn_lightweight_task_with_result_repeatability_registered,
    spawn_lightweight_task_with_stack_and_result_repeatability_registered,
    task_group_cleanup_should_cancel, task_result_cancelled, task_result_error, task_result_ready,
    task_result_timed_out, try_array_buffer, try_clone_array_containing_value, wait_all_cancelled,
    wait_all_error, wait_all_ready, wait_all_timed_out, wait_any_cancelled, wait_any_error,
    wait_any_ready, wait_any_timed_out, wait_for_runtime_scheduler,
    yield_now_with_runtime_scheduler, ArrayBinaryOp, ArrayDType, ArrayReduction, ArrayValue,
    CancellationContext, ChannelValue, ClosureCaptureValue, ClosureEnvironment, EnumVariantValue,
    FfiHandleValue, FileValue, FloatPowerWidth, FunctionValue, HttpExchangeValue,
    HttpListenerValue, HttpResponseValue, InstanceValue, IntegerArithmeticMode, MapValue,
    ProcessChildValue, ProcessChildWaitStatus, ProcessCompletedValue, ProcessPipeValue,
    ProcessRestartPolicy, ProcessSupervisorValue, ProcessSupervisorWaitStatus, RangeValue,
    RecvValueResult, RngValue, RunOutput, RuntimeSchedulerWakeReason, SendValueError, SetValue,
    TaskCancelledSignal, TaskGroupValue, TaskValue, TaskWaitStatus, TcpListenerValue,
    TcpStreamValue, TlsListenerValue, TlsStreamValue, TupleValue, UdpDatagramValue, UdpSocketValue,
    UnixListenerValue, UnixStreamValue, Value, VecValue, WebSocketListenerValue, WebSocketValue,
    NANOS_PER_MILLISECOND, NANOS_PER_MINUTE, NANOS_PER_SECOND,
};
use crate::sema::{substitute_type, Type};

const MIR_FLOOR_DIVISION_OPERANDS_ERROR: &str =
    "MIR floor division requires matching numeric operands";

macro_rules! io_timeout_or_return {
    ($value:expr, $label:expr) => {
        match expect_io_optional_timeout($value, $label) {
            Ok(timeout) => timeout,
            Err(error) => return Ok(result_err(error)),
        }
    };
}

pub type StdoutSink = Arc<dyn Fn(&str) + Send + Sync + 'static>;

pub fn run(module: &MirModule) -> Result<RunOutput> {
    run_with_stdout_sink(module, None)
}

pub(crate) fn run_trusted(module: &MirModule) -> Result<RunOutput> {
    run_with_stdout_sink_trusted(module, None)
}

pub fn run_with_stdout_sink(
    module: &MirModule,
    stdout_sink: Option<StdoutSink>,
) -> Result<RunOutput> {
    run_with_stdout_sink_and_program_args(module, stdout_sink, Vec::new())
}

pub(crate) fn run_with_stdout_sink_trusted(
    module: &MirModule,
    stdout_sink: Option<StdoutSink>,
) -> Result<RunOutput> {
    run_with_stdout_sink_and_program_args_trusted(module, stdout_sink, Vec::new())
}

pub fn run_with_stdout_sink_and_program_args(
    module: &MirModule,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    run_entry_with_stdout_sink_and_program_args(module, None, stdout_sink, program_args)
}

pub(crate) fn run_with_stdout_sink_and_program_args_trusted(
    module: &MirModule,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    run_entry_with_stdout_sink_and_program_args_trusted(module, None, stdout_sink, program_args)
}

/// Runs `module`, entering at `entry` instead of its ordinary entry point.
///
/// `entry` names a parameterless top-level function. It exists so a test runner
/// can execute one `def test_*()` at a time through exactly the same runtime,
/// scheduler, and trap handling an ordinary run uses, rather than a parallel
/// execution path that could diverge.
pub fn run_entry_with_stdout_sink_and_program_args(
    module: &MirModule,
    entry: Option<&str>,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    reject_untrusted_extern_calls(module)?;
    run_entry_with_stdout_sink_and_program_args_trusted(module, entry, stdout_sink, program_args)
}

pub(crate) fn run_entry_with_stdout_sink_and_program_args_trusted(
    module: &MirModule,
    entry: Option<&str>,
    stdout_sink: Option<StdoutSink>,
    program_args: Vec<String>,
) -> Result<RunOutput> {
    crate::runtime_config::validate_runtime_configuration()?;
    let entry = entry.map(|name| name.to_string());
    let module = module.clone();
    let program_args = Arc::new(program_args);
    let handle = match thread::Builder::new()
        .stack_size(MIR_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(move || {
                let stdout = Arc::new(Mutex::new(String::new()));
                let task_stdout = stdout.clone();
                let task_stdout_sink = stdout_sink.clone();
                let task_entry = entry.clone();
                let value_result = if module_uses_lightweight_tasks(&module) {
                    run_lightweight_root_task(move || {
                        let mut runtime = MirRuntime::new_with_stdout_sink_and_program_args(
                            module,
                            task_stdout,
                            task_stdout_sink,
                            CancellationContext::default(),
                            program_args,
                        );
                        runtime.run_entry(task_entry.as_deref())
                    })
                } else {
                    let mut runtime = MirRuntime::new_with_stdout_sink_and_program_args(
                        module,
                        task_stdout,
                        task_stdout_sink,
                        CancellationContext::default(),
                        program_args,
                    );
                    catch_lightweight_task_failure(|| runtime.run_entry(entry.as_deref()))
                };
                let rendered_stdout = lock_stdout(&stdout).clone();
                match value_result {
                    Ok(value) => Ok(RunOutput {
                        value,
                        stdout: rendered_stdout,
                    }),
                    Err(error) => Err(error.with_partial_stdout(rendered_stdout)),
                }
            }));
            match result {
                Ok(result) => result,
                Err(_) => Err(Diagnostic::new(
                    "Aura MIR runtime panicked while executing the program",
                )),
            }
        }) {
        Ok(handle) => handle,
        Err(error) => {
            return Err(Diagnostic::coded(
                "AU4001",
                format!("failed to start MIR runtime thread: {}", error),
            ));
        }
    };
    match handle.join() {
        Ok(result) => result.map_err(Diagnostic::into_runtime_trap),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn reject_untrusted_extern_calls(module: &MirModule) -> Result<()> {
    let symbol = module
        .functions
        .iter()
        .chain(module.top_level.iter())
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Extern(call),
                        ..
                    },
                ..
            } => Some(call.symbol.as_str()),
            Instruction::Safepoint
            | Instruction::BeginLoan { .. }
            | Instruction::BeginReturnedLoan { .. }
            | Instruction::Reborrow { .. }
            | Instruction::ReadLoan { .. }
            | Instruction::WriteLoan { .. }
            | Instruction::EndLoan { .. }
            | Instruction::ReturnLoan { .. }
            | Instruction::Assign { .. }
            | Instruction::Eval { .. }
            | Instruction::PushCleanup { .. }
            | Instruction::PopCleanup { .. } => None,
        });

    match symbol {
        Some(symbol) => Err(Diagnostic::coded(
            "AU4001",
            format!(
                "public MIR execution rejects the untrusted extern call `{symbol}`; FFI must be authorized by a manifest-rooted package with `[package] allow_ffi = true` and executed through a path-based API"
            ),
        )
        .into_runtime_trap()),
        None => Ok(()),
    }
}

fn module_uses_lightweight_tasks(module: &MirModule) -> bool {
    module
        .functions
        .iter()
        .chain(module.top_level.iter())
        // Runtime-provided declarations receive forwarding MIR bodies so
        // first-class builtin values dispatch through the same implementation
        // as direct calls. Those wrappers are not source reachability: in
        // particular, the always-present `process::run` wrapper must not force
        // every otherwise-synchronous program onto a 512 KiB task stack.
        .filter(|function| !crate::mir::has_runtime_named_function(&function.name))
        .any(function_uses_lightweight_tasks)
}

fn function_uses_lightweight_tasks(function: &MirFunction) -> bool {
    function.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| match instruction {
                Instruction::Assign { value, .. } => rvalue_uses_lightweight_tasks(value),
                Instruction::Safepoint
                | Instruction::BeginLoan { .. }
                | Instruction::BeginReturnedLoan { .. }
                | Instruction::Reborrow { .. }
                | Instruction::ReadLoan { .. }
                | Instruction::WriteLoan { .. }
                | Instruction::EndLoan { .. }
                | Instruction::ReturnLoan { .. }
                | Instruction::Eval { .. }
                | Instruction::PushCleanup { .. }
                | Instruction::PopCleanup { .. } => false,
            })
    })
}

fn rvalue_uses_lightweight_tasks(value: &Rvalue) -> bool {
    matches!(value, Rvalue::StartTask { .. })
        || matches!(
            value,
            Rvalue::Call {
                callee: CallTarget::Name(name),
                ..
            } if name == "process::run"
        )
        || rvalue_materializes_process_run(value)
}

fn operand_is_process_run_function(operand: &Operand) -> bool {
    matches!(
        operand,
        Operand::Function { name, .. } if name == "process::run"
    )
}

fn args_materialize_process_run(args: &[MirArg]) -> bool {
    args.iter()
        .any(|argument| operand_is_process_run_function(&argument.value))
}

fn rvalue_materializes_process_run(value: &Rvalue) -> bool {
    match value {
        Rvalue::Use(value)
        | Rvalue::Unary { value, .. }
        | Rvalue::Cast { value, .. }
        | Rvalue::Try { value }
        | Rvalue::TupleElement { tuple: value, .. }
        | Rvalue::VariantPayload {
            scrutinee: value, ..
        }
        | Rvalue::Member { object: value, .. } => operand_is_process_run_function(value),
        Rvalue::Closure { captures, .. } => captures
            .iter()
            .any(|capture| operand_is_process_run_function(&capture.value)),
        Rvalue::FormatString { parts } => parts.iter().any(|part| match part {
            crate::mir::MirFormatPart::Value(value)
            | crate::mir::MirFormatPart::Formatted { value, .. } => {
                operand_is_process_run_function(value)
            }
            crate::mir::MirFormatPart::Literal(_) => false,
        }),
        // `rvalue_uses_lightweight_tasks` recognizes every `StartTask` before
        // consulting this recursive materialization helper. Inspecting its
        // operands here was therefore unreachable, and obscured the simpler
        // contract: starting any task already requires the scheduler.
        Rvalue::StartTask { .. } => false,
        Rvalue::Binary { left, right, .. } => {
            operand_is_process_run_function(left) || operand_is_process_run_function(right)
        }
        Rvalue::Call { callee, args } => {
            let callee_materializes = match callee {
                CallTarget::Name(_) | CallTarget::Extern(_) => false,
                CallTarget::Value(value) => operand_is_process_run_function(value),
                CallTarget::Member { object, .. } => operand_is_process_run_function(object),
            };
            callee_materializes || args_materialize_process_run(args)
        }
        Rvalue::VecLiteral { elements, .. }
        | Rvalue::TupleLiteral { elements, .. }
        | Rvalue::SetLiteral { elements, .. }
        | Rvalue::EnumVariant {
            payloads: elements, ..
        } => elements.iter().any(operand_is_process_run_function),
        Rvalue::MapLiteral { entries, .. } => entries.iter().any(|entry| {
            operand_is_process_run_function(&entry.key)
                || operand_is_process_run_function(&entry.value)
        }),
        Rvalue::Construct { fields, .. } => fields
            .iter()
            .any(|field| operand_is_process_run_function(&field.value)),
        Rvalue::TupleTakeElement { .. } | Rvalue::ModuleConstant { .. } => false,
    }
}

fn lock_stdout(stdout: &Arc<Mutex<String>>) -> std::sync::MutexGuard<'_, String> {
    match stdout.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub fn run_serialized_mir(mir_json: &[u8], source_path: &str, source: &str) -> Result<RunOutput> {
    let module = deserialize_runtime_module(mir_json)?;
    validate_runtime_module_complexity(&module).map_err(Diagnostic::into_runtime_trap)?;
    let _ = source_path;
    let _ = source;
    run_with_stdout_sink_and_program_args(&module, None, host_process_args())
}

fn run_serialized_mir_trusted(
    mir_json: &[u8],
    source_path: &str,
    source: &str,
) -> Result<RunOutput> {
    let module = deserialize_runtime_module(mir_json)?;
    validate_runtime_module_complexity(&module).map_err(Diagnostic::into_runtime_trap)?;
    let _ = source_path;
    let _ = source;
    run_with_stdout_sink_and_program_args_trusted(&module, None, host_process_args())
}

fn deserialize_runtime_module(mir_json: &[u8]) -> Result<MirModule> {
    let module = match serde_json::from_slice::<MirModule>(mir_json) {
        Ok(module) => module,
        Err(error) => {
            return Err(Diagnostic::coded(
                "AU4001",
                format!("failed to deserialize embedded MIR: {}", error),
            ))
        }
    };
    Ok(module)
}

// Keep the MIR runtime call-depth budget comfortably below the host thread's
// stack ceiling. Recursive Aura programs should fail with a diagnostic before
// the runtime thread can overflow its Rust stack.
const MAX_CALL_DEPTH: usize = 256;
const MIR_RUNTIME_STACK_SIZE: usize = 64 * 1024 * 1024;
const MAX_EMBEDDED_RUNTIME_BYTES: usize = 1 << 30;
const MAX_RUNTIME_BLOCKS: usize = 1_000_000;
const MAX_RUNTIME_INSTRUCTIONS: usize = 1_000_000;
const MAX_RUNTIME_TERMINATOR_ARMS: usize = 1_000_000;

#[derive(Clone, Copy)]
struct RuntimeModuleLimits {
    max_blocks: usize,
    max_instructions: usize,
    max_terminator_arms: usize,
}

const DEFAULT_RUNTIME_MODULE_LIMITS: RuntimeModuleLimits = RuntimeModuleLimits {
    max_blocks: MAX_RUNTIME_BLOCKS,
    max_instructions: MAX_RUNTIME_INSTRUCTIONS,
    max_terminator_arms: MAX_RUNTIME_TERMINATOR_ARMS,
};

fn render_runtime_error(path: &str, source: &str, error: &Diagnostic) -> String {
    error.render_with_source(path, source)
}

fn write_stream(mut stream: impl Write, text: &str) -> io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

fn validate_embedded_runtime_length(name: &str, len: usize) -> std::result::Result<(), String> {
    if len > MAX_EMBEDDED_RUNTIME_BYTES {
        return Err(format!(
            "embedded {} length {} exceeds the supported runtime limit of {} bytes",
            name, len, MAX_EMBEDDED_RUNTIME_BYTES
        ));
    }
    Ok(())
}

fn validate_runtime_module_complexity(module: &MirModule) -> Result<()> {
    validate_runtime_module_complexity_with_limits(module, DEFAULT_RUNTIME_MODULE_LIMITS)
}

fn validate_runtime_module_complexity_with_limits(
    module: &MirModule,
    limits: RuntimeModuleLimits,
) -> Result<()> {
    let mut total_blocks = 0usize;
    let mut total_instructions = 0usize;
    let mut total_arms = 0usize;
    for function in module.functions.iter().chain(module.top_level.iter()) {
        total_blocks = total_blocks.saturating_add(function.blocks.len());
        if total_blocks > limits.max_blocks {
            return Err(Diagnostic::new(format!(
                "embedded MIR exceeds the supported block limit of {}",
                limits.max_blocks
            )));
        }
        for block in &function.blocks {
            total_instructions = total_instructions.saturating_add(block.instructions.len());
            if total_instructions > limits.max_instructions {
                return Err(Diagnostic::new(format!(
                    "embedded MIR exceeds the supported instruction limit of {}",
                    limits.max_instructions
                )));
            }
            total_arms = total_arms.saturating_add(match &block.terminator {
                Terminator::Match { arms, .. } => arms.len(),
                _ => 0,
            });
            if total_arms > limits.max_terminator_arms {
                return Err(Diagnostic::new(format!(
                    "embedded MIR exceeds the supported branching-arm limit of {}",
                    limits.max_terminator_arms
                )));
            }
        }
    }
    Ok(())
}

fn run_serialized_mir_entrypoint(mir_json: &[u8], source_path: &str, source: &str) -> i32 {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_serialized_mir_entrypoint_with_streams(
        mir_json,
        source_path,
        source,
        &mut stdout,
        &mut stderr,
    )
}

fn run_serialized_mir_entrypoint_with_streams(
    mir_json: &[u8],
    source_path: &str,
    source: &str,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    match run_serialized_mir_trusted(mir_json, source_path, source) {
        Ok(output) => {
            if let Err(error) = write_stream(&mut *stdout, &output.stdout) {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    return 0;
                }
                let _ = writeln!(&mut *stderr, "failed to write to stdout: {}", error);
                return 1;
            }
            if let Value::Int(code) = output.value {
                return code.as_i128().unwrap_or(0) as i32;
            }
            0
        }
        Err(error) => {
            if let Some(partial_stdout) = error.partial_stdout() {
                if let Err(write_error) = write_stream(&mut *stdout, partial_stdout) {
                    if write_error.kind() == io::ErrorKind::BrokenPipe {
                        return 0;
                    }
                    let _ = writeln!(&mut *stderr, "failed to write to stdout: {}", write_error);
                    return 1;
                }
            }
            let rendered = render_runtime_error(source_path, source, &error);
            let _ = writeln!(&mut *stderr, "{}", rendered);
            1
        }
    }
}

#[no_mangle]
/// # Safety
///
/// `mir_ptr`, `source_path_ptr`, and `source_ptr` must either be valid for reads of their paired
/// lengths or be null when the paired length is zero. The byte buffers must remain alive for the
/// duration of this call and must point to valid UTF-8 for the embedded source path/source
/// payloads.
pub unsafe extern "C" fn aura_native_run(
    mir_ptr: *const u8,
    mir_len: usize,
    source_path_ptr: *const u8,
    source_path_len: usize,
    source_ptr: *const u8,
    source_len: usize,
) -> i32 {
    let result = panic::catch_unwind(|| {
        if mir_ptr.is_null() || source_path_ptr.is_null() || source_ptr.is_null() {
            let _ = writeln!(
                io::stderr().lock(),
                "aura native runtime received a null input"
            );
            return 1;
        }

        for (name, len) in [
            ("MIR payload", mir_len),
            ("source path", source_path_len),
            ("source payload", source_len),
        ] {
            if let Err(message) = validate_embedded_runtime_length(name, len) {
                let _ = writeln!(io::stderr().lock(), "{}", message);
                return 1;
            }
        }

        let mir_json = unsafe { slice::from_raw_parts(mir_ptr, mir_len) };
        let source_path_bytes = unsafe { slice::from_raw_parts(source_path_ptr, source_path_len) };
        let source_bytes = unsafe { slice::from_raw_parts(source_ptr, source_len) };

        let source_path = match str::from_utf8(source_path_bytes) {
            Ok(text) => text,
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "embedded source path is not valid UTF-8: {}",
                    error
                );
                return 1;
            }
        };

        let source = match str::from_utf8(source_bytes) {
            Ok(text) => text,
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "embedded source is not valid UTF-8: {}",
                    error
                );
                return 1;
            }
        };

        run_serialized_mir_entrypoint(mir_json, source_path, source)
    });

    match result {
        Ok(code) => code,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                text.to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "unknown panic".to_string()
            };
            let _ = writeln!(
                io::stderr().lock(),
                "aura native runtime panicked: {}",
                message
            );
            1
        }
    }
}

struct MirRuntime {
    module: Arc<MirModule>,
    safepoints_enabled: bool,
    functions: HashMap<String, MirFunction>,
    classes: HashMap<String, MirClass>,
    trait_impls: Vec<MirTraitImpl>,
    stdout: Arc<Mutex<String>>,
    stdout_sink: Option<StdoutSink>,
    cancellation: CancellationContext,
    program_args: Arc<Vec<String>>,
    call_depth: usize,
    call_stack: Vec<RuntimeCallFrame>,
    task_ancestry: Vec<RuntimeTaskFrame>,
    return_type_stack: Vec<Type>,
    constant_states: Arc<Mutex<HashMap<String, MirConstantState>>>,
    pending_returned_view_projection: Option<String>,
}

#[derive(Clone)]
enum MirConstantState {
    Initializing,
    Ready(Arc<Value>),
    Failed(Diagnostic),
}

struct CallOutcome {
    value: Value,
    updated_receiver: Option<Value>,
    updated_params: Vec<(usize, Value)>,
}

#[derive(Clone)]
struct EvaluatedMirArg {
    name: Option<String>,
    value: Value,
    ty: Option<Type>,
    writeback_place: Option<String>,
}

struct StartTaskRequest<'a> {
    returns_handle: bool,
    result_is_repeatable: bool,
    stack_size: Option<usize>,
    task_group: &'a Operand,
    function: &'a Operand,
    args: &'a [MirArg],
    spawn_span: Span,
}

fn mir_function_value(name: &str, signature: &Type) -> Value {
    Value::Function(Box::new(FunctionValue {
        name: name.to_string(),
        signature: signature.clone(),
        source_path: None,
        entry_span: Span::new(0, 0),
        direct_thunk: None,
        direct_default_binder: None,
        closure_environment: None,
    }))
}

enum RvalueOutcome {
    Value(Value),
    SharedModuleConstant(Arc<Value>),
    Return(Value),
}

fn try_clone_mir_value(value: &Value) -> Result<Value> {
    try_clone_array_containing_value(value)
}

#[derive(Default)]
struct Env {
    values: HashMap<String, Value>,
    shared_values: HashMap<String, Arc<Value>>,
    types: HashMap<String, Type>,
    loans: HashMap<String, RuntimeLoan>,
}

#[derive(Clone)]
struct RuntimeLoan {
    source: String,
    mutable: bool,
}

#[cfg(test)]
std::thread_local! {
    static MIR_VALUE_CLONE_COUNT: Cell<usize> = const { Cell::new(0) };
    static MIR_ARRAY_PLACE_CLONE_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn mir_value_clone_count() -> usize {
    MIR_VALUE_CLONE_COUNT.with(Cell::get)
}

#[cfg(test)]
fn mir_array_place_clone_count() -> usize {
    MIR_ARRAY_PLACE_CLONE_COUNT.with(Cell::get)
}

impl Env {
    fn resolve_loan_place(&self, place: &str) -> Result<String> {
        let mut resolved = place.to_string();
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let (root, suffix) = resolved
                .split_once('.')
                .map(|(root, suffix)| (root, Some(suffix)))
                .unwrap_or((resolved.as_str(), None));
            let Some(loan) = self.loans.get(root) else {
                return Ok(resolved);
            };
            if !seen.insert(root.to_string()) {
                return Err(Diagnostic::new(format!(
                    "cyclic MIR loan descriptor rooted at `{root}`"
                )));
            }
            resolved = match suffix {
                Some(suffix) if !suffix.is_empty() => format!("{}.{}", loan.source, suffix),
                _ => loan.source.clone(),
            };
        }
    }

    fn begin_loan(&mut self, loan: &str, source: &str, mutable: bool) -> Result<()> {
        let source = self.resolve_loan_place(source)?;
        if !self
            .values
            .contains_key(source.split('.').next().unwrap_or_default())
            && !self
                .shared_values
                .contains_key(source.split('.').next().unwrap_or_default())
        {
            return Err(Diagnostic::new(format!(
                "cannot begin MIR loan `{loan}` from unknown place `{source}`"
            )));
        }
        self.loans
            .insert(loan.to_string(), RuntimeLoan { source, mutable });
        Ok(())
    }

    fn returned_view_projection(&self, loan: &str, origin: &str) -> Result<String> {
        let source = self.resolve_loan_place(loan)?;
        let origin = self.resolve_loan_place(origin)?;
        if source == origin {
            return Ok(String::new());
        }
        source
            .strip_prefix(&format!("{origin}."))
            .map(str::to_string)
            .ok_or_else(|| {
                Diagnostic::new(format!(
                    "returned MIR loan `{loan}` resolves to `{source}`, outside declared origin `{origin}`"
                ))
            })
    }

    fn end_loan(&mut self, loan: &str) -> Result<()> {
        self.loans
            .remove(loan)
            .map(|_| ())
            .ok_or_else(|| Diagnostic::new(format!("cannot end unknown MIR loan `{loan}`")))
    }

    fn define_typed(&mut self, name: impl Into<String>, ty: Type, value: Value) {
        let name = name.into();
        self.types.insert(name.clone(), ty);
        self.shared_values.remove(&name);
        self.values.insert(name, value);
    }

    fn read_member(&self, place: &str, field: &str) -> Result<Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        let mut current = if let Some(value) = self.shared_values.get(root) {
            value.as_ref()
        } else {
            self.values
                .get(root)
                .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{}`", place)))?
        };
        let mut index = 0usize;
        while index < rest.len() {
            let segment = &rest[index];
            let Value::Instance(instance) = current else {
                return Err(Diagnostic::new(format!(
                    "cannot access field `{}` on non-instance MIR place `{}`",
                    segment, place
                )));
            };
            current = match instance.fields.get(segment) {
                Some(value) => value,
                None => {
                    return Err(Diagnostic::new(format!(
                        "class `{}` has no field `{}` in MIR place `{}`",
                        instance.class_name, segment, place
                    )));
                }
            };
            index += 1;
        }
        let Value::Instance(instance) = current else {
            return Err(Diagnostic::new(format!(
                "cannot access field `{}` on non-instance MIR place `{}`",
                field, place
            )));
        };
        match instance.fields.get(field) {
            Some(value) => try_clone_mir_value(value),
            None => Err(Diagnostic::new(format!(
                "class `{}` has no field `{}` in MIR place `{}`",
                instance.class_name, field, place
            ))),
        }
    }

    fn place_ref(&self, place: &str) -> Result<&Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        let mut value = if let Some(value) = self.shared_values.get(root) {
            value.as_ref()
        } else {
            self.values
                .get(root)
                .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{}`", place)))?
        };
        let mut index = 0usize;
        while index < rest.len() {
            let segment = &rest[index];
            value = match value {
                Value::Instance(instance) => instance.fields.get(segment).ok_or_else(|| {
                    Diagnostic::new(format!(
                        "class `{}` has no field `{}` in MIR place `{}`",
                        instance.class_name, segment, place
                    ))
                })?,
                Value::Tuple(tuple) => {
                    let tuple_index = segment.parse::<usize>().map_err(|_| {
                        Diagnostic::new(format!(
                            "tuple projection `{segment}` is not a fixed position in MIR place `{place}`"
                        ))
                    })?;
                    tuple.elements.get(tuple_index).ok_or_else(|| {
                        Diagnostic::new(format!(
                            "tuple MIR place `{place}` has no element at index {tuple_index}"
                        ))
                    })?
                }
                _ => {
                    return Err(Diagnostic::new(format!(
                        "cannot access field `{segment}` on non-instance MIR place `{place}`"
                    )))
                }
            };
            index += 1;
        }
        Ok(value)
    }

    fn read_place(&self, place: &str) -> Result<Value> {
        #[cfg(test)]
        MIR_VALUE_CLONE_COUNT.with(|count| count.set(count.get() + 1));
        let value = self.place_ref(place)?;
        #[cfg(test)]
        if matches!(value, Value::Array(_)) {
            MIR_ARRAY_PLACE_CLONE_COUNT.with(|count| count.set(count.get() + 1));
        }
        try_clone_mir_value(value)
    }

    fn take_place(&mut self, place: &str) -> Result<Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        if rest.is_empty() {
            if self.shared_values.contains_key(root) {
                return Err(Diagnostic::new(format!(
                    "shared MIR place `{place}` reached a consuming context"
                )));
            }
            return self
                .values
                .remove(root)
                .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{}`", place)));
        }
        let value = self.values.get_mut(root).ok_or_else(|| {
            if self.shared_values.contains_key(root) {
                Diagnostic::new(format!("shared MIR place `{place}` cannot be mutated"))
            } else {
                Diagnostic::new(format!("unknown MIR place `{}`", place))
            }
        })?;
        take_nested_place(value, rest, place)
    }

    fn place_mut(&mut self, place: &str) -> Result<&mut Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        let value = self.values.get_mut(root).ok_or_else(|| {
            if self.shared_values.contains_key(root) {
                Diagnostic::new(format!("shared MIR place `{place}` cannot be mutated"))
            } else {
                Diagnostic::new(format!("unknown MIR place `{place}`"))
            }
        })?;
        nested_place_mut(value, rest, place)
    }

    fn take_variant_payload(&mut self, place: &str, index: usize) -> Result<Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        let value = self
            .values
            .get_mut(root)
            .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{}`", place)))?;
        let value = nested_place_mut(value, rest, place)?;
        let Value::EnumVariant(variant) = value else {
            return Err(Diagnostic::new(format!(
                "cannot take enum payload from non-enum MIR place `{place}`"
            )));
        };
        let payload = variant.payloads.get_mut(index).ok_or_else(|| {
            Diagnostic::new(format!(
                "enum variant `{}.{}` does not carry a payload at index {}",
                variant.enum_name, variant.variant_name, index
            ))
        })?;
        Ok(std::mem::replace(payload, Value::Unit))
    }

    fn tuple_element(&self, place: &str, index: usize) -> Result<Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let value = self.place_ref(place)?;
        let Value::Tuple(tuple) = value else {
            return Err(Diagnostic::new(format!(
                "cannot project a tuple element from non-tuple MIR place `{place}`"
            )));
        };
        tuple
            .elements
            .get(index)
            .ok_or_else(|| {
                Diagnostic::new(format!(
                    "tuple MIR place `{place}` has no element at index {index}"
                ))
            })
            .and_then(try_clone_mir_value)
    }

    fn take_tuple_element(&mut self, place: &str, index: usize) -> Result<Value> {
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");
        let value = self
            .values
            .get_mut(root)
            .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{place}`")))?;
        let value = nested_place_mut(value, rest, place)?;
        let Value::Tuple(tuple) = value else {
            return Err(Diagnostic::new(format!(
                "cannot take a tuple element from non-tuple MIR place `{place}`"
            )));
        };
        let element = tuple.elements.get_mut(index).ok_or_else(|| {
            Diagnostic::new(format!(
                "tuple MIR place `{place}` has no element at index {index}"
            ))
        })?;
        if matches!(element, Value::Unit) {
            return Err(Diagnostic::new(format!(
                "tuple element `{place}[{index}]` has already been moved"
            )));
        }
        Ok(std::mem::replace(element, Value::Unit))
    }

    fn write_place(&mut self, place: &str, value: Value) -> Result<()> {
        let original_root = place.split('.').next().unwrap_or_default();
        if let Some(loan) = self.loans.get(original_root) {
            if !loan.mutable {
                return Err(Diagnostic::new(format!(
                    "cannot write through shared MIR loan `{original_root}`"
                )));
            }
        }
        let resolved_place = self.resolve_loan_place(place)?;
        let place = resolved_place.as_str();
        let segments = split_place_segments(place)?;
        let (root, rest) = segments
            .split_first()
            .expect("split_place_segments rejects empty MIR places");

        if rest.is_empty() {
            self.shared_values.remove(root);
            self.values.insert((*root).to_string(), value);
            return Ok(());
        }

        let root_value = self
            .values
            .get_mut(root)
            .ok_or_else(|| Diagnostic::new(format!("unknown MIR place `{place}`")))?;
        let rest_refs = rest
            .iter()
            .map(|segment| segment.as_str())
            .collect::<Vec<_>>();
        write_nested_place(root_value, &rest_refs, value, place)
    }

    fn write_shared_place(&mut self, place: &str, value: Arc<Value>) -> Result<()> {
        if place.contains('.') {
            return Err(Diagnostic::new(format!(
                "module constant reference cannot be assigned to nested MIR place `{place}`"
            )));
        }
        self.values.remove(place);
        self.shared_values.insert(place.to_string(), value);
        Ok(())
    }

    fn place_type(&self, place: &str) -> Option<&Type> {
        self.types.get(place)
    }

    fn set_place_type(&mut self, place: &str, ty: Type) {
        self.types.insert(place.to_string(), ty);
    }
}

fn checked_mir_array_ref(value: &Value) -> &ArrayValue {
    let Value::Array(array) = value else {
        unreachable!("semantic analysis and MIR lowering preserve Array place variants")
    };
    array
}

fn checked_mir_array_mut(value: &mut Value) -> &mut ArrayValue {
    let Value::Array(array) = value else {
        unreachable!("semantic analysis and MIR lowering preserve Array place variants")
    };
    array
}

fn checked_mir_vec_ref(value: &Value) -> &VecValue {
    let Value::Vec(vector) = value else {
        unreachable!("semantic analysis and MIR lowering preserve checked Vec operands")
    };
    vector
}

fn checked_mir_tuple_ref(value: &Value) -> &TupleValue {
    let Value::Tuple(tuple) = value else {
        unreachable!("semantic analysis and MIR lowering preserve checked tuple operands")
    };
    tuple
}

fn checked_mir_function_ref(value: &Value) -> &FunctionValue {
    let Value::Function(function) = value else {
        unreachable!("semantic analysis and MIR lowering preserve checked function operands")
    };
    function
}

fn checked_mir_integer_ref(value: &Value) -> &IntegerValue {
    let Value::Int(integer) = value else {
        unreachable!("semantic analysis and MIR lowering preserve checked integer operands")
    };
    integer
}

fn checked_mir_function_return_type(signature: &Type) -> &Type {
    let (Type::Function { return_type, .. } | Type::Closure { return_type, .. }) = signature else {
        unreachable!("checked MIR function values carry function or closure signatures")
    };
    return_type
}

fn checked_mir_array_dtype(result_type: Option<&Type>) -> ArrayDType {
    let result_type = result_type.expect("checked MIR Array constructors carry a result type");
    let Type::Named(name, arguments) = result_type else {
        unreachable!("checked MIR Array constructors return Array[T]")
    };
    debug_assert_eq!(name, "Array");
    debug_assert_eq!(arguments.len(), 1);
    ArrayDType::from_type(&arguments[0])
        .expect("semantic analysis restricts Array elements to supported numeric dtypes")
}

fn array_place_ref<'a>(env: &'a Env, place: &str) -> Result<&'a ArrayValue> {
    Ok(checked_mir_array_ref(env.place_ref(place)?))
}

fn array_place_mut<'a>(env: &'a mut Env, place: &str) -> Result<&'a mut ArrayValue> {
    Ok(checked_mir_array_mut(env.place_mut(place)?))
}

fn split_place_segments(place: &str) -> Result<Vec<String>> {
    if place.is_empty() {
        return Err(Diagnostic::new("empty MIR place"));
    }

    let bytes = place.as_bytes();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'.' {
            if current.is_empty() {
                return Err(Diagnostic::new(format!("invalid MIR place `{}`", place)));
            }
            segments.push(current);
            current = String::new();
        } else {
            current.push(bytes[index] as char);
        }
        index += 1;
    }
    if current.is_empty() {
        return Err(Diagnostic::new(format!("invalid MIR place `{}`", place)));
    }
    segments.push(current);
    Ok(segments)
}

fn write_nested_place(
    current: &mut Value,
    segments: &[&str],
    value: Value,
    full_place: &str,
) -> Result<()> {
    let Some((segment, rest)) = segments.split_first() else {
        return Err(Diagnostic::new(format!(
            "cannot assign empty nested MIR place `{full_place}`"
        )));
    };
    match current {
        Value::Instance(instance) => {
            if rest.is_empty() {
                instance.fields.insert((*segment).to_string(), value);
                return Ok(());
            }
            let child = instance.fields.get_mut(*segment).ok_or_else(|| {
                Diagnostic::new(format!(
                    "class `{}` has no field `{}` in MIR place `{}`",
                    instance.class_name, segment, full_place
                ))
            })?;
            write_nested_place(child, rest, value, full_place)
        }
        Value::Tuple(tuple) => {
            let index = segment.parse::<usize>().map_err(|_| {
                Diagnostic::new(format!(
                    "tuple projection `{segment}` is not a fixed position in MIR place `{full_place}`"
                ))
            })?;
            let child = tuple.elements.get_mut(index).ok_or_else(|| {
                Diagnostic::new(format!(
                    "tuple MIR place `{full_place}` has no element at index {index}"
                ))
            })?;
            if rest.is_empty() {
                *child = value;
                Ok(())
            } else {
                write_nested_place(child, rest, value, full_place)
            }
        }
        _ => Err(Diagnostic::new(format!(
            "cannot assign nested MIR place `{full_place}` on non-instance value"
        ))),
    }
}

fn take_nested_place(value: &mut Value, segments: &[String], full_place: &str) -> Result<Value> {
    let Some((segment, rest)) = segments.split_first() else {
        return Err(Diagnostic::new(format!(
            "cannot move empty nested MIR place `{full_place}`"
        )));
    };
    match value {
        Value::Instance(instance) => {
            if rest.is_empty() {
                return instance.fields.remove(segment).ok_or_else(|| {
                    Diagnostic::new(format!(
                        "class `{}` has no field `{}` in MIR place `{}`",
                        instance.class_name, segment, full_place
                    ))
                });
            }
            let child = instance.fields.get_mut(segment).ok_or_else(|| {
                Diagnostic::new(format!(
                    "class `{}` has no field `{}` in MIR place `{}`",
                    instance.class_name, segment, full_place
                ))
            })?;
            take_nested_place(child, rest, full_place)
        }
        Value::Tuple(tuple) => {
            let index = segment.parse::<usize>().map_err(|_| {
                Diagnostic::new(format!(
                    "tuple projection `{segment}` is not a fixed position in MIR place `{full_place}`"
                ))
            })?;
            let child = tuple.elements.get_mut(index).ok_or_else(|| {
                Diagnostic::new(format!(
                    "tuple MIR place `{full_place}` has no element at index {index}"
                ))
            })?;
            if rest.is_empty() {
                Ok(std::mem::replace(child, Value::Unit))
            } else {
                take_nested_place(child, rest, full_place)
            }
        }
        _ => Err(Diagnostic::new(format!(
            "cannot move nested MIR place `{full_place}` from a non-instance value"
        ))),
    }
}

fn nested_place_mut<'a>(
    value: &'a mut Value,
    segments: &[String],
    full_place: &str,
) -> Result<&'a mut Value> {
    let Some((segment, rest)) = segments.split_first() else {
        return Ok(value);
    };
    let child = match value {
        Value::Instance(instance) => instance.fields.get_mut(segment).ok_or_else(|| {
            Diagnostic::new(format!(
                "class `{}` has no field `{}` in MIR place `{full_place}`",
                instance.class_name, segment
            ))
        })?,
        Value::Tuple(tuple) => {
            let index = segment.parse::<usize>().map_err(|_| {
                Diagnostic::new(format!(
                    "tuple projection `{segment}` is not a fixed position in MIR place `{full_place}`"
                ))
            })?;
            tuple.elements.get_mut(index).ok_or_else(|| {
                Diagnostic::new(format!(
                    "tuple MIR place `{full_place}` has no element at index {index}"
                ))
            })?
        }
        _ => {
            return Err(Diagnostic::new(format!(
                "cannot access nested MIR place `{full_place}` on a non-instance value"
            )))
        }
    };
    nested_place_mut(child, rest, full_place)
}

enum MirBorrowedOperand<'a> {
    Place(&'a Value),
    Immediate(Value),
}

impl MirBorrowedOperand<'_> {
    fn as_value(&self) -> &Value {
        match self {
            Self::Place(value) => value,
            Self::Immediate(value) => value,
        }
    }

    #[cfg(test)]
    fn is_borrowed_place(&self) -> bool {
        matches!(self, Self::Place(_))
    }
}

fn borrow_mir_operand<'a>(operand: &'a Operand, env: &'a Env) -> Result<MirBorrowedOperand<'a>> {
    Ok(match operand {
        Operand::Place(place) => MirBorrowedOperand::Place(env.place_ref(place)?),
        Operand::MovePlace(place) => {
            return Err(Diagnostic::new(format!(
                "cannot borrow consuming MIR operand `{place}`"
            )))
        }
        Operand::Function { name, signature } => {
            MirBorrowedOperand::Immediate(mir_function_value(name, signature))
        }
        Operand::Int(value) => {
            MirBorrowedOperand::Immediate(Value::Int(IntegerValue::from_literal(*value)))
        }
        Operand::Duration(value) => MirBorrowedOperand::Immediate(Value::Duration(*value)),
        Operand::Float(value) => MirBorrowedOperand::Immediate(Value::Float(*value)),
        Operand::Bool(value) => MirBorrowedOperand::Immediate(Value::Bool(*value)),
        Operand::String(value) => {
            #[cfg(test)]
            MIR_VALUE_CLONE_COUNT.with(|count| count.set(count.get() + 1));
            MirBorrowedOperand::Immediate(Value::String(value.clone()))
        }
        Operand::Unit => MirBorrowedOperand::Immediate(Value::Unit),
    })
}

fn mir_operand_is_array(operand: &Operand, env: &Env) -> Result<bool> {
    match operand {
        Operand::Place(place) | Operand::MovePlace(place) => {
            Ok(matches!(env.place_ref(place)?, Value::Array(_)))
        }
        _ => Ok(false),
    }
}

fn borrow_mir_string<'a>(operand: &'a Operand, env: &'a Env, call: &str) -> Result<&'a str> {
    match operand {
        Operand::Place(place) => match env.place_ref(place)? {
            Value::String(value) => Ok(value),
            other => Err(Diagnostic::coded(
                "AU4001",
                format!("`{call}` expects `str`, found `{}`", other.render()),
            )),
        },
        Operand::MovePlace(place) => Err(Diagnostic::new(format!(
            "cannot borrow consuming MIR operand `{place}` in `{call}`"
        ))),
        Operand::String(value) => Ok(value),
        _ => {
            let value = borrow_mir_operand(operand, env)?;
            Err(Diagnostic::coded(
                "AU4001",
                format!(
                    "`{call}` expects `str`, found `{}`",
                    value.as_value().render()
                ),
            ))
        }
    }
}

fn evaluate_borrowed_length_member(
    receiver: &Value,
    field: &str,
    args: &[MirArg],
) -> Option<Result<Value>> {
    let length = match (receiver, field) {
        (Value::String(text), "len") => text.chars().count(),
        (Value::String(text), "byte_len") => text.len(),
        (Value::Vec(vector), "len") => vector.elements.len(),
        (Value::Array(array), "len") => array.len(),
        (Value::Map(map), "len") => map.entries.len(),
        (Value::Set(set), "len") => set.elements.len(),
        _ => return None,
    };
    if !args.is_empty() {
        return Some(Err(Diagnostic::new(format!(
            "`{field}` does not take arguments"
        ))));
    }
    Some(Ok(Value::Int(IntegerValue::from_literal(length as u128))))
}

fn take_mir_operand(operand: &Operand, env: &mut Env) -> Result<Value> {
    match operand {
        Operand::Place(place) => env.read_place(place),
        Operand::MovePlace(place) => env.take_place(place),
        Operand::Function { name, signature } => Ok(mir_function_value(name, signature)),
        Operand::Int(value) => Ok(Value::Int(IntegerValue::from_literal(*value))),
        Operand::Duration(value) => Ok(Value::Duration(*value)),
        Operand::Float(value) => Ok(Value::Float(*value)),
        Operand::Bool(value) => Ok(Value::Bool(*value)),
        Operand::String(value) => Ok(Value::String(value.clone())),
        Operand::Unit => Ok(Value::Unit),
    }
}

fn bind_mir_arg_refs<'a>(expected_names: &[&str], args: &'a [MirArg]) -> Result<Vec<&'a MirArg>> {
    let mut values = vec![None; expected_names.len()];
    let mut next_positional = 0usize;
    for argument in args {
        if let Some(name) = argument.name.as_deref() {
            let Some(index) = expected_names
                .iter()
                .position(|candidate| *candidate == name)
            else {
                return Err(Diagnostic::new(format!("unknown MIR argument `{name}`")));
            };
            values[index] = Some(argument);
            continue;
        }
        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err(Diagnostic::new("too many MIR arguments"));
        }
        values[next_positional] = Some(argument);
        next_positional += 1;
    }
    values
        .into_iter()
        .map(|argument| argument.ok_or_else(|| Diagnostic::new("missing MIR argument")))
        .collect()
}

fn mir_json_variant<'a>(value: &'a Value, call: &str) -> Result<&'a EnumVariantValue> {
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

fn mir_json_exact_payload<'a>(
    value: &'a Value,
    expected_variant: &str,
    call: &str,
) -> Result<Option<&'a Value>> {
    let variant = mir_json_variant(value, call)?;
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

fn mir_json_into_exact_payload(
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

fn mir_json_indent(value: &Value) -> Result<Option<i64>> {
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
        ("Option", "Some", [Value::Int(value)]) if json_int_metadata_is_exact(value) => value
            .as_i128()
            .and_then(|value| i64::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                Diagnostic::coded(
                    "AU4001",
                    "`json::dumps` expects `indent` to contain an `int64`",
                )
            }),
        ("Option", "Some", [Value::Int(_)]) => Err(Diagnostic::coded(
            "AU4001",
            "`json::dumps` expects `indent` to contain an `int64`",
        )),
        _ => Err(Diagnostic::coded(
            "AU4001",
            "`json::dumps` expects `indent` to be `Option[int64]`",
        )),
    }
}

fn malformed_json_variant_metadata(expected: &str, call: &str) -> Diagnostic {
    Diagnostic::coded(
        "AU4001",
        format!("malformed runtime `json.Value.{expected}` payload in `{call}`"),
    )
}

fn evaluate_json_mir_host_call(
    name: &str,
    args: &[MirArg],
    env: &mut Env,
) -> Option<Result<Value>> {
    let result = match name {
        "json::parse" => {
            let bound = match bind_mir_arg_refs(&["text"], args) {
                Ok(bound) => bound,
                Err(error) => return Some(Err(error)),
            };
            let prepared = prepare_json_codec_source(|| {
                borrow_mir_string(&bound[0].value, env, name).and_then(clone_json_codec_source)
            });
            let (text, reservation) = match prepared {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            json_parse_owned_to_runtime(text, reservation)
        }
        "json::dumps" => {
            let bound = match bind_mir_arg_refs(&["value", "indent"], args) {
                Ok(bound) => bound,
                Err(error) => return Some(Err(error)),
            };
            let indent = match borrow_mir_operand(&bound[1].value, env) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            let indent = match mir_json_indent(indent.as_value()) {
                Ok(indent) => indent,
                Err(error) => return Some(Err(error)),
            };
            let value = match borrow_mir_operand(&bound[0].value, env) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            let converted = match runtime_value_to_json(value.as_value()) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            json_codec::dumps(&converted, indent)
                .map(Value::String)
                .map_err(json_dump_error_to_diagnostic)
        }
        "json::is_null" => {
            let bound = match bind_mir_arg_refs(&["value"], args) {
                Ok(bound) => bound,
                Err(error) => return Some(Err(error)),
            };
            let value = match borrow_mir_operand(&bound[0].value, env) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            let variant = match mir_json_variant(value.as_value(), name) {
                Ok(variant) => variant,
                Err(error) => return Some(Err(error)),
            };
            if variant.variant_name == "Null" && !variant.payloads.is_empty() {
                return Some(Err(Diagnostic::coded(
                    "AU4001",
                    "malformed runtime `json.Value.Null` payload in `json::is_null`",
                )));
            }
            Ok(Value::Bool(variant.variant_name == "Null"))
        }
        "json::as_bool" | "json::as_int" | "json::as_float" => {
            let bound = match bind_mir_arg_refs(&["value"], args) {
                Ok(bound) => bound,
                Err(error) => return Some(Err(error)),
            };
            let value = match borrow_mir_operand(&bound[0].value, env) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            let expected = match name {
                "json::as_bool" => "Bool",
                "json::as_int" => "Int",
                "json::as_float" => "Float",
                _ => unreachable!(),
            };
            match mir_json_exact_payload(value.as_value(), expected, name) {
                Ok(Some(Value::Bool(value))) if expected == "Bool" => {
                    Ok(option_some(Value::Bool(*value)))
                }
                Ok(Some(Value::Int(value)))
                    if expected == "Int" && json_int_metadata_is_exact(value) =>
                {
                    Ok(option_some(Value::Int(*value)))
                }
                Ok(Some(Value::Float(value))) if expected == "Float" => {
                    Ok(option_some(Value::Float(*value)))
                }
                Ok(Some(_)) => Err(malformed_json_variant_metadata(expected, name)),
                Ok(None) => Ok(option_none()),
                Err(error) => Err(error),
            }
        }
        "json::into_string" | "json::into_array" | "json::into_object" => {
            let bound = match bind_mir_arg_refs(&["value"], args) {
                Ok(bound) => bound,
                Err(error) => return Some(Err(error)),
            };
            let value = match take_mir_operand(&bound[0].value, env) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            let expected = match name {
                "json::into_string" => "String",
                "json::into_array" => "Array",
                "json::into_object" => "Object",
                _ => unreachable!(),
            };
            match mir_json_into_exact_payload(value, expected, name) {
                Ok(Some(Value::String(value))) if expected == "String" => {
                    Ok(option_some(Value::String(value)))
                }
                Ok(Some(Value::Vec(value)))
                    if expected == "Array" && json_array_metadata_is_exact(&value) =>
                {
                    Ok(option_some(Value::Vec(value)))
                }
                Ok(Some(Value::Map(value)))
                    if expected == "Object" && json_object_metadata_is_exact(&value) =>
                {
                    Ok(option_some(Value::Map(value)))
                }
                Ok(Some(_)) => Err(malformed_json_variant_metadata(expected, name)),
                Ok(None) => Ok(option_none()),
                Err(error) => Err(error),
            }
        }
        _ => return None,
    };
    Some(result)
}

fn evaluate_bytes_mir_host_call(name: &str, args: &[MirArg], env: &Env) -> Option<Result<Value>> {
    let expected_name = match name {
        "bytes::hex_encode" | "bytes::base64_encode" | "bytes::sha256" | "str.to_bytes" => "value",
        "bytes::hex_decode" | "bytes::base64_decode" | "bytes::sha256_string" => "text",
        "str.from_bytes" => "bytes",
        _ => return None,
    };
    let bound = match bind_mir_arg_refs(&[expected_name], args) {
        Ok(bound) => bound,
        Err(error) => return Some(Err(error)),
    };
    if name == "str.to_bytes" {
        if let Operand::String(text) = &bound[0].value {
            return Some(evaluate_string_to_bytes_host_ref(text));
        }
    }
    let value = match borrow_mir_operand(&bound[0].value, env) {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    evaluate_bytes_host_builtin_ref(name, value.as_value())
}

fn collect_queue_handles(value: &Value, queues: &mut Vec<ChannelValue>) {
    match value {
        Value::Channel(queue) => queues.push(queue.clone()),
        Value::Tuple(tuple) => {
            for element in &tuple.elements {
                collect_queue_handles(element, queues);
            }
        }
        Value::Vec(vector) => {
            for element in &vector.elements {
                collect_queue_handles(element, queues);
            }
        }
        Value::Set(set) => {
            for element in &set.elements {
                collect_queue_handles(element, queues);
            }
        }
        Value::Map(map) => {
            for (key, value) in &map.entries {
                collect_queue_handles(key, queues);
                collect_queue_handles(value, queues);
            }
        }
        Value::Instance(instance) => {
            for value in instance.fields.values() {
                collect_queue_handles(value, queues);
            }
        }
        Value::EnumVariant(variant) => {
            for payload in &variant.payloads {
                collect_queue_handles(payload, queues);
            }
        }
        _ => {}
    }
}

impl MirRuntime {
    #[cfg(test)]
    fn new(
        module: MirModule,
        stdout: Arc<Mutex<String>>,
        cancellation: CancellationContext,
    ) -> Self {
        Self::new_with_stdout_sink(module, stdout, None, cancellation)
    }

    #[cfg(test)]
    fn new_with_stdout_sink(
        module: MirModule,
        stdout: Arc<Mutex<String>>,
        stdout_sink: Option<StdoutSink>,
        cancellation: CancellationContext,
    ) -> Self {
        Self::new_with_stdout_sink_and_program_args(
            module,
            stdout,
            stdout_sink,
            cancellation,
            Arc::new(Vec::new()),
        )
    }

    fn new_with_stdout_sink_and_program_args(
        module: MirModule,
        stdout: Arc<Mutex<String>>,
        stdout_sink: Option<StdoutSink>,
        cancellation: CancellationContext,
        program_args: Arc<Vec<String>>,
    ) -> Self {
        let safepoints_enabled = module_uses_lightweight_tasks(&module);
        let mut functions = HashMap::new();
        for function in &module.functions {
            functions.insert(function.name.clone(), function.clone());
        }
        let mut classes = HashMap::new();
        for class in &module.classes {
            classes.insert(class.name.clone(), class.clone());
        }
        let trait_impls = module.trait_impls.clone();
        Self {
            module: Arc::new(module),
            safepoints_enabled,
            functions,
            classes,
            trait_impls,
            stdout,
            stdout_sink,
            cancellation,
            program_args,
            call_depth: 0,
            call_stack: Vec::new(),
            task_ancestry: Vec::new(),
            return_type_stack: Vec::new(),
            constant_states: Arc::new(Mutex::new(HashMap::new())),
            pending_returned_view_projection: None,
        }
    }

    fn read_module_constant(&mut self, key: &str, initializer: &str) -> Result<Arc<Value>> {
        {
            let states = self
                .constant_states
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match states.get(key) {
                Some(MirConstantState::Ready(value)) => return Ok(value.clone()),
                Some(MirConstantState::Failed(error)) => return Err(error.clone()),
                Some(MirConstantState::Initializing) => {
                    return Err(Diagnostic::coded(
                        "AU4001",
                        format!(
                        "module constant `{key}` was read while its module was still initializing"
                    ),
                    ))
                }
                None => {}
            }
        }
        self.constant_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_string(), MirConstantState::Initializing);
        let function = self.functions.get(initializer).cloned().ok_or_else(|| {
            Diagnostic::new(format!(
                "missing MIR initializer `{initializer}` for module constant `{key}`"
            ))
        })?;
        match self.call_function(&function, None, Vec::new()) {
            Ok(outcome) => {
                let stored = Arc::new(outcome.value);
                self.constant_states
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key.to_string(), MirConstantState::Ready(stored.clone()));
                Ok(stored)
            }
            Err(error) => {
                self.constant_states
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key.to_string(), MirConstantState::Failed(error.clone()));
                Err(error)
            }
        }
    }

    fn initialize_module_constants(&mut self) -> Result<()> {
        for constant in self.module.constants.clone() {
            let _ = self.read_module_constant(&constant.key, &constant.initializer)?;
        }
        Ok(())
    }

    fn cleanup_module_constants(&mut self) {
        let mut states = self
            .constant_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for constant in self.module.constants.iter().rev() {
            states.remove(&constant.key);
        }
    }

    fn annotate_runtime_trap_once(&self, mut error: Diagnostic) -> Diagnostic {
        error.capture_runtime_frames_once(
            self.call_stack.iter().rev().cloned().collect(),
            self.task_ancestry.iter().rev().cloned().collect(),
        );
        error
    }

    fn find_trait_impl_method(&self, receiver_ty: &Type, field: &str) -> Option<&MirMethod> {
        let mut best = None;
        let mut best_specificity = 0usize;
        for trait_impl in &self.trait_impls {
            let mut type_params = std::collections::BTreeSet::new();
            collect_type_params_from_type(&trait_impl.for_type, &mut type_params);
            let mut substitutions = HashMap::new();
            if !crate::sema::type_pattern_matches(
                &trait_impl.for_type,
                receiver_ty,
                &type_params,
                &mut substitutions,
            ) {
                continue;
            }
            for method in &trait_impl.methods {
                if method.name == field {
                    let specificity = crate::sema::trait_impl_specificity_parts(
                        &trait_impl.for_type,
                        &trait_impl.trait_args,
                    );
                    if best.is_none() || specificity > best_specificity {
                        best = Some(method);
                        best_specificity = specificity;
                    }
                }
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_resolved_trait_method_call(
        &mut self,
        receiver: Value,
        receiver_ty: &Type,
        field: &str,
        method: MirMethod,
        receiver_place: Option<&str>,
        args: &[MirArg],
        expected_return_type: Option<&Type>,
        env: &mut Env,
    ) -> Result<Value> {
        let function = self
            .functions
            .get(&method.function_name)
            .cloned()
            .ok_or_else(|| {
                Diagnostic::new(format!(
                    "unknown MIR method body `{}`",
                    method.function_name
                ))
            })?;
        let evaluated_args = evaluate_named_args(args, env)?;
        let writeback_places = evaluated_args
            .iter()
            .map(|argument| argument.writeback_place.clone())
            .collect::<Vec<_>>();
        let outcome = self.call_function_with_receiver_type(
            &function,
            Some(receiver),
            evaluated_args,
            expected_return_type,
            None,
            Some(receiver_ty),
        )?;
        if method.receiver == Some(MirReceiverKind::BorrowMut) {
            let updated = outcome.updated_receiver.ok_or_else(|| {
                Diagnostic::new(format!(
                    "mutable MIR method `{}` did not return an updated receiver",
                    field
                ))
            })?;
            if let Some(place) = receiver_place {
                env.write_place(place, updated)?;
            }
        }
        self.apply_borrowed_param_writebacks(
            &function.params,
            &writeback_places,
            outcome.updated_params,
            env,
        )?;
        Ok(outcome.value)
    }

    fn find_from_trait_impl_method(
        &self,
        source_ty: &Type,
        target_ty: &Type,
    ) -> Option<MirFunction> {
        let mut best = None;
        let mut best_specificity = 0usize;
        for trait_impl in &self.trait_impls {
            if trait_impl.trait_name != "From" || trait_impl.trait_args.len() != 1 {
                continue;
            }
            let mut type_params = std::collections::BTreeSet::new();
            collect_type_params_from_type(&trait_impl.for_type, &mut type_params);
            for trait_arg in &trait_impl.trait_args {
                collect_type_params_from_type(trait_arg, &mut type_params);
            }
            let mut substitutions = HashMap::new();
            if !crate::sema::type_pattern_matches(
                &trait_impl.for_type,
                target_ty,
                &type_params,
                &mut substitutions,
            ) {
                continue;
            }
            if crate::sema::substitute_type(&trait_impl.trait_args[0], &substitutions) != *source_ty
            {
                continue;
            }
            for method in &trait_impl.methods {
                if method.name == "from" {
                    if let Some(function) = self.functions.get(&method.function_name) {
                        let specificity = crate::sema::trait_impl_specificity_parts(
                            &trait_impl.for_type,
                            &trait_impl.trait_args,
                        );
                        if best.is_none() || specificity > best_specificity {
                            best = Some(function.clone());
                            best_specificity = specificity;
                        }
                    }
                }
            }
        }
        best
    }

    fn current_return_type(&self) -> Option<&Type> {
        self.return_type_stack.last()
    }

    fn convert_try_error_via_from(&mut self, payload: Value, source_ty: &Type) -> Result<Value> {
        let Some(Type::Named(return_name, return_args)) = self.current_return_type() else {
            return Err(Diagnostic::new(
                "MIR `try` is only allowed inside a function returning `Result`",
            ));
        };
        if return_name != "Result" || return_args.len() != 2 {
            return Err(Diagnostic::new(
                "MIR `try` is only allowed inside a function returning `Result`",
            ));
        }
        let target_error_ty = return_args[1].clone();
        if source_ty == &target_error_ty {
            return Ok(payload);
        }
        let Some(function) = self.find_from_trait_impl_method(source_ty, &target_error_ty) else {
            return Err(Diagnostic::new(format!(
                "`try` error type `{}` does not match enclosing `Result` error type `{}`",
                source_ty, target_error_ty
            )));
        };
        let outcome = self.call_function_for_target(
            &function,
            None,
            vec![EvaluatedMirArg {
                name: None,
                value: payload,
                ty: Some(source_ty.clone()),
                writeback_place: None,
            }],
            None,
        )?;
        Ok(outcome.value)
    }

    fn find_trait_impl_method_for_class_name(
        &self,
        class_name: &str,
        field: &str,
    ) -> Option<&MirMethod> {
        let mut best = None;
        let mut best_specificity = 0usize;
        let mut ambiguous = false;
        for trait_impl in &self.trait_impls {
            match &trait_impl.for_type {
                Type::Named(name, _) if name == class_name => {
                    for method in &trait_impl.methods {
                        if method.name == field {
                            let specificity = crate::sema::trait_impl_specificity_parts(
                                &trait_impl.for_type,
                                &trait_impl.trait_args,
                            );
                            if best.is_none() || specificity > best_specificity {
                                best = Some(method);
                                best_specificity = specificity;
                                ambiguous = false;
                            } else if specificity == best_specificity {
                                ambiguous = true;
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        if ambiguous {
            None
        } else {
            best
        }
    }

    /// Enters the module at `entry`, or at its ordinary entry point when
    /// `entry` is absent.
    fn run_entry(&mut self, entry: Option<&str>) -> Result<Value> {
        if let Err(error) = self.initialize_module_constants() {
            self.cleanup_module_constants();
            return Err(error);
        }
        let result = if let Some(entry) = entry {
            let function = self.functions.get(entry).cloned().ok_or_else(|| {
                Diagnostic::new(format!("no entry function named `{entry}` was found"))
            });
            function.and_then(|function| {
                self.call_function(&function, None, Vec::new())
                    .map(|outcome| outcome.value)
            })
        } else {
            self.run_main()
        };
        self.cleanup_module_constants();
        result
    }

    fn run_main(&mut self) -> Result<Value> {
        if let Some(main_fn) = self.functions.get("main").cloned() {
            return Ok(self.call_function(&main_fn, None, Vec::new())?.value);
        }

        let Some(top_level) = self.module.top_level.clone() else {
            return Err(Diagnostic::new(
                "no `main` function or top-level script statements were found",
            ));
        };
        Ok(self.call_function(&top_level, None, Vec::new())?.value)
    }

    fn infer_value_type(value: &Value) -> Option<Type> {
        match value {
            Value::Int(value) => Some(Type::named(value.runtime_type_name().unwrap_or("int64"))),
            Value::Float(_) => Some(Type::named("float64")),
            Value::Bool(_) => Some(Type::named("bool")),
            Value::String(_) => Some(Type::named("str")),
            Value::Tuple(tuple) => Some(Type::Tuple(tuple.element_types.clone())),
            Value::Vec(vector) => Some(Type::Named(
                "list".to_string(),
                vec![vector.element_type.clone()],
            )),
            Value::Array(array) => {
                Some(Type::Named("Array".to_string(), vec![array.element_type()]))
            }
            Value::Set(set) => Some(Type::Named(
                "set".to_string(),
                vec![set.element_type.clone()],
            )),
            Value::Map(map) => Some(Type::Named(
                "dict".to_string(),
                vec![map.key_type.clone(), map.value_type.clone()],
            )),
            Value::Duration(_) => Some(Type::named("Duration")),
            Value::Rng(_) => Some(Type::named("random.Rng")),
            Value::Range(_) => Some(Type::named("Range")),
            Value::ModuleNamespace(_) => None,
            Value::Function(function) => Some(function.signature.clone()),
            Value::Unit => Some(Type::Unit),
            Value::FfiHandle(handle) => Some(Type::named(handle.type_name())),
            Value::Instance(instance) => Some(Type::named(&instance.class_name)),
            Value::EnumVariant(variant) => match (
                variant.enum_name.as_str(),
                variant.variant_name.as_str(),
                variant.single_payload(),
            ) {
                ("Option", "Some", Some(payload)) => Self::infer_value_type(payload)
                    .map(|inner| Type::Named("Option".to_string(), vec![inner])),
                ("Option", "None", _) => Some(Type::Named("Option".to_string(), vec![Type::Unit])),
                ("Result", "Ok", Some(payload)) => Self::infer_value_type(payload)
                    .map(|ok| Type::Named("Result".to_string(), vec![ok, Type::Unit])),
                ("Result", "Err", Some(payload)) => Self::infer_value_type(payload)
                    .map(|err| Type::Named("Result".to_string(), vec![Type::Unit, err])),
                ("SendError", "Closed" | "Cancelled", Some(payload)) => {
                    Self::infer_value_type(payload)
                        .map(|inner| Type::Named("SendError".to_string(), vec![inner]))
                }
                ("Stdio", _, _) => Some(Type::Named("process.Stdio".to_string(), Vec::new())),
                ("ExitStatus", _, _) => {
                    Some(Type::Named("process.ExitStatus".to_string(), Vec::new()))
                }
                ("Wait", _, _) => Some(Type::Named("process.Wait".to_string(), Vec::new())),
                ("Error", _, _) => Some(Type::Named("process.Error".to_string(), Vec::new())),
                _ => Some(Type::named(&variant.enum_name)),
            },
            Value::Channel(_) | Value::Task(_) | Value::TaskGroup(_) => None,
            Value::File(_) => Some(Type::Named("fs.File".to_string(), Vec::new())),
            Value::TcpListener(_) => Some(Type::Named("net.TcpListener".to_string(), Vec::new())),
            Value::TcpStream(_) => Some(Type::Named("net.TcpStream".to_string(), Vec::new())),
            Value::UdpSocket(_) => Some(Type::Named("net.UdpSocket".to_string(), Vec::new())),
            Value::UdpDatagram(_) => Some(Type::Named("net.UdpDatagram".to_string(), Vec::new())),
            Value::HttpListener(_) => Some(Type::Named("net.HttpListener".to_string(), Vec::new())),
            Value::HttpExchange(_) => Some(Type::Named("net.HttpExchange".to_string(), Vec::new())),
            Value::HttpResponse(_) => Some(Type::Named("net.HttpResponse".to_string(), Vec::new())),
            Value::WebSocketListener(_) => {
                Some(Type::Named("net.WebSocketListener".to_string(), Vec::new()))
            }
            Value::WebSocket(_) => Some(Type::Named("net.WebSocket".to_string(), Vec::new())),
            Value::UnixListener(_) => Some(Type::Named("net.UnixListener".to_string(), Vec::new())),
            Value::UnixStream(_) => Some(Type::Named("net.UnixStream".to_string(), Vec::new())),
            Value::TlsListener(_) => Some(Type::Named("net.TlsListener".to_string(), Vec::new())),
            Value::TlsStream(_) => Some(Type::Named("net.TlsStream".to_string(), Vec::new())),
            Value::ProcessChild(_) => Some(Type::Named("process.Child".to_string(), Vec::new())),
            Value::ProcessPipe(_) => Some(Type::Named("process.Pipe".to_string(), Vec::new())),
            Value::ProcessCompleted(_) => {
                Some(Type::Named("process.Completed".to_string(), Vec::new()))
            }
            Value::ProcessSupervisor(_) => {
                Some(Type::Named("process.Supervisor".to_string(), Vec::new()))
            }
        }
    }

    fn infer_instance_type(&self, instance: &InstanceValue) -> Option<Type> {
        let class = self.classes.get(&instance.class_name)?;
        if class.type_params.is_empty() {
            return Some(Type::named(&instance.class_name));
        }

        let mut substitutions = HashMap::new();
        for field in &class.fields {
            let actual_value = instance.fields.get(&field.name)?;
            let actual_ty = self.infer_runtime_value_type(actual_value)?;
            collect_runtime_type_substitutions(&field.ty, &actual_ty, &mut substitutions);
        }

        let resolved_args = class
            .type_params
            .iter()
            .map(|type_param| match substitutions.get(type_param).cloned() {
                Some(ty) => ty,
                None => Type::named("Unknown"),
            })
            .collect();
        Some(Type::Named(instance.class_name.clone(), resolved_args))
    }

    fn infer_runtime_value_type(&self, value: &Value) -> Option<Type> {
        match value {
            Value::Instance(instance) => self.infer_instance_type(instance),
            Value::EnumVariant(_variant) => Self::infer_value_type(value).map(|ty| match ty {
                Type::Named(name, args) if name == "Option" && args == vec![Type::Unit] => {
                    Type::Named(name, vec![Type::named("Unknown")])
                }
                Type::Named(name, args) if name == "Result" && args.contains(&Type::Unit) => {
                    Type::Named(
                        name,
                        args.into_iter()
                            .map(|arg| {
                                if arg == Type::Unit {
                                    Type::named("Unknown")
                                } else {
                                    arg
                                }
                            })
                            .collect(),
                    )
                }
                other => other,
            }),
            _ => Self::infer_value_type(value),
        }
    }

    fn validate_value_fits_type(
        &self,
        value: &Value,
        ty: &Type,
        span: Option<crate::diag::Span>,
    ) -> Result<()> {
        if let Some(bounds) = crate::sema::integer_type_bounds(ty) {
            let Value::Int(value) = value else {
                return Ok(());
            };
            if !value.fits_bounds(bounds) {
                let message = format!("integer value `{}` does not fit in `{}`", value, ty);
                return Err(match span {
                    Some(span) => Diagnostic::at(span, message),
                    None => Diagnostic::new(message),
                });
            }
        }
        Ok(())
    }

    fn coerce_value_to_type(
        &self,
        value: Value,
        ty: &Type,
        span: Option<crate::diag::Span>,
    ) -> Result<Value> {
        let coerced = match (&value, ty) {
            (Value::Unit, Type::Named(name, args)) if name == "Option" && args.len() == 1 => {
                option_none()
            }
            (Value::Int(value), Type::Named(name, args))
                if args.is_empty() && (name.starts_with("int") || name.starts_with("uint")) =>
            {
                match IntegerKind::from_runtime_type_name(name)
                    .and_then(|kind| (*value).with_runtime_kind(kind))
                {
                    Some(value) => Value::Int(value),
                    None => Value::Int(*value),
                }
            }
            (Value::Float(_), Type::Named(name, _)) if name == "float32" || name == "float64" => {
                cast_numeric_value(value, ty, span)?
            }
            (Value::Int(_), Type::Named(name, _)) if name == "float32" || name == "float64" => {
                cast_numeric_value(value, ty, span)?
            }
            (Value::Float(_), Type::Named(name, _))
                if name.starts_with("int") || name.starts_with("uint") =>
            {
                cast_numeric_value(value, ty, span)?
            }
            (Value::Tuple(_), Type::Tuple(element_types)) => {
                let Value::Tuple(tuple) = value else {
                    unreachable!("tuple coercion arm validates the runtime value")
                };
                if tuple.elements.len() != element_types.len() {
                    return Err(Diagnostic::new(format!(
                        "tuple value has {} elements but target type expects {}",
                        tuple.elements.len(),
                        element_types.len()
                    )));
                }
                let elements = tuple
                    .elements
                    .into_iter()
                    .zip(element_types)
                    .map(|(element, element_ty)| {
                        self.coerce_value_to_type(element, element_ty, span)
                    })
                    .collect::<Result<Vec<_>>>()?;
                Value::Tuple(TupleValue {
                    element_types: element_types.clone(),
                    elements,
                })
            }
            (Value::Function(function), Type::Function { .. }) => {
                let mut function = function.clone();
                function.signature = ty.clone();
                Value::Function(function)
            }
            (Value::Function(function), Type::Closure { .. }) => {
                let mut function = function.clone();
                function.signature = ty.clone();
                Value::Function(function)
            }
            _ => value,
        };
        self.validate_value_fits_type(&coerced, ty, span)?;
        Ok(coerced)
    }

    fn resolve_place_type(&self, place: &str, env: &Env) -> Option<Type> {
        let segments = split_place_segments(place).ok()?;
        let (root, rest) = segments.split_first()?;
        let mut current = if let Some(ty) = env.place_type(root).cloned() {
            ty
        } else {
            let value = env.read_place(root).ok()?;
            self.infer_runtime_value_type(&value)?
        };

        let mut index = 0usize;
        while index < rest.len() {
            let segment = &rest[index];
            let Type::Named(class_name, args) = current else {
                return None;
            };
            let class = self.classes.get(&class_name)?;
            let field = class.fields.iter().find(|field| field.name == *segment)?;
            let substitutions = class
                .type_params
                .iter()
                .cloned()
                .zip(args)
                .collect::<HashMap<_, _>>();
            current = substitute_type(&field.ty, &substitutions);
            index += 1;
        }

        Some(current)
    }

    fn resolve_operand_type(&self, operand: &Operand, env: &Env) -> Option<Type> {
        match operand {
            Operand::Place(place) | Operand::MovePlace(place) => {
                self.resolve_place_type(place, env)
            }
            Operand::Function { signature, .. } => Some(signature.as_ref().clone()),
            Operand::Int(_) => Some(Type::named("int64")),
            Operand::Duration(_) => Some(Type::named("Duration")),
            Operand::Float(_) => Some(Type::named("float64")),
            Operand::Bool(_) => Some(Type::named("bool")),
            Operand::String(_) => Some(Type::named("str")),
            Operand::Unit => Some(Type::Unit),
        }
    }

    fn call_function(
        &mut self,
        function: &MirFunction,
        receiver: Option<Value>,
        args: Vec<EvaluatedMirArg>,
    ) -> Result<CallOutcome> {
        self.call_function_for_target(function, receiver, args, None)
    }

    fn call_function_for_target(
        &mut self,
        function: &MirFunction,
        receiver: Option<Value>,
        args: Vec<EvaluatedMirArg>,
        expected_return_type: Option<&Type>,
    ) -> Result<CallOutcome> {
        self.call_function_with_receiver_type(
            function,
            receiver,
            args,
            expected_return_type,
            None,
            None,
        )
    }

    fn call_function_for_value(
        &mut self,
        function: &MirFunction,
        args: Vec<EvaluatedMirArg>,
        signature: &Type,
        expected_return_type: Option<&Type>,
    ) -> Result<CallOutcome> {
        self.call_function_with_receiver_type(
            function,
            None,
            args,
            expected_return_type,
            Some(signature),
            None,
        )
    }

    fn call_function_with_receiver_type(
        &mut self,
        function: &MirFunction,
        receiver: Option<Value>,
        args: Vec<EvaluatedMirArg>,
        expected_return_type: Option<&Type>,
        concrete_function_type: Option<&Type>,
        receiver_type: Option<&Type>,
    ) -> Result<CallOutcome> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(Diagnostic::at(
                function.span,
                format!(
                    "maximum call depth of {} exceeded while calling `{}`",
                    MAX_CALL_DEPTH, function.name
                ),
            ));
        }
        // Supplied arguments have already been evaluated in source order.
        // Bind them to declaration slots, then evaluate target-owned defaults
        // freshly in declaration order before entering the selected function's
        // call frame.
        let bound_args =
            self.bind_function_args(function, args, expected_return_type, concrete_function_type)?;
        let public_function_name = public_runtime_function_name(&function.name);
        self.call_stack.push(RuntimeCallFrame {
            function: public_function_name,
            span: RuntimeSourceSpan::point(function.source_path.clone(), function.span),
        });
        self.call_depth += 1;
        let outcome = (|| {
            let mut substitutions = HashMap::new();
            if let Some(receiver_type) = receiver_type {
                if let Some(receiver_local) = function
                    .local_types
                    .iter()
                    .find(|local| local.name == "self")
                {
                    collect_runtime_type_substitutions(
                        &receiver_local.ty,
                        receiver_type,
                        &mut substitutions,
                    );
                }
            }
            if let Some(expected_return_type) = expected_return_type {
                collect_runtime_type_substitutions(
                    &function.return_type,
                    expected_return_type,
                    &mut substitutions,
                );
            }
            collect_function_signature_substitutions(
                function,
                concrete_function_type,
                &mut substitutions,
            );
            for (param, argument) in function.params.iter().zip(bound_args.iter()) {
                if let Some(actual_ty) = argument
                    .ty
                    .clone()
                    .or_else(|| self.infer_runtime_value_type(&argument.value))
                {
                    collect_runtime_type_substitutions(&param.ty, &actual_ty, &mut substitutions);
                }
            }

            let mut env = Env::default();
            for local in &function.local_types {
                env.set_place_type(&local.name, substitute_type(&local.ty, &substitutions));
            }
            if function.receiver.is_some() {
                let Some(receiver) = receiver else {
                    return Err(Diagnostic::new(format!(
                        "MIR function `{}` is missing its receiver",
                        function.name
                    )));
                };
                let receiver_ty = receiver_type
                    .cloned()
                    .or_else(|| self.infer_runtime_value_type(&receiver))
                    .unwrap_or(Type::named("Unknown"));
                env.define_typed("self", receiver_ty, receiver);
            }

            for (param, argument) in function.params.iter().zip(bound_args) {
                let ty = substitute_type(&param.ty, &substitutions);
                let value = self.coerce_value_to_type(argument.value, &ty, None)?;
                env.define_typed(&param.name, ty, value);
            }

            let return_type = substitute_type(&function.return_type, &substitutions);
            self.return_type_stack.push(return_type);
            let value_result = self.execute_function(function, &mut env);
            self.return_type_stack.pop();
            let value = value_result?;
            let updated_receiver = if function.receiver == Some(MirReceiverKind::BorrowMut) {
                Some(env.read_place("self")?)
            } else {
                None
            };
            let mut updated_params = Vec::new();
            for (index, param) in function.params.iter().enumerate() {
                if param.passing == MirReceiverKind::BorrowMut {
                    updated_params.push((index, env.read_place(&param.name)?));
                }
            }
            Ok(CallOutcome {
                value,
                updated_receiver,
                updated_params,
            })
        })();
        self.call_depth -= 1;
        let outcome = outcome.map_err(|error| self.annotate_runtime_trap_once(error));
        self.call_stack.pop();
        outcome
    }

    fn bind_function_args(
        &mut self,
        function: &MirFunction,
        args: Vec<EvaluatedMirArg>,
        expected_return_type: Option<&Type>,
        concrete_function_type: Option<&Type>,
    ) -> Result<Vec<EvaluatedMirArg>> {
        let mut bound = bind_optional_function_args(&function.params, args)?;
        let mut substitutions = HashMap::new();
        if let Some(expected_return_type) = expected_return_type {
            collect_runtime_type_substitutions(
                &function.return_type,
                expected_return_type,
                &mut substitutions,
            );
        }
        collect_function_signature_substitutions(
            function,
            concrete_function_type,
            &mut substitutions,
        );
        for (param, argument) in function.params.iter().zip(bound.iter()) {
            let Some(argument) = argument else {
                continue;
            };
            if let Some(actual_ty) = argument
                .ty
                .clone()
                .or_else(|| self.infer_runtime_value_type(&argument.value))
            {
                collect_runtime_type_substitutions(&param.ty, &actual_ty, &mut substitutions);
            }
        }
        for (index, param) in function.params.iter().enumerate() {
            if bound[index].is_some() {
                continue;
            }
            let default_name = param.default_function.as_ref().ok_or_else(|| {
                Diagnostic::new(format!(
                    "missing MIR argument `{}` for function `{}`",
                    param.name, function.name
                ))
            })?;
            let default_function = self.functions.get(default_name).cloned().ok_or_else(|| {
                Diagnostic::new(format!(
                    "unknown MIR default function `{default_name}` for `{}`",
                    function.name
                ))
            })?;
            let expected = substitute_type(&param.ty, &substitutions);
            let value = self
                .call_function_for_target(&default_function, None, Vec::new(), Some(&expected))?
                .value;
            bound[index] = Some(EvaluatedMirArg {
                name: Some(param.name.clone()),
                value,
                ty: Some(expected),
                writeback_place: None,
            });
        }
        Ok(bound
            .into_iter()
            .map(|argument| argument.expect("every function argument is supplied or has a default"))
            .collect())
    }

    fn execute_function(&mut self, function: &MirFunction, env: &mut Env) -> Result<Value> {
        let block_map = function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.label.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut current_label = function.entry.clone();
        let mut loop_state = HashMap::<String, i128>::new();
        let mut cleanup_stack = Vec::<String>::new();
        let mut safepoint_fuel = MIR_LOOP_SAFEPOINT_INTERVAL;

        loop {
            let block_index = block_map.get(&current_label).copied().ok_or_else(|| {
                Diagnostic::new(format!(
                    "unknown MIR block `{}` in function `{}`",
                    current_label, function.name
                ))
            })?;
            let block = &function.blocks[block_index];
            for instruction in &block.instructions {
                match self.execute_instruction(
                    instruction,
                    env,
                    &mut cleanup_stack,
                    &mut safepoint_fuel,
                ) {
                    Ok(Some(value)) => {
                        self.unwind_cleanups(&mut cleanup_stack, env, true)?;
                        return Ok(value);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let error = self.annotate_runtime_trap_once(error);
                        let _ = self.unwind_cleanups(&mut cleanup_stack, env, true);
                        return Err(error);
                    }
                }
            }

            let outcome = match self.execute_terminator(
                &block.label,
                &block.terminator,
                env,
                &mut loop_state,
                &mut cleanup_stack,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let error = self.annotate_runtime_trap_once(error);
                    let _ = self.unwind_cleanups(&mut cleanup_stack, env, true);
                    return Err(error);
                }
            };
            match outcome {
                BlockOutcome::Return(value) => {
                    self.unwind_cleanups(&mut cleanup_stack, env, true)?;
                    return Ok(value);
                }
                BlockOutcome::Goto(next) => {
                    Self::clear_exited_for_range_states(
                        function,
                        &block.label,
                        &next,
                        &mut loop_state,
                    );
                    current_label = next;
                }
            }
        }
    }

    fn clear_exited_for_range_states(
        function: &MirFunction,
        current_label: &str,
        next_label: &str,
        loop_state: &mut HashMap<String, i128>,
    ) {
        for block in &function.blocks {
            let Terminator::ForRange { exit_label, .. } = &block.terminator else {
                continue;
            };
            if block.label != current_label && exit_label == next_label {
                loop_state.remove(&block.label);
            }
        }
    }

    fn execute_instruction(
        &mut self,
        instruction: &Instruction,
        env: &mut Env,
        cleanup_stack: &mut Vec<String>,
        safepoint_fuel: &mut u64,
    ) -> Result<Option<Value>> {
        match instruction {
            Instruction::Safepoint => {
                if self.safepoints_enabled {
                    *safepoint_fuel -= 1;
                    if *safepoint_fuel == 0 {
                        *safepoint_fuel = MIR_LOOP_SAFEPOINT_INTERVAL;
                        yield_now_with_runtime_scheduler();
                    }
                }
                Ok(None)
            }
            Instruction::BeginLoan {
                loan,
                source,
                mutable,
            } => {
                env.begin_loan(loan, source, *mutable)?;
                Ok(None)
            }
            Instruction::BeginReturnedLoan {
                loan,
                origin,
                projections,
                mutable,
            } => {
                let projection = self
                    .pending_returned_view_projection
                    .take()
                    .ok_or_else(|| {
                        Diagnostic::new(format!(
                            "MIR returned loan `{loan}` has no transferred projection"
                        ))
                    })?;
                if !projections.contains(&projection) {
                    return Err(Diagnostic::new(format!(
                        "MIR returned loan `{loan}` selected undeclared projection `{projection}`"
                    )));
                }
                let source = if projection.is_empty() {
                    origin.clone()
                } else {
                    format!("{origin}.{projection}")
                };
                env.begin_loan(loan, &source, *mutable)?;
                Ok(None)
            }
            Instruction::Reborrow {
                loan,
                parent,
                projection,
                mutable,
            } => {
                let source = if projection.is_empty() {
                    parent.clone()
                } else {
                    format!("{parent}.{projection}")
                };
                env.begin_loan(loan, &source, *mutable)?;
                Ok(None)
            }
            Instruction::ReadLoan { target, loan } => {
                let value = env.read_place(loan)?;
                env.write_place(target, value)?;
                Ok(None)
            }
            Instruction::WriteLoan { loan, value } => {
                let target_ty = self.resolve_place_type(loan, env);
                let evaluated =
                    match self.evaluate_rvalue_for_target(value, env, target_ty.as_ref())? {
                        RvalueOutcome::Value(value) => value,
                        RvalueOutcome::SharedModuleConstant(value) => {
                            try_clone_mir_value(value.as_ref())?
                        }
                        RvalueOutcome::Return(value) => return Ok(Some(value)),
                    };
                let evaluated = if let Some(target_ty) = target_ty {
                    self.coerce_value_to_type(evaluated, &target_ty, None)?
                } else {
                    evaluated
                };
                env.write_place(loan, evaluated)?;
                Ok(None)
            }
            Instruction::EndLoan { loan } => {
                env.end_loan(loan)?;
                Ok(None)
            }
            Instruction::ReturnLoan { loan, origin } => {
                self.pending_returned_view_projection =
                    Some(env.returned_view_projection(loan, origin)?);
                Ok(None)
            }
            Instruction::Assign { target, value } => {
                let target_ty = self.resolve_place_type(target, env);
                match self.evaluate_rvalue_for_target(value, env, target_ty.as_ref())? {
                    RvalueOutcome::Value(evaluated) => {
                        let span = match value {
                            Rvalue::Unary { span, .. } | Rvalue::Binary { span, .. } => Some(*span),
                            _ => None,
                        };
                        if let Some(target_ty) = target_ty {
                            let evaluated =
                                self.coerce_value_to_type(evaluated, &target_ty, span)?;
                            if !target.contains('.') {
                                env.set_place_type(target, target_ty);
                            }
                            env.write_place(target, evaluated)?;
                            return Ok(None);
                        } else if !target.contains('.') {
                            if let Some(inferred_ty) = self.infer_runtime_value_type(&evaluated) {
                                env.set_place_type(target, inferred_ty);
                            }
                        }
                        env.write_place(target, evaluated)?;
                        Ok(None)
                    }
                    RvalueOutcome::SharedModuleConstant(evaluated) => {
                        if let Some(target_ty) = target_ty {
                            if !target.contains('.') {
                                env.set_place_type(target, target_ty);
                            }
                        }
                        env.write_shared_place(target, evaluated)?;
                        Ok(None)
                    }
                    RvalueOutcome::Return(value) => Ok(Some(value)),
                }
            }
            Instruction::Eval { value } => {
                let _ = self.evaluate_owned_operand(value, env)?;
                Ok(None)
            }
            Instruction::PushCleanup { place } => {
                cleanup_stack.push(place.clone());
                Ok(None)
            }
            Instruction::PopCleanup {
                place,
                cancel_before_cleanup,
            } => {
                self.pop_cleanup(place, cleanup_stack, env, *cancel_before_cleanup)?;
                Ok(None)
            }
        }
    }

    fn execute_terminator(
        &mut self,
        block_label: &str,
        terminator: &Terminator,
        env: &mut Env,
        loop_state: &mut HashMap<String, i128>,
        _cleanup_stack: &mut Vec<String>,
    ) -> Result<BlockOutcome> {
        match terminator {
            Terminator::Return(value) => Ok(BlockOutcome::Return(
                self.evaluate_owned_operand(value, env)?,
            )),
            Terminator::Goto(label) => Ok(BlockOutcome::Goto(label.clone())),
            Terminator::Branch {
                condition,
                then_label,
                else_label,
            } => match self.evaluate_operand(condition, env)? {
                Value::Bool(true) => Ok(BlockOutcome::Goto(then_label.clone())),
                Value::Bool(false) => Ok(BlockOutcome::Goto(else_label.clone())),
                other => Err(Diagnostic::new(format!(
                    "MIR branch condition must evaluate to `bool`, found `{}`",
                    other.render()
                ))),
            },
            Terminator::ForRange {
                binding,
                iterable,
                body_label,
                exit_label,
            } => {
                let iterable = self.evaluate_operand(iterable, env)?;
                let Value::Range(range) = iterable else {
                    return Err(Diagnostic::new(format!(
                        "MIR `for` requires a `Range`, found `{}`",
                        iterable.render()
                    )));
                };
                let next = loop_state
                    .entry(block_label.to_string())
                    .or_insert(range.start);
                if *next < range.end {
                    let current = *next;
                    *next += 1;
                    env.write_place(binding, Value::Int(IntegerValue::from_signed(current)))?;
                    Ok(BlockOutcome::Goto(body_label.clone()))
                } else {
                    loop_state.remove(block_label);
                    Ok(BlockOutcome::Goto(exit_label.clone()))
                }
            }
            Terminator::Match {
                scrutinee,
                arms,
                otherwise,
            } => {
                let scrutinee = self.evaluate_operand(scrutinee, env)?;
                let Value::EnumVariant(variant) = scrutinee else {
                    return Err(Diagnostic::new(format!(
                        "MIR `match` expected an enum value, found `{}`",
                        scrutinee.render()
                    )));
                };
                for arm in arms {
                    if arm.wildcard
                        || (arm.enum_name.is_none()
                            && arm.variant_name.as_deref() == Some(variant.variant_name.as_str()))
                        || (arm.enum_name.as_deref() == Some(variant.enum_name.as_str())
                            && arm.variant_name.as_deref() == Some(variant.variant_name.as_str()))
                    {
                        return Ok(BlockOutcome::Goto(arm.label.clone()));
                    }
                }
                Ok(BlockOutcome::Goto(otherwise.clone()))
            }
            Terminator::AssertFail {
                message,
                captures,
                span,
            } => {
                if !matches!(captures.len(), 0 | 2) {
                    return Err(Diagnostic::coded_at(
                        "AU4001",
                        *span,
                        format!(
                            "MIR assertion captures must contain zero or two operands, found {}",
                            captures.len()
                        ),
                    ));
                }
                if let [left, right] = captures.as_slice() {
                    let labels = (left.label.as_str(), right.label.as_str());
                    if !matches!(labels, ("left", "right") | ("item", "collection")) {
                        return Err(Diagnostic::coded_at(
                            "AU4001",
                            *span,
                            format!(
                                "MIR assertion captures use invalid labels `{}` and `{}`",
                                left.label, right.label,
                            ),
                        ));
                    }
                }
                let mut rendered_captures = Vec::with_capacity(captures.len());
                for capture in captures {
                    let rendered = match self.evaluate_owned_operand(&capture.value, env)? {
                        Value::String(rendered) => rendered,
                        other => {
                            return Err(Diagnostic::coded_at(
                                "AU4001",
                                *span,
                                format!(
                                    "MIR assertion capture `{}` must evaluate to rendered `str`, found `{}`",
                                    capture.label,
                                    other.render()
                                ),
                            ))
                        }
                    };
                    rendered_captures.push((capture, rendered));
                }
                let message = match message {
                    Some(message) => match self.evaluate_owned_operand(message, env)? {
                        Value::String(message) => message,
                        other => {
                            return Err(Diagnostic::coded_at(
                                "AU4001",
                                *span,
                                format!(
                                    "MIR assertion message must evaluate to `str`, found `{}`",
                                    other.render()
                                ),
                            ))
                        }
                    },
                    None => "assertion failed".to_string(),
                };
                let mut diagnostic = Diagnostic::coded_at("AU4001", *span, message);
                for (capture, rendered) in rendered_captures {
                    diagnostic = diagnostic.with_assertion_operand(
                        capture.label.clone(),
                        capture.ty.to_string(),
                        rendered,
                    );
                }
                Err(diagnostic)
            }
            Terminator::Unreachable => Err(Diagnostic::new("reached unreachable MIR block")),
        }
    }

    fn pop_cleanup(
        &mut self,
        place: &str,
        cleanup_stack: &mut Vec<String>,
        env: &mut Env,
        cancel_before_cleanup: bool,
    ) -> Result<()> {
        let Some(active_place) = cleanup_stack.pop() else {
            return Err(Diagnostic::new(format!(
                "MIR cleanup stack underflow while closing `{}`",
                place
            )));
        };
        if active_place != place {
            return Err(Diagnostic::new(format!(
                "MIR cleanup stack mismatch: expected `{}`, found `{}`",
                place, active_place
            )));
        }
        self.run_cleanup_place(&active_place, env, cancel_before_cleanup)
    }

    fn unwind_cleanups(
        &mut self,
        cleanup_stack: &mut Vec<String>,
        env: &mut Env,
        cancel_before_cleanup: bool,
    ) -> Result<()> {
        while let Some(place) = cleanup_stack.pop() {
            self.run_cleanup_place(&place, env, cancel_before_cleanup)?;
        }
        Ok(())
    }

    fn run_cleanup_place(
        &mut self,
        place: &str,
        env: &mut Env,
        cancel_before_cleanup: bool,
    ) -> Result<()> {
        let resource_type = self.resolve_place_type(place, env);
        let resource = env.read_place(place)?;
        match resource {
            Value::TaskGroup(group) => self.close_task_group(group, cancel_before_cleanup),
            Value::File(file) => {
                file.close();
                Ok(())
            }
            Value::TcpListener(listener) => {
                listener.close();
                Ok(())
            }
            Value::TcpStream(stream) => {
                stream.close();
                Ok(())
            }
            Value::UdpSocket(socket) => {
                socket.close();
                Ok(())
            }
            Value::HttpListener(listener) => {
                listener.close();
                Ok(())
            }
            Value::HttpExchange(_) => Ok(()),
            Value::HttpResponse(_) => Ok(()),
            Value::WebSocketListener(_) => Ok(()),
            Value::WebSocket(socket) => {
                let _ = socket.close();
                Ok(())
            }
            Value::UnixListener(listener) => {
                listener.close();
                Ok(())
            }
            Value::UnixStream(stream) => {
                stream.close();
                Ok(())
            }
            Value::TlsListener(listener) => {
                listener.close();
                Ok(())
            }
            Value::TlsStream(stream) => {
                stream.close();
                Ok(())
            }
            Value::ProcessChild(child) => {
                child.close();
                Ok(())
            }
            Value::ProcessPipe(pipe) => {
                pipe.close();
                Ok(())
            }
            Value::ProcessCompleted(_) => Ok(()),
            Value::ProcessSupervisor(supervisor) => {
                supervisor.close();
                Ok(())
            }
            Value::Instance(instance) => {
                let class = self
                    .classes
                    .get(&instance.class_name)
                    .cloned()
                    .ok_or_else(|| {
                        Diagnostic::new(format!("unknown MIR class `{}`", instance.class_name))
                    })?;
                let method = class
                    .methods
                    .iter()
                    .find(|method| method.name == "close")
                    .cloned()
                    .ok_or_else(|| {
                        Diagnostic::new(format!(
                            "class `{}` cannot be used with MIR `with` because it has no `close` method",
                            class.name
                        ))
                    })?;
                let function = self
                    .functions
                    .get(&method.function_name)
                    .cloned()
                    .ok_or_else(|| {
                        Diagnostic::new(format!(
                            "unknown MIR method body `{}`",
                            method.function_name
                        ))
                    })?;
                let outcome = self.call_function_with_receiver_type(
                    &function,
                    Some(Value::Instance(instance)),
                    Vec::new(),
                    None,
                    None,
                    resource_type.as_ref(),
                )?;
                if let Some(updated_receiver) = outcome.updated_receiver {
                    env.write_place(place, updated_receiver)?;
                }
                Ok(())
            }
            _ => Err(Diagnostic::new(format!(
                "MIR cleanup place `{}` is not a managed resource",
                place
            ))),
        }
    }

    #[cfg(test)]
    fn evaluate_rvalue(&mut self, value: &Rvalue, env: &mut Env) -> Result<RvalueOutcome> {
        self.evaluate_rvalue_for_target(value, env, None)
    }

    fn evaluate_rvalue_for_target(
        &mut self,
        value: &Rvalue,
        env: &mut Env,
        expected_type: Option<&Type>,
    ) -> Result<RvalueOutcome> {
        match value {
            Rvalue::Use(operand) => Ok(RvalueOutcome::Value(
                self.evaluate_owned_operand(operand, env)?,
            )),
            Rvalue::ModuleConstant { key, initializer } => Ok(RvalueOutcome::SharedModuleConstant(
                self.read_module_constant(key, initializer)?,
            )),
            Rvalue::Closure {
                function,
                signature,
                captures,
                consuming,
            } => {
                let mut captured = Vec::with_capacity(captures.len());
                for capture in captures {
                    captured.push(ClosureCaptureValue {
                        name: capture.name.clone(),
                        ty: capture.ty.clone(),
                        value: self.evaluate_owned_operand(&capture.value, env)?,
                        source_place: capture.source_place.clone(),
                        mutable: capture.passing == MirReceiverKind::BorrowMut,
                    });
                }
                let metadata = self.functions.get(function);
                Ok(RvalueOutcome::Value(Value::Function(Box::new(
                    FunctionValue {
                        name: function.clone(),
                        signature: signature.clone(),
                        source_path: metadata.and_then(|function| function.source_path.clone()),
                        entry_span: metadata
                            .map(|function| function.span)
                            .unwrap_or(Span::new(0, 0)),
                        direct_thunk: None,
                        direct_default_binder: None,
                        closure_environment: Some(Arc::new(ClosureEnvironment::new(
                            captured, *consuming,
                        ))),
                    },
                ))))
            }
            Rvalue::FormatString { parts } => {
                let mut rendered = String::new();
                for part in parts {
                    match part {
                        MirFormatPart::Literal(text) => append_string_checked(&mut rendered, text)?,
                        MirFormatPart::Value(value) => {
                            let value = self.evaluate_operand(value, env)?.render();
                            append_string_checked(&mut rendered, &value)?;
                        }
                        MirFormatPart::Formatted {
                            value,
                            spec,
                            value_type,
                        } => {
                            let value = self.evaluate_operand(value, env)?;
                            let formatted = format_runtime_value(&value, value_type, spec)?;
                            append_string_checked(&mut rendered, &formatted)?;
                        }
                    }
                }
                Ok(RvalueOutcome::Value(Value::String(rendered)))
            }
            Rvalue::Unary { op, value, span } => {
                let value = self.evaluate_operand(value, env)?;
                let value = if *op == UnaryOp::BitNot {
                    match expected_type {
                        Some(expected) => {
                            self.coerce_value_to_type(value, expected, Some(*span))?
                        }
                        None => value,
                    }
                } else {
                    value
                };
                let result = match (op, value) {
                    (UnaryOp::Not, Value::Bool(value)) => Value::Bool(!value),
                    (UnaryOp::Neg, Value::Int(value)) => Value::Int(
                        value
                            .checked_neg()
                            .ok_or(Diagnostic::new("integer overflow"))?,
                    ),
                    (UnaryOp::Neg, Value::Float(value)) => Value::Float(-value),
                    (UnaryOp::BitNot, Value::Int(value)) => Value::Int(value.bitnot().ok_or(
                        Diagnostic::coded("AU4001", "invalid typed integer for unary `~`"),
                    )?),
                    (UnaryOp::Not, other) => {
                        return Err(Diagnostic::new(format!(
                            "`not` expects `bool`, found `{}`",
                            other.render()
                        )))
                    }
                    (UnaryOp::Neg, other) => {
                        return Err(Diagnostic::new(format!(
                            "unary `-` expects a numeric value, found `{}`",
                            other.render()
                        )))
                    }
                    (UnaryOp::BitNot, other) => {
                        return Err(Diagnostic::coded(
                            "AU2003",
                            format!(
                                "unary `~` expects an integer value, found `{}`",
                                other.render()
                            ),
                        ))
                    }
                };
                Ok(RvalueOutcome::Value(result))
            }
            Rvalue::Cast { value, ty, span } => {
                let value = self.evaluate_operand(value, env)?;
                Ok(RvalueOutcome::Value(cast_numeric_value(
                    value,
                    ty,
                    Some(*span),
                )?))
            }
            Rvalue::Try { value } => {
                let source_error_ty = match self.resolve_operand_type(value, env) {
                    Some(Type::Named(name, args)) if name == "Result" && args.len() == 2 => {
                        args.get(1).cloned()
                    }
                    _ => None,
                };
                let value = self.evaluate_owned_operand(value, env)?;
                let Value::EnumVariant(variant) = value else {
                    return Err(Diagnostic::new(format!(
                        "MIR `try` requires a `Result` value at runtime, found `{}`",
                        value.render()
                    )));
                };
                if variant.enum_name != "Result" {
                    return Err(Diagnostic::new(format!(
                        "MIR `try` requires a `Result` value at runtime, found `{}`",
                        variant.enum_name
                    )));
                }
                let EnumVariantValue {
                    variant_name,
                    mut payloads,
                    ..
                } = variant;
                if payloads.len() != 1 {
                    return Err(Diagnostic::new(
                        "MIR `try` encountered an invalid `Result` payload at runtime",
                    ));
                }
                let payload = payloads
                    .pop()
                    .expect("validated Result variant should carry one payload");
                match variant_name.as_str() {
                    "Ok" => Ok(RvalueOutcome::Value(payload)),
                    "Err" => {
                        let source_ty = source_error_ty
                            .or_else(|| self.infer_runtime_value_type(&payload))
                            .unwrap_or(Type::named("Unknown"));
                        let payload = self.convert_try_error_via_from(payload, &source_ty)?;
                        Ok(RvalueOutcome::Return(Value::EnumVariant(
                            EnumVariantValue {
                                enum_name: "Result".to_string(),
                                variant_name: "Err".to_string(),
                                payloads: vec![payload],
                            },
                        )))
                    }
                    _ => Err(Diagnostic::new(
                        "MIR `try` encountered an invalid `Result` payload at runtime",
                    )),
                }
            }
            Rvalue::StartTask {
                returns_handle,
                result_is_copy,
                stack_size,
                task_group,
                function,
                args,
                span,
            } => {
                let stack_size = stack_size
                    .as_ref()
                    .map(|operand| self.evaluate_task_stack_size(operand, env))
                    .transpose()?;
                Ok(RvalueOutcome::Value(self.start_task(
                    StartTaskRequest {
                        returns_handle: *returns_handle,
                        result_is_repeatable: *result_is_copy,
                        stack_size,
                        task_group,
                        function,
                        args,
                        spawn_span: *span,
                    },
                    env,
                )?))
            }
            Rvalue::Binary {
                op,
                left,
                right,
                span,
            } => {
                if mir_operand_is_array(left, env)? || mir_operand_is_array(right, env)? {
                    let left = borrow_mir_operand(left, env)?;
                    let right = borrow_mir_operand(right, env)?;
                    return Ok(RvalueOutcome::Value(self.eval_array_binary(
                        *op,
                        left.as_value(),
                        right.as_value(),
                        Some(*span),
                    )?));
                }
                let mut left = self.evaluate_operand(left, env)?;
                let mut right = self.evaluate_operand(right, env)?;
                let coerce_operands = matches!(
                    op,
                    BinaryOp::Pow
                        | BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                        | BinaryOp::Shl
                        | BinaryOp::Shr
                );
                if coerce_operands {
                    if let Some(expected) = expected_type {
                        left = self.coerce_value_to_type(left, expected, Some(*span))?;
                        right = self.coerce_value_to_type(right, expected, Some(*span))?;
                    }
                }
                if *op == BinaryOp::Pow {
                    if let (Value::Float(left), Value::Float(right)) = (&left, &right) {
                        let width = match expected_type {
                            Some(Type::Named(name, args))
                                if name == "float32" && args.is_empty() =>
                            {
                                FloatPowerWidth::Float32
                            }
                            _ => FloatPowerWidth::Float64,
                        };
                        return float_power(*left, *right, width)
                            .map(Value::Float)
                            .map(RvalueOutcome::Value)
                            .map_err(|error| with_optional_diagnostic_span(error, Some(*span)));
                    }
                }
                Ok(RvalueOutcome::Value(self.eval_binary(
                    *op,
                    left,
                    right,
                    Some(*span),
                )?))
            }
            Rvalue::Call { callee, args } => Ok(RvalueOutcome::Value(
                self.evaluate_call_for_target(callee, args, env, expected_type)?,
            )),
            Rvalue::VecLiteral {
                elements,
                element_type,
            } => Ok(RvalueOutcome::Value(Value::Vec(VecValue {
                element_type: element_type.clone(),
                elements: elements
                    .iter()
                    .map(|operand| self.evaluate_owned_operand(operand, env))
                    .collect::<Result<Vec<_>>>()?,
            }))),
            Rvalue::TupleLiteral {
                elements,
                element_types,
            } => {
                if elements.len() != element_types.len() {
                    return Err(Diagnostic::new(format!(
                        "MIR tuple literal has {} values but {} element types",
                        elements.len(),
                        element_types.len()
                    )));
                }
                let mut values = Vec::with_capacity(elements.len());
                for operand in elements {
                    values.push(self.evaluate_owned_operand(operand, env)?);
                }
                Ok(RvalueOutcome::Value(Value::Tuple(TupleValue {
                    element_types: element_types.clone(),
                    elements: values,
                })))
            }
            Rvalue::TupleElement {
                tuple,
                index,
                element_type: _,
            } => {
                let element = match tuple {
                    Operand::Place(place) => env.tuple_element(place, *index)?,
                    _ => {
                        let value = self.evaluate_operand(tuple, env)?;
                        return Err(Diagnostic::new(format!(
                            "MIR tuple projection expected a tuple, found `{}`",
                            value.render()
                        )));
                    }
                };
                Ok(RvalueOutcome::Value(element))
            }
            Rvalue::TupleTakeElement {
                place,
                index,
                element_type: _,
            } => Ok(RvalueOutcome::Value(env.take_tuple_element(place, *index)?)),
            Rvalue::SetLiteral {
                elements,
                element_type,
            } => {
                let mut values = Vec::new();
                for operand in elements {
                    let value = self.evaluate_owned_operand(operand, env)?;
                    if !values.contains(&value) {
                        values.push(value);
                    }
                }
                Ok(RvalueOutcome::Value(Value::Set(SetValue {
                    element_type: element_type.clone(),
                    elements: values,
                })))
            }
            Rvalue::MapLiteral {
                entries,
                key_type,
                value_type,
            } => {
                let mut values = Vec::new();
                for entry in entries {
                    let key = self.evaluate_owned_operand(&entry.key, env)?;
                    let value = self.evaluate_owned_operand(&entry.value, env)?;
                    if let Some(index) = values
                        .iter()
                        .position(|(candidate_key, _)| *candidate_key == key)
                    {
                        values[index].1 = value;
                    } else {
                        values.push((key, value));
                    }
                }
                Ok(RvalueOutcome::Value(Value::Map(MapValue {
                    key_type: key_type.clone(),
                    value_type: value_type.clone(),
                    entries: values,
                })))
            }
            Rvalue::Construct { class_name, fields } => {
                let mut values = BTreeMap::new();
                for field in fields {
                    values.insert(
                        field.name.clone(),
                        self.evaluate_owned_operand(&field.value, env)?,
                    );
                }
                Ok(RvalueOutcome::Value(Value::Instance(InstanceValue {
                    class_name: class_name.clone(),
                    fields: values,
                })))
            }
            Rvalue::EnumVariant {
                enum_name,
                variant_name,
                payloads,
            } => Ok(RvalueOutcome::Value(Value::EnumVariant(EnumVariantValue {
                enum_name: enum_name.clone(),
                variant_name: variant_name.clone(),
                payloads: payloads
                    .iter()
                    .map(|payload| take_mir_operand(payload, env))
                    .collect::<Result<Vec<_>>>()?,
            }))),
            Rvalue::VariantPayload {
                scrutinee,
                variant_name: _,
                index,
            } => {
                if let Operand::MovePlace(place) = scrutinee {
                    return Ok(RvalueOutcome::Value(
                        env.take_variant_payload(place, *index)?,
                    ));
                }
                let scrutinee = self.evaluate_owned_operand(scrutinee, env)?;
                let Value::EnumVariant(variant) = scrutinee else {
                    return Err(Diagnostic::new(format!(
                        "MIR variant payload extraction expected an enum value, found `{}`",
                        scrutinee.render()
                    )));
                };
                let payload = variant.payloads.into_iter().nth(*index).ok_or_else(|| {
                    Diagnostic::new(format!(
                        "enum variant `{}.{}` does not carry a payload at index {}",
                        variant.enum_name, variant.variant_name, index
                    ))
                })?;
                Ok(RvalueOutcome::Value(payload))
            }
            Rvalue::Member { object, field } => match object {
                Operand::Place(place) => Ok(RvalueOutcome::Value(env.read_member(place, field)?)),
                // Lowering materializes every composite value in a place, so
                // no non-place Operand can contain an Instance. Keep the
                // public forged-MIR diagnostic without retaining an
                // impossible successful extraction branch.
                _ => {
                    let object = self.evaluate_operand(object, env)?;
                    Err(Diagnostic::new(format!(
                        "cannot access field `{}` on non-instance value `{}`",
                        field,
                        object.render()
                    )))
                }
            },
        }
    }

    #[cfg(test)]
    fn evaluate_call(
        &mut self,
        callee: &CallTarget,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        self.evaluate_call_for_target(callee, args, env, None)
    }

    fn evaluate_call_for_target(
        &mut self,
        callee: &CallTarget,
        args: &[MirArg],
        env: &mut Env,
        expected_return_type: Option<&Type>,
    ) -> Result<Value> {
        match callee {
            CallTarget::Name(name) => {
                if name == "control::__retry_cancel_if_requested" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    if poll_cancellation(&self.cancellation) {
                        panic::panic_any(TaskCancelledSignal);
                    }
                    return Ok(Value::Unit);
                }
                if name == "print" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["value"], values)?;
                    let rendered_value = match (&bound[0].value, bound[0].ty.as_ref()) {
                        (Value::Float(value), Some(Type::Named(name, args)))
                            if name == "float32" && args.is_empty() =>
                        {
                            render_float32(*value as f32)
                        }
                        (Value::Float(value), _) => render_float(*value),
                        (value, _) => value.render(),
                    };
                    let rendered = format!("{}\n", rendered_value);
                    let mut stdout = lock_stdout(&self.stdout);
                    stdout.push_str(&rendered);
                    drop(stdout);
                    if let Some(sink) = &self.stdout_sink {
                        sink(&rendered);
                    }
                    return Ok(Value::Unit);
                }

                if name == "range" {
                    let values = evaluate_named_args(args, env)?;
                    return build_range(values);
                }

                if name == "Queue" {
                    let values = evaluate_named_args(args, env)?;
                    if values.len() > 1 {
                        return Err(Diagnostic::new(format!(
                            "`{}()` expects at most one optional `capacity` argument",
                            name
                        )));
                    }
                    let capacity = match values.as_slice() {
                        [] => None,
                        [argument] => {
                            if argument.name.as_deref() != Some("capacity")
                                && argument.name.is_some()
                            {
                                return Err(Diagnostic::new(
                                    "`Queue()` expects an optional `capacity=` argument",
                                ));
                            }
                            let capacity =
                                expect_i32_value(&argument.value, "Queue(capacity=...)")?;
                            if capacity <= 0 {
                                return Err(Diagnostic::new(
                                    "`Queue(capacity=...)` expects a positive `int32`",
                                ));
                            }
                            Some(capacity as usize)
                        }
                        _ => unreachable!(),
                    };
                    return Ok(Value::Channel(match capacity {
                        Some(capacity) => ChannelValue::with_capacity(capacity),
                        None => ChannelValue::new(),
                    }));
                }

                if name == "list" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(Value::Vec(VecValue {
                        element_type: Type::named("Unknown"),
                        elements: Vec::new(),
                    }));
                }

                if name == "set" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(Value::Set(SetValue {
                        element_type: Type::named("Unknown"),
                        elements: Vec::new(),
                    }));
                }

                if name == "dict" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(Value::Map(MapValue {
                        key_type: Type::named("Unknown"),
                        value_type: Type::named("Unknown"),
                        entries: Vec::new(),
                    }));
                }

                if name == "TaskGroup" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(Value::TaskGroup(TaskGroupValue::new(&self.cancellation)));
                }

                if name == "random::Rng" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["seed"], values)?;
                    let seed = expect_i64_value(&bound[0].value, "random.Rng(seed=...)")?;
                    return Ok(Value::Rng(RngValue::from_seed(seed)));
                }

                if name == "random::secure_int" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["lo", "hi"], values)?;
                    let lo = expect_i64_value(&bound[0].value, "random.secure_int(...)")?;
                    let hi = expect_i64_value(&bound[1].value, "random.secure_int(...)")?;
                    return randomness::secure_int(lo, hi)
                        .map(|value| Value::Int(IntegerValue::from_signed(i128::from(value))))
                        .map_err(|error| {
                            random_resource_error_to_diagnostic(error, Some((lo, hi)))
                        });
                }

                if name == "random::secure_bytes" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["n"], values)?;
                    let count = expect_i64_value(&bound[0].value, "random.secure_bytes(n)")?;
                    if count < 0 {
                        return Err(Diagnostic::coded(
                            "AU4003",
                            format!(
                                "`random.secure_bytes(n)` requires a non-negative byte count, found `{count}`"
                            ),
                        ));
                    }
                    // Every supported Aura release target is 64-bit, so a
                    // validated non-negative int64 always fits usize.
                    let count = count as usize;
                    return randomness::secure_bytes(count)
                        .map(bytes_vec_value)
                        .map_err(|error| random_resource_error_to_diagnostic(error, None));
                }

                if let Some((_, member_name)) = name
                    .split_once('.')
                    .filter(|(type_name, _)| *type_name == "Array")
                {
                    let dtype = checked_mir_array_dtype(expected_return_type);
                    let values = evaluate_named_args(args, env)?;
                    let array = if member_name == "zeros" {
                        let bound = bind_builtin_args(&["shape"], values)?;
                        let shape = array_shape_from_runtime(&bound[0].value)?;
                        ArrayValue::zeros(dtype, shape)?
                    } else if member_name == "full" {
                        let bound = bind_builtin_args(&["shape", "value"], values)?;
                        let shape = array_shape_from_runtime(&bound[0].value)?;
                        ArrayValue::full(dtype, shape, &bound[1].value)?
                    } else {
                        debug_assert_eq!(member_name, "from_list");
                        let bound = bind_builtin_args(&["values", "shape"], values)?;
                        let vector = checked_mir_vec_ref(&bound[0].value);
                        debug_assert_eq!(ArrayDType::from_type(&vector.element_type), Some(dtype));
                        let shape = array_shape_from_runtime(&bound[1].value)?;
                        ArrayValue::from_vec(vector, Some(&shape))?
                    };
                    return Ok(Value::Array(array));
                }

                if let Some((type_name, member_name)) = name
                    .split_once('.')
                    .filter(|(type_name, _)| *type_name == "Duration")
                {
                    if let Some(constructor) =
                        BuiltinAssociatedFunction::resolve(type_name, member_name)
                    {
                        let values = evaluate_named_args(args, env)?;
                        let bound = bind_builtin_args(&["value"], values)?;
                        let value = match &bound[0].value {
                            Value::Int(value) => *value,
                            _ => {
                                return Err(Diagnostic::new(format!(
                                    "`Duration.{}` expects `int64`",
                                    constructor.name()
                                )))
                            }
                        };
                        let value = value
                            .as_i128()
                            .filter(|value| i64::try_from(*value).is_ok())
                            .ok_or_else(|| {
                                Diagnostic::new(format!(
                                    "`Duration.{}` expects `int64`",
                                    constructor.name()
                                ))
                            })?;
                        let scale = if member_name == "ms" {
                            NANOS_PER_MILLISECOND
                        } else if member_name == "seconds" {
                            NANOS_PER_SECOND
                        } else {
                            debug_assert_eq!(member_name, "minutes");
                            NANOS_PER_MINUTE
                        };
                        // `value` is int64 and the largest scale is 60e9, so the
                        // product is strictly inside the i128 Duration range.
                        return Ok(Value::Duration(value * scale));
                    }
                }

                if let Some((type_name, "with_capacity")) = name
                    .split_once('.')
                    .filter(|(type_name, _)| matches!(*type_name, "list" | "dict" | "set"))
                {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["minimum"], values)?;
                    let minimum = expect_i64_value(
                        &bound[0].value,
                        &format!("{type_name}.with_capacity(...)"),
                    )?;
                    let minimum = usize::try_from(minimum).map_err(|_| {
                        Diagnostic::coded("AU4003", "collection capacity cannot be negative")
                    })?;
                    return match (type_name, expected_return_type) {
                        ("list", Some(Type::Named(_, args))) if args.len() == 1 => {
                            let mut elements = Vec::new();
                            elements.try_reserve(minimum).map_err(|_| {
                                Diagnostic::coded("AU4005", "list capacity allocation failed")
                            })?;
                            Ok(Value::Vec(VecValue {
                                element_type: args[0].clone(),
                                elements,
                            }))
                        }
                        ("dict", Some(Type::Named(_, args))) if args.len() == 2 => {
                            let mut entries = Vec::new();
                            entries.try_reserve(minimum).map_err(|_| {
                                Diagnostic::coded("AU4005", "dictionary capacity allocation failed")
                            })?;
                            Ok(Value::Map(MapValue {
                                key_type: args[0].clone(),
                                value_type: args[1].clone(),
                                entries,
                            }))
                        }
                        ("set", Some(Type::Named(_, args))) if args.len() == 1 => {
                            let mut elements = Vec::new();
                            elements.try_reserve(minimum).map_err(|_| {
                                Diagnostic::coded("AU4005", "set capacity allocation failed")
                            })?;
                            Ok(Value::Set(SetValue {
                                element_type: args[0].clone(),
                                elements,
                            }))
                        }
                        _ => Err(Diagnostic::new("invalid collection capacity constructor")),
                    };
                }

                if name == "cancelled" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(Value::Bool(poll_cancellation(&self.cancellation)));
                }

                if name == "yield_now" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    yield_now_with_runtime_scheduler();
                    return Ok(Value::Unit);
                }

                if name == "sleep" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["duration"], values)?;
                    let duration = match bound[0].value {
                        Value::Duration(duration) => duration_to_host_timer(duration, "sleep(...)")
                            .map_err(|error| Diagnostic::coded("AU4001", error.to_string()))?,
                        _ => {
                            return Err(Diagnostic::new(
                                "`sleep(...)` expects a duration value in MIR runtime",
                            ))
                        }
                    };
                    sleep_with_runtime_scheduler(duration, Some(&self.cancellation))
                        .map_err(timer_error_to_diagnostic)?;
                    return Ok(Value::Unit);
                }

                if name == "select" {
                    let values = evaluate_named_args(args, env)?;
                    return select_runtime_values(
                        validate_mir_select_sources(values)?,
                        Some(&self.cancellation),
                    );
                }

                if matches!(name.as_str(), "wait_any" | "wait_all") {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["tasks", "timeout"], values)?;
                    let tasks = self.expect_task_list(&bound[0].value, name)?;
                    let timeout = expect_optional_timeout(
                        Some(&bound[1].value),
                        &format!("{name}(timeout=...)"),
                    )?;
                    return if name == "wait_any" {
                        self.wait_any(tasks, timeout)
                    } else {
                        self.wait_all(tasks, timeout)
                    };
                }

                if name == "abs" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["value"], values)?;
                    return match bound[0].value.clone() {
                        Value::Int(value) => match value.representation() {
                            IntegerRepresentation::Signed(signed) if signed < 0 => {
                                if signed == i128::MIN {
                                    return Err(Diagnostic::new(
                                        "`abs(...)` overflowed the signed integer range",
                                    ));
                                }
                                value.checked_neg().map(Value::Int).ok_or(Diagnostic::new(
                                    "`abs(...)` overflowed the signed integer range",
                                ))
                            }
                            IntegerRepresentation::Signed(_)
                            | IntegerRepresentation::Unsigned(_) => Ok(Value::Int(value)),
                        },
                        Value::Float(value) => Ok(Value::Float(value.abs())),
                        other => Err(Diagnostic::new(format!(
                            "`abs(...)` expects an integer or float value, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "min" || name == "max" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["left", "right"], values)?;
                    return match (&bound[0].value, &bound[1].value) {
                        (Value::Int(left), Value::Int(right)) => Ok(
                            if (name == "min" && left <= right) || (name == "max" && left >= right)
                            {
                                bound[0].value.clone()
                            } else {
                                bound[1].value.clone()
                            },
                        ),
                        (Value::Float(left), Value::Float(right)) => Ok(
                            if (name == "min" && left <= right) || (name == "max" && left >= right)
                            {
                                bound[0].value.clone()
                            } else {
                                bound[1].value.clone()
                            },
                        ),
                        _ => Err(Diagnostic::new(format!(
                            "`{}` expects matching numeric arguments",
                            name
                        ))),
                    };
                }

                if name == "sqrt" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["value"], values)?;
                    return match bound[0].value.clone() {
                        Value::Float(value) => Ok(Value::Float(value.sqrt())),
                        other => Err(Diagnostic::new(format!(
                            "`sqrt(...)` expects `float32` or `float64`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "round" {
                    return evaluate_round_builtin(args, env);
                }

                if name == "divmod" {
                    return evaluate_divmod_builtin(args, env);
                }

                if name == "parse_int32" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["text"], values)?;
                    return match &bound[0].value {
                        Value::String(text) => match text.parse::<i32>() {
                            Ok(value) => Ok(result_ok(Value::Int(IntegerValue::from_signed(
                                value as i128,
                            )))),
                            Err(error) => Ok(result_err(Value::String(error.to_string()))),
                        },
                        other => Err(Diagnostic::new(format!(
                            "`parse_int32(...)` expects `str`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "parse_int64" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["text"], values)?;
                    return match &bound[0].value {
                        Value::String(text) => match text.parse::<i64>() {
                            Ok(value) => Ok(result_ok(Value::Int(IntegerValue::from_signed(
                                value as i128,
                            )))),
                            Err(error) => Ok(result_err(Value::String(error.to_string()))),
                        },
                        other => Err(Diagnostic::new(format!(
                            "`parse_int64(...)` expects `str`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "parse_float64" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["text"], values)?;
                    return match &bound[0].value {
                        Value::String(text) => match text.parse::<f64>() {
                            Ok(value) if value.is_finite() => Ok(result_ok(Value::Float(value))),
                            Ok(_) => Ok(result_err(Value::String(
                                "float must be finite".to_string(),
                            ))),
                            Err(error) => Ok(result_err(Value::String(error.to_string()))),
                        },
                        other => Err(Diagnostic::new(format!(
                            "`parse_float64(...)` expects `str`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "io::write" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["text"], values)?;
                    return match &bound[0].value {
                        Value::String(text) => {
                            let mut stdout = lock_stdout(&self.stdout);
                            stdout.push_str(text);
                            drop(stdout);
                            if let Some(sink) = &self.stdout_sink {
                                sink(text);
                            }
                            Ok(result_ok(Value::Unit))
                        }
                        other => Err(Diagnostic::new(format!(
                            "`io.write(...)` expects `str`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if name == "io::flush" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return Ok(result_ok(Value::Unit));
                }

                if name == "io::read_line" {
                    let values = evaluate_named_args(args, env)?;
                    bind_builtin_args(&[], values)?;
                    return match io_read_line() {
                        Ok(Some(line)) => Ok(result_ok(option_some(Value::String(line)))),
                        Ok(None) => Ok(result_ok(option_none())),
                        Err(error) => Ok(result_err(io_error(error))),
                    };
                }

                if name == "fs::exists" {
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&["path"], values)?;
                    return match &bound[0].value {
                        Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).exists())),
                        other => Err(Diagnostic::new(format!(
                            "`fs.exists(...)` expects `str`, found `{}`",
                            other.render()
                        ))),
                    };
                }

                if let Some(result) = evaluate_bytes_mir_host_call(name, args, env) {
                    return result;
                }

                if let Some(result) = evaluate_json_mir_host_call(name, args, env) {
                    return result;
                }

                if let Some(metadata) = host_builtin_metadata(name) {
                    // Checked MIR materializes builtin defaults before runtime dispatch, so
                    // optional metadata parameters are present in this argument list too.
                    let arg_names = metadata
                        .params
                        .iter()
                        .map(|param| param.name.as_str())
                        .collect::<Vec<_>>();
                    let values = evaluate_named_args(args, env)?;
                    let bound = bind_builtin_args(&arg_names, values)?;
                    return evaluate_host_builtin_with_program_args(
                        &metadata.qualified_name,
                        bound.into_iter().map(|argument| argument.value).collect(),
                        self.program_args.as_slice(),
                    );
                }

                if matches!(
                    name.as_str(),
                    "fs::read_to_string"
                        | "fs::read_bytes"
                        | "fs::write_string"
                        | "fs::write_bytes"
                        | "fs::append_string"
                        | "fs::append_bytes"
                        | "fs::create_dir"
                        | "fs::read_dir"
                        | "fs::remove_file"
                        | "fs::open"
                        | "fs::create"
                        | "fs::append"
                        | "net::connect"
                        | "net::connect_timeout"
                        | "net::listen"
                        | "net::udp_bind"
                        | "net::unix_listen"
                        | "net::unix_connect"
                        | "net::unix_connect_timeout"
                        | "net::tls_listen"
                        | "net::tls_connect"
                        | "net::tls_connect_timeout"
                        | "net::http_listen"
                        | "net::http_request_text"
                        | "net::http_request_text_timeout"
                        | "net::http_request_bytes"
                        | "net::http_request_bytes_timeout"
                        | "net::websocket_listen"
                        | "net::websocket_connect"
                        | "net::websocket_connect_timeout"
                        | "process::inherit"
                        | "process::null"
                        | "process::pipe"
                        | "process::supervisor"
                        | "process::start"
                        | "process::run"
                ) {
                    let values = evaluate_named_args(args, env)?;
                    return self.evaluate_builtin_io_call(name, values);
                }

                if name == "print" || name == "range" {
                    unreachable!("handled earlier");
                }

                let function =
                    self.functions.get(name).cloned().ok_or_else(|| {
                        Diagnostic::new(format!("unknown MIR function `{}`", name))
                    })?;
                let evaluated_args = evaluate_named_args(args, env)?;
                let writeback_places =
                    bind_function_writeback_places(&function.params, &evaluated_args)?;
                let outcome = self.call_function_for_target(
                    &function,
                    None,
                    evaluated_args,
                    expected_return_type,
                )?;
                self.apply_borrowed_param_writebacks(
                    &function.params,
                    &writeback_places,
                    outcome.updated_params,
                    env,
                )?;
                Ok(outcome.value)
            }
            CallTarget::Extern(call) => self.evaluate_extern_call(call, args, env),
            CallTarget::Value(function) => {
                self.evaluate_function_value_call(function, args, env, expected_return_type)
            }
            CallTarget::Member {
                object,
                field,
                receiver_place,
            } => {
                if matches!(field.as_str(), "len" | "byte_len") {
                    if let Operand::Place(place) = object {
                        if let Some(result) =
                            evaluate_borrowed_length_member(env.place_ref(place)?, field, args)
                        {
                            return result;
                        }
                    }
                }

                if field == "to_bytes" {
                    if let Operand::Place(place) = object {
                        let receiver = env.place_ref(place)?;
                        if let Value::String(text) = receiver {
                            if !args.is_empty() {
                                return Err(Diagnostic::new("`to_bytes` does not take arguments"));
                            }
                            return evaluate_string_to_bytes_host_ref(text);
                        }
                    }
                }

                if field == "__slice" {
                    if let Operand::Place(place) = object {
                        let values = evaluate_named_args(args, env)?;
                        let (start, end, span) = self.mir_slice_args(values)?;
                        let result = match env.place_ref(place)? {
                            Value::Vec(vector) => {
                                slice_vec_owned(vector, start, end).map(Value::Vec)
                            }
                            Value::Array(array) => {
                                array.slice_first_axis(start, end).map(Value::Array)
                            }
                            Value::String(text) => {
                                slice_string_owned(text, start, end).map(Value::String)
                            }
                            other => {
                                return Err(Diagnostic::new(format!(
                                    "unsupported MIR member call `__slice` on `{}`",
                                    other.render()
                                )))
                            }
                        };
                        return result.map_err(|mut error| {
                            error.span = Some(span);
                            error
                        });
                    }
                }

                if let Operand::Place(place) = object {
                    if matches!(env.place_ref(place)?, Value::Array(_)) {
                        return self.evaluate_array_place_method(place, field, args, env);
                    }
                }

                let receiver_static_ty = self
                    .resolve_operand_type(object, env)
                    .filter(|ty| !matches!(ty, Type::TypeParam(_)));
                let mut receiver = self.evaluate_owned_operand(object, env)?;

                if field == "__take_index_option" {
                    let receiver = std::mem::replace(&mut receiver, Value::Unit);
                    return match receiver {
                        Value::Vec(vector) => self.evaluate_vec_method(
                            vector,
                            field,
                            receiver_place.as_deref(),
                            args,
                            env,
                        ),
                        Value::Set(set) => self.evaluate_set_method(
                            set,
                            field,
                            receiver_place.as_deref(),
                            args,
                            env,
                        ),
                        other => Err(Diagnostic::new(format!(
                            "unsupported MIR member call `{}` on `{}`",
                            field,
                            other.render()
                        ))),
                    };
                }

                if field == "__slice" {
                    let receiver = std::mem::replace(&mut receiver, Value::Unit);
                    return match receiver {
                        Value::Vec(vector) => {
                            self.evaluate_vec_method(vector, field, None, args, env)
                        }
                        Value::String(text) => self.evaluate_string_method(text, field, args, env),
                        other => Err(Diagnostic::new(format!(
                            "unsupported MIR member call `__slice` on `{}`",
                            other.render()
                        ))),
                    };
                }

                if !matches!(receiver, Value::Instance(_)) {
                    let resolved_receiver_ty = receiver_static_ty
                        .clone()
                        .or_else(|| self.infer_runtime_value_type(&receiver));
                    if let Some(resolved_receiver_ty) = resolved_receiver_ty {
                        let builtin_name = match &resolved_receiver_ty {
                            Type::Named(name, _) => Some(name.as_str()),
                            _ => None,
                        };
                        if builtin_name
                            .and_then(|name| BuiltinMember::resolve_runtime(name, field))
                            .is_none()
                        {
                            if let Some(method) = self
                                .find_trait_impl_method(&resolved_receiver_ty, field)
                                .cloned()
                            {
                                return self.evaluate_resolved_trait_method_call(
                                    receiver,
                                    &resolved_receiver_ty,
                                    field,
                                    method,
                                    receiver_place.as_deref(),
                                    args,
                                    expected_return_type,
                                    env,
                                );
                            }
                        }
                    }
                }

                let mut receiver = match receiver {
                    Value::Vec(vector) => {
                        return self.evaluate_vec_method(
                            vector,
                            field,
                            receiver_place.as_deref(),
                            args,
                            env,
                        )
                    }
                    Value::Map(map) => {
                        return self.evaluate_map_method(
                            map,
                            field,
                            receiver_place.as_deref(),
                            args,
                            env,
                        )
                    }
                    Value::Set(set) => {
                        return self.evaluate_set_method(
                            set,
                            field,
                            receiver_place.as_deref(),
                            args,
                            env,
                        )
                    }
                    receiver => receiver,
                };
                match &receiver {
                    Value::Float(value) if field == "sqrt" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`sqrt` does not take arguments"));
                        }
                        Ok(Value::Float(value.sqrt()))
                    }
                    Value::Int(value) if field == "to_string" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_string` does not take arguments"));
                        }
                        Ok(Value::String(value.to_string()))
                    }
                    Value::Int(value) if field == "to_float" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_float` does not take arguments"));
                        }
                        Ok(Value::Float(value.to_f64()))
                    }
                    Value::Int(value)
                        if matches!(
                            field.as_str(),
                            "wrapping_add"
                                | "wrapping_sub"
                                | "wrapping_mul"
                                | "saturating_add"
                                | "saturating_sub"
                                | "saturating_mul"
                        ) =>
                    {
                        let values = evaluate_named_args(args, env)?;
                        let bound = bind_builtin_args(&["rhs"], values)?;
                        macro_rules! mismatch {
                            () => {
                                Diagnostic::coded(
                                    "AU4001",
                                    format!(
                                        "`{field}` expects matching fixed-width integer operands"
                                    ),
                                )
                            };
                        }
                        let Some(receiver_kind) = receiver_static_ty
                            .as_ref()
                            .and_then(|ty| match ty {
                                Type::Named(name, args) if args.is_empty() => {
                                    IntegerKind::from_runtime_type_name(name)
                                }
                                _ => None,
                            })
                            .or_else(|| value.runtime_kind())
                        else {
                            return Err(mismatch!());
                        };
                        // Call checking has already made `rhs` exactly the receiver type, but a
                        // contextual integer literal can still arrive through a materialized MIR
                        // place carrying its default `int64` runtime tag. Direct emission loads
                        // that operand as the checked receiver type; mirror that coercion here.
                        let with_receiver_kind =
                            |operand: IntegerValue| operand.with_runtime_kind(receiver_kind);
                        let Some(left) = with_receiver_kind(*value) else {
                            return Err(mismatch!());
                        };
                        let Value::Int(rhs) = &bound[0].value else {
                            return Err(mismatch!());
                        };
                        let Some(rhs) = with_receiver_kind(*rhs) else {
                            return Err(mismatch!());
                        };
                        let result = match field.as_str() {
                            "wrapping_add" => left.wrapping_add(rhs),
                            "wrapping_sub" => left.wrapping_sub(rhs),
                            "wrapping_mul" => left.wrapping_mul(rhs),
                            "saturating_add" => left.saturating_add(rhs),
                            "saturating_sub" => left.saturating_sub(rhs),
                            _ => {
                                debug_assert_eq!(field, "saturating_mul");
                                left.saturating_mul(rhs)
                            }
                        };
                        let Some(result) = result else {
                            return Err(mismatch!());
                        };
                        Ok(Value::Int(result))
                    }
                    Value::Int(value)
                        if matches!(
                            field.as_str(),
                            "wrapping_shl" | "wrapping_shr" | "saturating_shl" | "saturating_shr"
                        ) =>
                    {
                        let values = evaluate_named_args(args, env)?;
                        let bound = bind_builtin_args(&["count"], values)?;
                        macro_rules! mismatch {
                            () => {
                                Diagnostic::coded(
                                    "AU4001",
                                    format!(
                                        "`{field}` expects matching fixed-width integer operands"
                                    ),
                                )
                            };
                        }
                        let Some(receiver_kind) = receiver_static_ty
                            .as_ref()
                            .and_then(|ty| match ty {
                                Type::Named(name, args) if args.is_empty() => {
                                    IntegerKind::from_runtime_type_name(name)
                                }
                                _ => None,
                            })
                            .or_else(|| value.runtime_kind())
                        else {
                            return Err(mismatch!());
                        };
                        let with_receiver_kind =
                            |operand: IntegerValue| operand.with_runtime_kind(receiver_kind);
                        let Some(left) = with_receiver_kind(*value) else {
                            return Err(mismatch!());
                        };
                        let Value::Int(count) = &bound[0].value else {
                            return Err(mismatch!());
                        };
                        let Some(count) = with_receiver_kind(*count) else {
                            return Err(mismatch!());
                        };
                        let result = match field.as_str() {
                            "wrapping_shl" => left.wrapping_shl(count),
                            "wrapping_shr" => left.wrapping_shr(count),
                            "saturating_shl" => left.saturating_shl(count),
                            _ => {
                                debug_assert_eq!(field, "saturating_shr");
                                left.saturating_shr(count)
                            }
                        };
                        result
                            .map(Value::Int)
                            .map_err(|error| integer_shift_diagnostic(error, None))
                    }
                    Value::Float(value) if field == "to_string" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_string` does not take arguments"));
                        }
                        Ok(Value::String(Value::Float(*value).render()))
                    }
                    Value::Bool(value) if field == "to_string" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_string` does not take arguments"));
                        }
                        Ok(Value::String(value.to_string()))
                    }
                    Value::Duration(value) if field == "to_ms" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_ms` does not take arguments"));
                        }
                        Ok(Value::Float(duration_to_milliseconds(*value)))
                    }
                    Value::Duration(value) if field == "to_seconds" => {
                        if !args.is_empty() {
                            return Err(Diagnostic::new("`to_seconds` does not take arguments"));
                        }
                        Ok(Value::Float(duration_to_seconds(*value)))
                    }
                    Value::Rng(rng) => self.evaluate_rng_method(rng.clone(), field, args, env),
                    Value::String(value) => {
                        self.evaluate_string_method(value.clone(), field, args, env)
                    }
                    Value::Channel(channel) => {
                        self.evaluate_channel_method(channel.clone(), field, args, env)
                    }
                    Value::Task(task) => self.evaluate_task_method(task.clone(), field, args, env),
                    Value::TaskGroup(group) => {
                        self.evaluate_task_group_method(group.clone(), field, args, env)
                    }
                    Value::File(file) => self.evaluate_file_method(file.clone(), field, args, env),
                    Value::TcpListener(listener) => {
                        self.evaluate_tcp_listener_method(listener.clone(), field, args, env)
                    }
                    Value::TcpStream(stream) => {
                        self.evaluate_tcp_stream_method(stream.clone(), field, args, env)
                    }
                    Value::UdpSocket(socket) => {
                        self.evaluate_udp_socket_method(socket.clone(), field, args, env)
                    }
                    Value::UdpDatagram(datagram) => {
                        self.evaluate_udp_datagram_method(datagram.clone(), field, args, env)
                    }
                    Value::HttpListener(listener) => {
                        self.evaluate_http_listener_method(listener.clone(), field, args, env)
                    }
                    Value::HttpExchange(exchange) => {
                        self.evaluate_http_exchange_method(exchange.clone(), field, args, env)
                    }
                    Value::HttpResponse(response) => {
                        self.evaluate_http_response_method(response.clone(), field, args, env)
                    }
                    Value::WebSocketListener(listener) => {
                        self.evaluate_websocket_listener_method(listener.clone(), field, args, env)
                    }
                    Value::WebSocket(socket) => {
                        self.evaluate_websocket_method(socket.clone(), field, args, env)
                    }
                    Value::UnixListener(listener) => {
                        self.evaluate_unix_listener_method(listener.clone(), field, args, env)
                    }
                    Value::UnixStream(stream) => {
                        self.evaluate_unix_stream_method(stream.clone(), field, args, env)
                    }
                    Value::TlsListener(listener) => {
                        self.evaluate_tls_listener_method(listener.clone(), field, args, env)
                    }
                    Value::TlsStream(stream) => {
                        self.evaluate_tls_stream_method(stream.clone(), field, args, env)
                    }
                    Value::ProcessChild(child) => {
                        self.evaluate_process_child_method(child.clone(), field, args, env)
                    }
                    Value::ProcessPipe(pipe) => {
                        self.evaluate_process_pipe_method(pipe.clone(), field, args, env)
                    }
                    Value::ProcessCompleted(completed) => {
                        self.evaluate_process_completed_method(completed.clone(), field, args, env)
                    }
                    Value::ProcessSupervisor(supervisor) => self
                        .evaluate_process_supervisor_method(supervisor.clone(), field, args, env),
                    Value::Instance(instance) => {
                        let resolved_receiver_ty = receiver_static_ty
                            .clone()
                            .unwrap_or(Type::named(&instance.class_name));
                        let class =
                            self.classes
                                .get(&instance.class_name)
                                .cloned()
                                .ok_or_else(|| {
                                    Diagnostic::new(format!(
                                        "unknown MIR class `{}`",
                                        instance.class_name
                                    ))
                                })?;
                        let method = class
                            .methods
                            .iter()
                            .find(|method| method.name == *field)
                            .cloned()
                            .or_else(|| {
                                self.find_trait_impl_method(&resolved_receiver_ty, field)
                                    .or_else(|| {
                                        self.find_trait_impl_method_for_class_name(
                                            &instance.class_name,
                                            field,
                                        )
                                    })
                                    .cloned()
                            })
                            .ok_or_else(|| {
                                Diagnostic::new(format!(
                                    "class `{}` has no MIR method `{}`",
                                    class.name, field
                                ))
                            })?;
                        let function = self
                            .functions
                            .get(&method.function_name)
                            .cloned()
                            .ok_or_else(|| {
                                Diagnostic::new(format!(
                                    "unknown MIR method body `{}`",
                                    method.function_name
                                ))
                            })?;
                        let evaluated_args = evaluate_named_args(args, env)?;
                        let writeback_places = evaluated_args
                            .iter()
                            .map(|argument| argument.writeback_place.clone())
                            .collect::<Vec<_>>();
                        let outcome = self.call_function_with_receiver_type(
                            &function,
                            Some(if method.receiver == Some(MirReceiverKind::Value) {
                                std::mem::replace(&mut receiver, Value::Unit)
                            } else {
                                try_clone_mir_value(&receiver)?
                            }),
                            evaluated_args,
                            expected_return_type,
                            None,
                            Some(&resolved_receiver_ty),
                        )?;
                        if method.receiver == Some(MirReceiverKind::BorrowMut) {
                            let updated = outcome.updated_receiver.ok_or_else(|| {
                                Diagnostic::new(format!(
                                    "mutable MIR method `{}` did not return an updated receiver",
                                    field
                                ))
                            })?;
                            if let Some(place) = receiver_place {
                                env.write_place(place, updated)?;
                            }
                        }
                        self.apply_borrowed_param_writebacks(
                            &function.params,
                            &writeback_places,
                            outcome.updated_params,
                            env,
                        )?;
                        Ok(outcome.value)
                    }
                    // Checked non-instance trait calls return through the trait-dispatch
                    // block above before reaching this exhaustive runtime-value match.
                    other => Err(Diagnostic::new(format!(
                        "unsupported MIR member call `{}` on `{}`",
                        field,
                        other.render()
                    ))),
                }
            }
        }
    }

    fn evaluate_extern_call(
        &mut self,
        call: &MirExternCall,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        self.evaluate_extern_call_with(call, args, env, |symbol, signature, arguments| {
            // SAFETY: semantic analysis admits only the documented FFI v0
            // surface. The remaining process-symbol signature and foreign
            // implementation obligations belong to the package author.
            unsafe { crate::ffi::call_process_symbol(symbol, signature, arguments) }
        })
    }

    fn evaluate_extern_call_with<F>(
        &mut self,
        call: &MirExternCall,
        args: &[MirArg],
        env: &mut Env,
        dispatch: F,
    ) -> Result<Value>
    where
        F: FnOnce(&str, &FfiSignature, &mut [FfiValue]) -> std::result::Result<FfiValue, FfiError>,
    {
        if call.abi != "C" {
            return Err(Diagnostic::coded(
                "AU4005",
                format!(
                    "FFI call to `{}` failed: unsupported runtime ABI `{}`",
                    call.symbol, call.abi
                ),
            ));
        }
        let evaluated = evaluate_named_args(args, env)?;
        let names = call
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        let ordered = bind_builtin_args(&names, evaluated).map_err(|error| {
            Diagnostic::coded(
                "AU4005",
                format!("FFI call to `{}` failed: {}", call.symbol, error.message),
            )
        })?;

        let parameter_types = call
            .params
            .iter()
            .map(ffi_type_for_extern_param)
            .collect::<Result<Vec<_>>>()?;
        let result_type = ffi_type_for_extern_result(&call.return_type)?;
        let signature = FfiSignature::new(parameter_types, result_type);
        let mut arguments = ordered
            .iter()
            .zip(&call.params)
            .map(|(argument, param)| ffi_value_from_runtime(&argument.value, &param.ty))
            .collect::<Result<Vec<_>>>()?;

        let result = dispatch(&call.symbol, &signature, &mut arguments);

        // Mutable byte views are fixed-length copy-in/out values. Apply the
        // engine's updated scratch bytes to the original Aura place before
        // interpreting the foreign return, so both backends share the same
        // post-call state even when return validation traps.
        for ((param, argument), ffi_value) in call.params.iter().zip(&ordered).zip(&arguments) {
            if param.passing != MirReceiverKind::BorrowMut {
                continue;
            }
            let FfiValue::Bytes(bytes) = ffi_value else {
                return Err(Diagnostic::coded(
                    "AU4005",
                    format!(
                        "FFI call to `{}` failed: mutable parameter `{}` did not marshal as bytes",
                        call.symbol, param.name
                    ),
                ));
            };
            let place = argument.writeback_place.as_deref().ok_or_else(|| {
                Diagnostic::coded(
                    "AU4005",
                    format!(
                        "FFI call to `{}` failed: mutable parameter `{}` requires a writeback place",
                        call.symbol, param.name
                    ),
                )
            })?;
            env.write_place(place, bytes_runtime_value(bytes))?;
        }

        let result = result.map_err(|error| ffi_runtime_diagnostic(&call.symbol, error))?;
        runtime_value_from_ffi(result, &call.return_type).map_err(|error| {
            Diagnostic::coded(
                "AU4005",
                format!("FFI call to `{}` failed: {}", call.symbol, error.message),
            )
        })
    }

    fn evaluate_function_value_call(
        &mut self,
        function: &Operand,
        args: &[MirArg],
        env: &mut Env,
        expected_return_type: Option<&Type>,
    ) -> Result<Value> {
        let function_value = self.evaluate_owned_operand(function, env)?;
        let Value::Function(function_value) = function_value else {
            return Err(Diagnostic::new(format!(
                "indirect MIR call expected a function value, found `{}`",
                function_value.render()
            )));
        };
        let evaluated_args = evaluate_named_args(args, env)?;
        self.evaluate_function_value_with_args(
            *function_value,
            evaluated_args,
            expected_return_type,
            env,
        )
    }

    fn evaluate_function_value_with_args(
        &mut self,
        function_value: FunctionValue,
        mut evaluated_args: Vec<EvaluatedMirArg>,
        expected_return_type: Option<&Type>,
        env: &mut Env,
    ) -> Result<Value> {
        let function = self
            .functions
            .get(&function_value.name)
            .cloned()
            .ok_or_else(|| {
                Diagnostic::new(format!("unknown MIR function `{}`", function_value.name))
            })?;
        if let Some(closure) = &function_value.closure_environment {
            let captures = closure.arguments(&function_value.name)?;
            let mut combined = Vec::with_capacity(captures.len() + evaluated_args.len());
            for capture in captures {
                let value = match &capture.source_place {
                    Some(source) => env.read_place(source)?,
                    None => capture.value,
                };
                combined.push(EvaluatedMirArg {
                    name: None,
                    value,
                    ty: Some(capture.ty),
                    writeback_place: capture.mutable.then_some(capture.source_place).flatten(),
                });
            }
            combined.append(&mut evaluated_args);
            let writeback_places = bind_function_writeback_places(&function.params, &combined)?;
            let outcome =
                self.call_function_for_target(&function, None, combined, expected_return_type)?;
            self.apply_borrowed_param_writebacks(
                &function.params,
                &writeback_places,
                outcome.updated_params,
                env,
            )?;
            return Ok(outcome.value);
        }
        let writeback_places = bind_function_writeback_places(&function.params, &evaluated_args)?;
        let outcome = self.call_function_for_value(
            &function,
            evaluated_args,
            &function_value.signature,
            expected_return_type,
        )?;
        self.apply_borrowed_param_writebacks(
            &function.params,
            &writeback_places,
            outcome.updated_params,
            env,
        )?;
        Ok(outcome.value)
    }

    fn start_task(&mut self, request: StartTaskRequest<'_>, env: &mut Env) -> Result<Value> {
        let StartTaskRequest {
            returns_handle,
            result_is_repeatable,
            stack_size,
            task_group,
            function,
            args,
            spawn_span,
        } = request;
        let function_value = self.evaluate_operand(function, env)?;
        let Value::Function(function_value) = function_value else {
            return Err(Diagnostic::new(format!(
                "MIR task start expected a function value, found `{}`",
                function_value.render()
            )));
        };
        let function = self
            .functions
            .get(&function_value.name)
            .cloned()
            .ok_or_else(|| {
                Diagnostic::new(format!("unknown MIR function `{}`", function_value.name))
            })?;
        self.require_task_startable_function(&function)?;
        // Capture evaluation and target-owned defaults both happen in the
        // parent task. The child receives a complete declaration-ordered
        // argument vector, so a dynamically selected target cannot defer its
        // default side effects until after scheduling.
        let mut supplied_args = evaluate_named_args(args, env)?;
        let is_closure = function_value.closure_environment.is_some();
        if let Some(environment) = &function_value.closure_environment {
            let captures = environment.arguments(&function_value.name)?;
            let mut combined = Vec::with_capacity(captures.len() + supplied_args.len());
            combined.extend(captures.into_iter().map(|capture| EvaluatedMirArg {
                name: None,
                value: capture.value,
                ty: Some(capture.ty),
                writeback_place: None,
            }));
            combined.append(&mut supplied_args);
            supplied_args = combined;
        }
        let bound_args = self.bind_function_args(
            &function,
            supplied_args,
            None,
            (!is_closure).then_some(&function_value.signature),
        )?;
        let mut producer_queues = Vec::new();
        for argument in &bound_args {
            collect_queue_handles(&argument.value, &mut producer_queues);
        }

        let value = self.evaluate_operand(task_group, env)?;
        let Value::TaskGroup(group_value) = value else {
            return Err(Diagnostic::new(
                "MIR task start requires a task-group value",
            ));
        };

        let cancellation = group_value.child_cancellation();

        let module = (*self.module).clone();
        let stdout = self.stdout.clone();
        let stdout_sink = self.stdout_sink.clone();
        let program_args = self.program_args.clone();
        let constant_states = self.constant_states.clone();
        let function_for_task = function.clone();
        let function_signature = function_value.signature.clone();
        let mut task_ancestry = self.task_ancestry.clone();
        let parent_frame = self
            .call_stack
            .last()
            .expect("task starts only while an Aura call frame is active");
        task_ancestry.push(RuntimeTaskFrame {
            task_function: function.name.clone(),
            task_entry_span: RuntimeSourceSpan::point(function.source_path.clone(), function.span),
            parent_function: parent_frame.function.clone(),
            spawn_span: RuntimeSourceSpan::point(parent_frame.span.path.clone(), spawn_span),
        });
        let entry = move || {
            let mut runtime = MirRuntime::new_with_stdout_sink_and_program_args(
                module,
                stdout,
                stdout_sink,
                cancellation,
                program_args,
            );
            runtime.constant_states = constant_states;
            runtime.task_ancestry = task_ancestry;
            if is_closure {
                runtime
                    .call_function_for_target(&function_for_task, None, bound_args, None)
                    .map(|outcome| outcome.value)
            } else {
                runtime
                    .call_function_for_value(
                        &function_for_task,
                        bound_args,
                        &function_signature,
                        None,
                    )
                    .map(|outcome| outcome.value)
            }
        };
        let register_before_submit = |task: &TaskValue| {
            group_value.register_task(task.clone());
            for queue in &producer_queues {
                queue.register_producer_task(task);
                queue.register_task_handle(task);
            }
        };
        let task = match stack_size {
            Some(stack_size) => {
                spawn_lightweight_task_with_stack_and_result_repeatability_registered(
                    stack_size,
                    result_is_repeatable,
                    entry,
                    register_before_submit,
                )
            }
            None => spawn_lightweight_task_with_result_repeatability_registered(
                result_is_repeatable,
                entry,
                register_before_submit,
            ),
        }?;

        if returns_handle {
            Ok(Value::Task(task))
        } else {
            Ok(Value::Unit)
        }
    }

    fn evaluate_task_stack_size(&self, operand: &Operand, env: &Env) -> Result<usize> {
        let value = self.evaluate_operand(operand, env)?;
        let Value::Int(bytes) = value else {
            return Err(Diagnostic::coded(
                "AU4005",
                "task stack size must evaluate to an int64 value",
            ));
        };
        let bytes = bytes.as_i128().ok_or_else(|| {
            Diagnostic::coded("AU4005", "task stack size must evaluate to an int64 value")
        })?;
        if bytes < i128::from(crate::call::MIN_TASK_STACK_BYTES)
            || bytes > i128::from(crate::call::MAX_TASK_STACK_BYTES)
        {
            return Err(Diagnostic::coded(
                "AU4005",
                format!(
                    "task stack size must be between {} and {} bytes, found {}",
                    crate::call::MIN_TASK_STACK_BYTES,
                    crate::call::MAX_TASK_STACK_BYTES,
                    bytes
                ),
            ));
        }
        usize::try_from(bytes)
            .map_err(|_| Diagnostic::coded("AU4005", "task stack size does not fit this platform"))
    }

    fn apply_borrowed_param_writebacks(
        &mut self,
        params: &[MirParam],
        writeback_places: &[Option<String>],
        updated_params: Vec<(usize, Value)>,
        env: &mut Env,
    ) -> Result<()> {
        for (index, value) in updated_params {
            let Some(param) = params.get(index) else {
                continue;
            };
            if param.passing != MirReceiverKind::BorrowMut {
                continue;
            }
            let place = writeback_places
                .get(index)
                .and_then(Option::as_deref)
                .ok_or_else(|| {
                    Diagnostic::new(format!(
                        "mutable borrowed MIR parameter `{}` requires a writeback place",
                        param.name
                    ))
                })?;
            env.write_place(place, value)?;
        }
        Ok(())
    }

    fn require_task_startable_function(&self, function: &MirFunction) -> Result<()> {
        if let Some(param) = function
            .params
            .iter()
            .find(|param| param.passing == MirReceiverKind::BorrowMut)
        {
            return Err(Diagnostic::coded(
                "AU3002",
                format!(
                "task starting does not support mutable MIR parameter `{}` on function `{}`; child tasks cannot write back through the starting call frame",
                param.name, function.name
            ),
            ));
        }
        Ok(())
    }

    fn evaluate_rng_method(
        &mut self,
        rng: RngValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "next_int" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["lo", "hi"], values)?;
                let lo = expect_i64_value(&bound[0].value, "Rng.next_int(...)")?;
                let hi = expect_i64_value(&bound[1].value, "Rng.next_int(...)")?;
                rng.next_int(lo, hi)
                    .map(|value| Value::Int(IntegerValue::from_signed(i128::from(value))))
                    .map_err(|_| invalid_random_bounds_diagnostic(lo, hi))
            }
            "next_float" => {
                let values = evaluate_named_args(args, env)?;
                bind_builtin_args(&[], values)?;
                Ok(Value::Float(rng.next_float()))
            }
            "shuffle" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["values"], values)?;
                let mut vector = match &bound[0].value {
                    Value::Vec(vector) => vector.clone(),
                    other => {
                        return Err(Diagnostic::new(format!(
                            "`Rng.shuffle(...)` expects `list[T]`, found `{}`",
                            other.render()
                        )))
                    }
                };
                let place = bound[0].writeback_place.as_deref().ok_or_else(|| {
                    Diagnostic::new("`Rng.shuffle(...)` requires a mutable list place")
                })?;
                rng.shuffle(&mut vector.elements);
                env.write_place(place, Value::Vec(vector))?;
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported Rng method `{field}` in MIR runtime"
            ))),
        }
    }

    fn evaluate_channel_method(
        &mut self,
        channel: ChannelValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "put" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["value", "timeout"], values)?.into_iter();
                let value = bound
                    .next()
                    .expect("bound Queue.put value should exist")
                    .value;
                let timeout_arg = bound.next().expect("bound Queue.put timeout should exist");
                let timeout =
                    expect_optional_timeout(Some(&timeout_arg.value), "put(timeout=...)")?;
                let outcome = channel
                    .send_with_timeout(value, timeout, Some(&self.cancellation))
                    .map_err(timer_error_to_diagnostic)?;
                match outcome {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(SendValueError::Closed(value)) => Ok(result_err(send_error_closed(*value))),
                    Err(SendValueError::Cancelled(value)) => {
                        Ok(result_err(send_error_cancelled(*value)))
                    }
                    Err(SendValueError::TimedOut(value)) => {
                        Ok(result_err(send_error_timed_out(*value)))
                    }
                    Err(SendValueError::Full(value)) => Ok(result_err(send_error_full(*value))),
                }
            }
            "try_put" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                let value = bound
                    .into_iter()
                    .next()
                    .expect("bound Queue.try_put value should exist")
                    .value;
                match channel.try_send_result(value) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(SendValueError::Closed(value)) => Ok(result_err(send_error_closed(*value))),
                    Err(SendValueError::Full(value)) => Ok(result_err(send_error_full(*value))),
                    Err(SendValueError::Cancelled(value)) => {
                        Ok(result_err(send_error_cancelled(*value)))
                    }
                    Err(SendValueError::TimedOut(value)) => {
                        Ok(result_err(send_error_timed_out(*value)))
                    }
                }
            }
            "get" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["timeout"], values)?;
                let timeout = expect_optional_timeout(Some(&bound[0].value), "get(timeout=...)")?;
                let outcome = channel
                    .recv_result_with_cancellation(timeout, Some(&self.cancellation))
                    .map_err(timer_error_to_diagnostic)?;
                Ok(match outcome {
                    RecvValueResult::Value(value) => queue_receive_item(value),
                    RecvValueResult::Closed => queue_receive_closed(),
                    RecvValueResult::TimedOut => queue_receive_timed_out(),
                    RecvValueResult::Cancelled => queue_receive_cancelled(),
                })
            }
            "__get_in_task_group" => {
                let [task_group_arg] = args else {
                    return Err(Diagnostic::new(
                        "internal queue iteration helper expects one task-group argument",
                    ));
                };
                let task_group = self.evaluate_operand(&task_group_arg.value, env)?;
                let Value::TaskGroup(group) = task_group else {
                    return Err(Diagnostic::new(format!(
                        "internal queue iteration helper expected `TaskGroup`, found `{}`",
                        task_group.render()
                    )));
                };
                Ok(
                    match recv_for_task_group_iteration(&channel, &self.cancellation, &group) {
                        RecvValueResult::Value(value) => queue_receive_item(value),
                        RecvValueResult::Closed => queue_receive_closed(),
                        RecvValueResult::TimedOut => queue_receive_timed_out(),
                        RecvValueResult::Cancelled => queue_receive_cancelled(),
                    },
                )
            }
            "__get_with_registered_producers" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "internal queue producer iteration helper expects no arguments",
                    ));
                }
                Ok(
                    match recv_for_registered_producers_iteration(&channel, &self.cancellation) {
                        RecvValueResult::Value(value) => queue_receive_item(value),
                        RecvValueResult::Closed => queue_receive_closed(),
                        RecvValueResult::TimedOut => queue_receive_timed_out(),
                        RecvValueResult::Cancelled => queue_receive_cancelled(),
                    },
                )
            }
            "get_or_none" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["timeout"], values)?;
                let timeout =
                    expect_optional_timeout(Some(&bound[0].value), "get_or_none(timeout=...)")?;
                let outcome = if args.is_empty() {
                    if self.cancellation.is_cancelled() {
                        RecvValueResult::Cancelled
                    } else {
                        match channel.try_recv() {
                            crate::runtime_value::TryRecvResult::Value(value) => {
                                RecvValueResult::Value(value)
                            }
                            crate::runtime_value::TryRecvResult::Closed => RecvValueResult::Closed,
                            crate::runtime_value::TryRecvResult::Empty => RecvValueResult::TimedOut,
                        }
                    }
                } else {
                    channel
                        .recv_result_with_cancellation(timeout, Some(&self.cancellation))
                        .map_err(timer_error_to_diagnostic)?
                };
                Ok(match outcome {
                    RecvValueResult::Value(value) => option_some(value),
                    RecvValueResult::Closed
                    | RecvValueResult::TimedOut
                    | RecvValueResult::Cancelled => option_none(),
                })
            }
            "get_or" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["default", "timeout"], values)?.into_iter();
                let default = bound
                    .next()
                    .expect("bound Queue.get_or default should exist")
                    .value;
                let timeout_arg = bound
                    .next()
                    .expect("bound Queue.get_or timeout should exist");
                let timeout =
                    expect_optional_timeout(Some(&timeout_arg.value), "get_or(timeout=...)")?;
                let outcome = if args.len() == 1 && args[0].name.as_deref() != Some("timeout") {
                    if self.cancellation.is_cancelled() {
                        RecvValueResult::Cancelled
                    } else {
                        match channel.try_recv() {
                            crate::runtime_value::TryRecvResult::Value(value) => {
                                RecvValueResult::Value(value)
                            }
                            crate::runtime_value::TryRecvResult::Closed => RecvValueResult::Closed,
                            crate::runtime_value::TryRecvResult::Empty => RecvValueResult::TimedOut,
                        }
                    }
                } else {
                    channel
                        .recv_result_with_cancellation(timeout, Some(&self.cancellation))
                        .map_err(timer_error_to_diagnostic)?
                };
                Ok(match outcome {
                    RecvValueResult::Value(value) => value,
                    RecvValueResult::Closed
                    | RecvValueResult::TimedOut
                    | RecvValueResult::Cancelled => default,
                })
            }
            "close" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`close` does not take arguments"));
                }
                channel.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported channel method `{}`",
                field
            ))),
        }
    }

    fn evaluate_array_place_method(
        &mut self,
        object_place: &str,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "shape" => {
                debug_assert!(args.is_empty());
                array_place_ref(env, object_place)?.shape_value()
            }
            "clone" => {
                debug_assert!(args.is_empty());
                array_place_ref(env, object_place)?
                    .try_clone()
                    .map(Value::Array)
            }
            "get" | "__index_option" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index"], values)?;
                let coordinates = array_coordinates_from_runtime(&bound[0].value)?;
                Ok(array_place_ref(env, object_place)?
                    .get_optional(&coordinates)?
                    .map(option_some)
                    .unwrap_or_else(option_none))
            }
            "__index" => {
                let values = evaluate_named_args(args, env)?;
                debug_assert_eq!(values.len(), 3);
                let coordinates = array_coordinates_from_runtime(&values[0].value)?;
                let line = self.mir_index_from_value(values[1].value.clone())?;
                let column = self.mir_index_from_value(values[2].value.clone())?;
                array_place_ref(env, object_place)?
                    .get(&coordinates)
                    .map_err(|mut error| {
                        error.span = Some(Span::new(line, column));
                        error
                    })
            }
            "set" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index", "value"], values)?;
                let coordinates = array_coordinates_from_runtime(&bound[0].value)?;
                let previous = array_place_mut(env, object_place)?
                    .set(&coordinates, bound[1].value.clone())?;
                Ok(option_some(previous))
            }
            "__set_index" => {
                let values = evaluate_named_args(args, env)?;
                debug_assert_eq!(values.len(), 4);
                let coordinates = array_coordinates_from_runtime(&values[0].value)?;
                let line = self.mir_index_from_value(values[2].value.clone())?;
                let column = self.mir_index_from_value(values[3].value.clone())?;
                array_place_mut(env, object_place)?
                    .set(&coordinates, values[1].value.clone())
                    .map_err(|mut error| {
                        error.span = Some(Span::new(line, column));
                        error
                    })?;
                Ok(Value::Unit)
            }
            "fill" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                array_place_mut(env, object_place)?.fill(bound[0].value.clone())?;
                Ok(Value::Unit)
            }
            "map" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["f"], values)?;
                let callback = checked_mir_function_ref(&bound[0].value);
                let output_type = checked_mir_function_return_type(&callback.signature).clone();
                debug_assert!(ArrayDType::from_type(&output_type).is_some());
                let (input_type, len, shape) = {
                    let array = array_place_ref(env, object_place)?;
                    (array.element_type(), array.len(), array.try_shape()?)
                };
                let mut mapped = try_array_buffer(len, "Array.map result")?;
                for index in 0..len {
                    let element = array_place_ref(env, object_place)?.value_at_flat(index);
                    mapped.push(self.evaluate_function_value_with_args(
                        callback.clone(),
                        vec![EvaluatedMirArg {
                            name: None,
                            value: element,
                            ty: Some(input_type.clone()),
                            writeback_place: None,
                        }],
                        Some(&output_type),
                        env,
                    )?);
                }
                ArrayValue::from_values(&output_type, shape, mapped).map(Value::Array)
            }
            "sum" | "min" | "max" | "mean" => {
                debug_assert!(args.is_empty());
                let reduction = match field {
                    "sum" => ArrayReduction::Sum,
                    "min" => ArrayReduction::Min,
                    "max" => ArrayReduction::Max,
                    _ => {
                        debug_assert_eq!(field, "mean");
                        ArrayReduction::Mean
                    }
                };
                array_place_ref(env, object_place)?.reduce(reduction)
            }
            "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "saturating_add"
            | "saturating_sub" | "saturating_mul" => {
                let bound = bind_mir_arg_refs(&["rhs"], args)?;
                let rhs = borrow_mir_operand(&bound[0].value, env)?;
                let operation = match field {
                    "wrapping_add" | "saturating_add" => ArrayBinaryOp::Add,
                    "wrapping_sub" | "saturating_sub" => ArrayBinaryOp::Sub,
                    _ => {
                        debug_assert!(matches!(field, "wrapping_mul" | "saturating_mul"));
                        ArrayBinaryOp::Mul
                    }
                };
                let mode = if field.starts_with("wrapping_") {
                    IntegerArithmeticMode::Wrapping
                } else {
                    IntegerArithmeticMode::Saturating
                };
                let array = array_place_ref(env, object_place)?;
                let result = match rhs.as_value() {
                    Value::Array(right) => array.binary(right, operation, mode),
                    scalar => array.scalar_binary(scalar, false, operation, mode),
                }?;
                Ok(Value::Array(result))
            }
            _ => unreachable!("semantic analysis lowers only supported Array methods"),
        }
    }

    fn evaluate_vec_method(
        &mut self,
        vector: VecValue,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "len" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`len` does not take arguments"));
                }
                Ok(Value::Int(IntegerValue::from_literal(
                    vector.elements.len() as u128,
                )))
            }
            "is_empty" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`is_empty` does not take arguments"));
                }
                Ok(Value::Bool(vector.elements.is_empty()))
            }
            "copy" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`copy` does not take arguments"));
                }
                Ok(Value::Vec(vector))
            }
            "append" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                let mut updated = vector;
                let Some(value) = bound.into_iter().next().map(|arg| arg.value) else {
                    return Err(Diagnostic::new(
                        "internal error: `append` should bind one argument",
                    ));
                };
                updated.elements.push(value);
                let updated_value = Value::Vec(updated);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`append` requires a mutable list place"));
                };
                env.write_place(place, updated_value)?;
                Ok(Value::Unit)
            }
            "pop" => {
                let values = evaluate_named_args(args, env)?;
                let mut updated = vector;
                let index_value = if values.is_empty() {
                    Value::Int(IntegerValue::from_signed(-1))
                } else {
                    bind_builtin_args(&["index"], values)?[0].value.clone()
                };
                let (supplied_index, index) =
                    self.mir_vec_index_from_value(index_value, updated.elements.len())?;
                let Some(index) = index.filter(|index| *index < updated.elements.len()) else {
                    return Err(Diagnostic::coded(
                        "AU4003",
                        format!(
                            "list pop index `{supplied_index}` is out of bounds for length `{}`",
                            updated.elements.len()
                        ),
                    ));
                };
                let value = updated.elements.remove(index);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`pop` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(value)
            }
            "get" | "__index_option" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index"], values)?;
                let (_, index) =
                    self.mir_vec_index_from_value(bound[0].value.clone(), vector.elements.len())?;
                match index.and_then(|index| vector.elements.get(index)) {
                    Some(value) => try_clone_mir_value(value).map(option_some),
                    None => Ok(option_none()),
                }
            }
            "__take_index_option" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index"], values)?;
                let mut updated = vector;
                let (_, index) =
                    self.mir_vec_index_from_value(bound[0].value.clone(), updated.elements.len())?;
                let value = index
                    .filter(|index| *index < updated.elements.len())
                    .map(|index| updated.elements.remove(index));
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new(
                        "owned vector iteration requires its private source place",
                    ));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(value.map(option_some).unwrap_or_else(option_none))
            }
            "__index" => {
                let values = evaluate_named_args(args, env)?;
                if values.len() != 3 {
                    return Err(Diagnostic::new(
                        "internal vector indexing requires index, line, and column operands",
                    ));
                }
                let line = self.mir_index_from_value(values[1].value.clone())?;
                let column = self.mir_index_from_value(values[2].value.clone())?;
                let (supplied_index, index) =
                    self.mir_vec_index_from_value(values[0].value.clone(), vector.elements.len())?;
                let value = index
                    .and_then(|index| vector.elements.get(index))
                    .ok_or_else(|| {
                        Diagnostic::at(
                            crate::diag::Span::new(line, column),
                            format!(
                                "list index `{}` is out of bounds for length `{}`",
                                supplied_index,
                                vector.elements.len()
                            ),
                        )
                    })?;
                try_clone_mir_value(value)
            }
            "__slice" => {
                let values = evaluate_named_args(args, env)?;
                let (start, end, span) = self.mir_slice_args(values)?;
                slice_vec_owned(&vector, start, end)
                    .map(Value::Vec)
                    .map_err(|mut error| {
                        error.span = Some(span);
                        error
                    })
            }
            "set" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["index", "value"], values)?.into_iter();
                let index_value = bound
                    .next()
                    .expect("bound Vec.set index should exist")
                    .value;
                let value = bound
                    .next()
                    .expect("bound Vec.set value should exist")
                    .value;
                let mut updated = vector;
                let (supplied_index, index) =
                    self.mir_vec_index_from_value(index_value, updated.elements.len())?;
                let Some(index) = index.filter(|index| *index < updated.elements.len()) else {
                    return Err(Diagnostic::new(format!(
                        "list set index `{}` is out of bounds for length `{}`",
                        supplied_index,
                        updated.elements.len()
                    )));
                };
                let previous = std::mem::replace(&mut updated.elements[index], value);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`set` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(previous)
            }
            "__set_index" => {
                let values = evaluate_named_args(args, env)?;
                if values.len() != 4 {
                    return Err(Diagnostic::new(
                        "internal indexed assignment requires index, value, line, and column operands",
                    ));
                }
                let mut values = values.into_iter();
                let index_value = values.next().expect("index argument should exist").value;
                let value = values.next().expect("value argument should exist").value;
                let line =
                    self.mir_index_from_value(values.next().expect("line should exist").value)?;
                let column =
                    self.mir_index_from_value(values.next().expect("column should exist").value)?;
                let mut updated = vector;
                let (supplied_index, index) =
                    self.mir_vec_index_from_value(index_value, updated.elements.len())?;
                let Some(index) = index.filter(|index| *index < updated.elements.len()) else {
                    return Err(Diagnostic::at(
                        crate::diag::Span::new(line, column),
                        format!(
                            "list index `{}` is out of bounds for length `{}`",
                            supplied_index,
                            updated.elements.len()
                        ),
                    ));
                };
                updated.elements[index] = value;
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new(
                        "indexed assignment requires a mutable list place",
                    ));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "remove" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                let mut updated = vector;
                let Some(index) = updated
                    .elements
                    .iter()
                    .position(|candidate| *candidate == bound[0].value)
                else {
                    return Err(
                        Diagnostic::coded("AU4008", "collection value was not found").with_help(
                            "check `value in values` before removing when absence is expected",
                        ),
                    );
                };
                updated.elements.remove(index);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`remove` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "index" | "count" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                if field == "count" {
                    let count = vector
                        .elements
                        .iter()
                        .filter(|candidate| **candidate == bound[0].value)
                        .count();
                    return Ok(Value::Int(IntegerValue::from_literal(count as u128)));
                }
                let Some(index) = vector
                    .elements
                    .iter()
                    .position(|candidate| *candidate == bound[0].value)
                else {
                    return Err(
                        Diagnostic::coded("AU4008", "collection value was not found").with_help(
                            "check `value in values` before searching when absence is expected",
                        ),
                    );
                };
                Ok(Value::Int(IntegerValue::from_literal(index as u128)))
            }
            "swap" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["first", "second"], values)?;
                let mut updated = vector;
                let (supplied_first, first) =
                    self.mir_vec_index_from_value(bound[0].value.clone(), updated.elements.len())?;
                let (supplied_second, second) =
                    self.mir_vec_index_from_value(bound[1].value.clone(), updated.elements.len())?;
                let (Some(first), Some(second)) = (
                    first.filter(|index| *index < updated.elements.len()),
                    second.filter(|index| *index < updated.elements.len()),
                ) else {
                    return Err(Diagnostic::new(format!(
                        "list swap indices `{}` and `{}` are out of bounds for length `{}`",
                        supplied_first,
                        supplied_second,
                        updated.elements.len()
                    )));
                };
                updated.elements.swap(first, second);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`swap` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "contains" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                Ok(Value::Bool(
                    vector
                        .elements
                        .iter()
                        .any(|candidate| *candidate == bound[0].value),
                ))
            }
            "insert" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["index", "value"], values)?.into_iter();
                let index_value = bound
                    .next()
                    .expect("bound Vec.insert index should exist")
                    .value;
                let value = bound
                    .next()
                    .expect("bound Vec.insert value should exist")
                    .value;
                let mut updated = vector;
                let supplied_index = expect_i64_value(&index_value, "list.insert index")? as i128;
                let len = updated.elements.len() as i128;
                let adjusted = if supplied_index < 0 {
                    len.saturating_add(supplied_index)
                } else {
                    supplied_index
                };
                let index = adjusted.clamp(0, len) as usize;
                updated.elements.insert(index, value);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`insert` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "clear" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`clear` does not take arguments"));
                }
                let mut updated = vector;
                updated.elements.clear();
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`clear` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "reverse" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`reverse` does not take arguments"));
                }
                let mut updated = vector;
                updated.elements.reverse();
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`reverse` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "extend" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["other"], values)?;
                let Value::Vec(other) = bound
                    .into_iter()
                    .next()
                    .expect("bound Vec.extend value should exist")
                    .value
                else {
                    return Err(Diagnostic::new("`extend` requires another `list[T]` value"));
                };
                let mut updated = vector;
                updated.elements.extend(other.elements);
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`extend` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            "reserve" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["additional"], values)?;
                let additional = expect_i64_value(&bound[0].value, "list.reserve(...)")?;
                let additional = usize::try_from(additional).map_err(|_| {
                    Diagnostic::coded("AU4003", "collection capacity cannot be negative")
                })?;
                let mut updated = vector;
                updated
                    .elements
                    .try_reserve(additional)
                    .map_err(|_| Diagnostic::coded("AU4005", "list capacity allocation failed"))?;
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`reserve` requires a mutable list place"));
                };
                env.write_place(place, Value::Vec(updated))?;
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported vector method `{}`",
                field
            ))),
        }
    }

    fn evaluate_map_method(
        &mut self,
        map: MapValue,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "len" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`len` does not take arguments"));
                }
                Ok(Value::Int(IntegerValue::from_literal(
                    map.entries.len() as u128
                )))
            }
            "is_empty" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`is_empty` does not take arguments"));
                }
                Ok(Value::Bool(map.entries.is_empty()))
            }
            "copy" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`copy` does not take arguments"));
                }
                Ok(Value::Map(map))
            }
            "get" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["key"], values)?;
                match map
                    .entries
                    .iter()
                    .find(|(candidate_key, _)| *candidate_key == bound[0].value)
                {
                    Some((_, value)) => try_clone_mir_value(value).map(option_some),
                    None => Ok(option_none()),
                }
            }
            "__index" => {
                let values = evaluate_named_args(args, env)?;
                if values.len() != 3 {
                    return Err(Diagnostic::new(
                        "internal map indexing requires key, line, and column operands",
                    ));
                }
                let key = values[0].value.clone();
                let line = self.mir_index_from_value(values[1].value.clone())?;
                let column = self.mir_index_from_value(values[2].value.clone())?;
                let value = map
                    .entries
                    .iter()
                    .find(|(candidate_key, _)| *candidate_key == key)
                    .map(|(_, value)| value)
                    .ok_or_else(|| {
                        Diagnostic::coded_at(
                            "AU4003",
                            crate::diag::Span::new(line, column),
                            format!("dict key `{}` was not present", key.render()),
                        )
                    })?;
                try_clone_mir_value(value)
            }
            "set" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["key", "value"], values)?.into_iter();
                let key = bound.next().expect("bound Map.set key should exist").value;
                let value = bound
                    .next()
                    .expect("bound Map.set value should exist")
                    .value;
                let mut updated = map;
                let previous = if let Some(index) = updated
                    .entries
                    .iter()
                    .position(|(candidate_key, _)| *candidate_key == key)
                {
                    Some(std::mem::replace(&mut updated.entries[index].1, value))
                } else {
                    updated.entries.push((key, value));
                    None
                };
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new(
                        "indexed assignment requires a mutable dict place",
                    ));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(previous.map(option_some).unwrap_or_else(option_none))
            }
            "__set_index" => {
                let values = evaluate_named_args(args, env)?;
                if values.len() != 4 {
                    return Err(Diagnostic::new(
                        "internal map indexed assignment requires key, value, line, and column operands",
                    ));
                }
                let mut values = values.into_iter();
                let mut updated = map;
                let key = values.next().expect("map key should exist").value;
                let value = values.next().expect("map value should exist").value;
                if let Some(index) = updated
                    .entries
                    .iter()
                    .position(|(candidate_key, _)| *candidate_key == key)
                {
                    updated.entries[index].1 = value;
                } else {
                    updated.entries.push((key, value));
                }
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new(
                        "indexed assignment requires a mutable dict place",
                    ));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(Value::Unit)
            }
            "remove" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["key"], values)?;
                let mut updated = map;
                let removed = if let Some(index) = updated
                    .entries
                    .iter()
                    .position(|(candidate_key, _)| *candidate_key == bound[0].value)
                {
                    Some(updated.entries.remove(index).1)
                } else {
                    None
                };
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`remove` requires a mutable dict place"));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(removed.map(option_some).unwrap_or_else(option_none))
            }
            "contains_key" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["key"], values)?;
                Ok(Value::Bool(map.entries.iter().any(|(candidate_key, _)| {
                    *candidate_key == bound[0].value
                })))
            }
            "keys" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`keys` does not take arguments"));
                }
                let mut elements =
                    try_array_buffer(map.entries.len(), "Array-containing Map key copy")?;
                for (key, _) in &map.entries {
                    elements.push(try_clone_mir_value(key)?);
                }
                Ok(Value::Vec(VecValue {
                    element_type: map.key_type.clone(),
                    elements,
                }))
            }
            "values" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`values` does not take arguments"));
                }
                let mut elements =
                    try_array_buffer(map.entries.len(), "Array-containing Map value copy")?;
                for (_, value) in &map.entries {
                    elements.push(try_clone_mir_value(value)?);
                }
                Ok(Value::Vec(VecValue {
                    element_type: map.value_type.clone(),
                    elements,
                }))
            }
            "items" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(format!(
                        "`{}` does not take arguments",
                        field
                    )));
                }
                let mut elements =
                    try_array_buffer(map.entries.len(), "Array-containing Map entry copy")?;
                for (key, value) in &map.entries {
                    elements.push(Value::Tuple(TupleValue {
                        element_types: vec![map.key_type.clone(), map.value_type.clone()],
                        elements: vec![try_clone_mir_value(key)?, try_clone_mir_value(value)?],
                    }));
                }
                Ok(Value::Vec(VecValue {
                    element_type: Type::Tuple(vec![map.key_type.clone(), map.value_type.clone()]),
                    elements,
                }))
            }
            "clear" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`clear` does not take arguments"));
                }
                let mut updated = map;
                updated.entries.clear();
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`clear` requires a mutable map place"));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(Value::Unit)
            }
            "update" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["other"], values)?;
                let Value::Map(other) = bound
                    .into_iter()
                    .next()
                    .expect("bound dict.update value should exist")
                    .value
                else {
                    return Err(Diagnostic::new(
                        "`update` requires another `dict[K, V]` value",
                    ));
                };
                let mut updated = map;
                for (key, value) in other.entries {
                    if let Some(index) = updated
                        .entries
                        .iter()
                        .position(|(candidate_key, _)| *candidate_key == key)
                    {
                        updated.entries[index].1 = value;
                    } else {
                        updated.entries.push((key, value));
                    }
                }
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`update` requires a mutable dict place"));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(Value::Unit)
            }
            "reserve" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["additional"], values)?;
                let additional = expect_i64_value(&bound[0].value, "dict.reserve(...)")?;
                let additional = usize::try_from(additional).map_err(|_| {
                    Diagnostic::coded("AU4003", "collection capacity cannot be negative")
                })?;
                let mut updated = map;
                updated.entries.try_reserve(additional).map_err(|_| {
                    Diagnostic::coded("AU4005", "dictionary capacity allocation failed")
                })?;
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`reserve` requires a mutable dict place"));
                };
                env.write_place(place, Value::Map(updated))?;
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported dict method `{}`",
                field
            ))),
        }
    }

    fn evaluate_string_method(
        &mut self,
        text: String,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "len" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`len` does not take arguments"));
                }
                Ok(Value::Int(IntegerValue::from_literal(
                    text.chars().count() as u128
                )))
            }
            "byte_len" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`byte_len` does not take arguments"));
                }
                Ok(Value::Int(IntegerValue::from_literal(text.len() as u128)))
            }
            "__slice" => {
                let values = evaluate_named_args(args, env)?;
                let (start, end, span) = self.mir_slice_args(values)?;
                slice_string_owned(&text, start, end)
                    .map(Value::String)
                    .map_err(|mut error| {
                        error.span = Some(span);
                        error
                    })
            }
            "contains" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(needle) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`contains` requires a `str` argument"));
                };
                Ok(Value::Bool(text.contains(&needle)))
            }
            "starts_with" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(prefix) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`starts_with` requires a `str` argument"));
                };
                Ok(Value::Bool(text.starts_with(&prefix)))
            }
            "ends_with" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(suffix) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`ends_with` requires a `str` argument"));
                };
                Ok(Value::Bool(text.ends_with(&suffix)))
            }
            "split" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(separator) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`split` requires a `str` argument"));
                };
                Ok(Value::Vec(VecValue {
                    element_type: Type::named("str"),
                    elements: text
                        .split(&separator)
                        .map(|part| Value::String(part.to_string()))
                        .collect(),
                }))
            }
            "replace" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["from", "to"], values)?;
                let Value::String(from) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`replace` requires `str` for `from`"));
                };
                let Value::String(to) = bound[1].value.clone() else {
                    return Err(Diagnostic::new("`replace` requires `str` for `to`"));
                };
                Ok(Value::String(text.replace(&from, &to)))
            }
            "to_lower" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`to_lower` does not take arguments"));
                }
                Ok(Value::String(text.to_lowercase()))
            }
            "to_upper" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`to_upper` does not take arguments"));
                }
                Ok(Value::String(text.to_uppercase()))
            }
            "join" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["parts"], values)?;
                let Value::Vec(parts) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`join` requires `list[str]`"));
                };
                let mut rendered_parts = Vec::new();
                for value in parts.elements {
                    let Value::String(part) = value else {
                        return Err(Diagnostic::new("`join` requires `list[str]`"));
                    };
                    rendered_parts.push(part);
                }
                Ok(Value::String(rendered_parts.join(&text)))
            }
            "add" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["other"], values)?;
                let Value::String(other) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`add` requires a `str` argument"));
                };
                Ok(Value::String(text + &other))
            }
            "strip_prefix" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(prefix) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`strip_prefix` requires a `str` argument"));
                };
                Ok(text
                    .strip_prefix(&prefix)
                    .map(|rest| option_some(Value::String(rest.to_string())))
                    .unwrap_or_else(option_none))
            }
            "strip_suffix" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["text"], values)?;
                let Value::String(suffix) = bound[0].value.clone() else {
                    return Err(Diagnostic::new("`strip_suffix` requires a `str` argument"));
                };
                Ok(text
                    .strip_suffix(&suffix)
                    .map(|rest| option_some(Value::String(rest.to_string())))
                    .unwrap_or_else(option_none))
            }
            "trim" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`trim` does not take arguments"));
                }
                Ok(Value::String(text.trim().to_string()))
            }
            "clone" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`clone` does not take arguments"));
                }
                Ok(Value::String(text))
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported string method `{}`",
                field
            ))),
        }
    }

    fn evaluate_set_method(
        &mut self,
        set: SetValue,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "len" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`len` does not take arguments"));
                }
                Ok(Value::Int(IntegerValue::from_literal(
                    set.elements.len() as u128
                )))
            }
            "is_empty" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`is_empty` does not take arguments"));
                }
                Ok(Value::Bool(set.elements.is_empty()))
            }
            "copy" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`copy` does not take arguments"));
                }
                Ok(Value::Set(set))
            }
            "contains" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                Ok(Value::Bool(
                    set.elements
                        .iter()
                        .any(|candidate| *candidate == bound[0].value),
                ))
            }
            "add" => {
                let values = evaluate_named_args(args, env)?;
                let value = bind_builtin_args(&["value"], values)?
                    .into_iter()
                    .next()
                    .expect("bound set.add value should exist")
                    .value;
                let mut updated = set;
                let inserted = if updated.elements.contains(&value) {
                    false
                } else {
                    updated.elements.push(value);
                    true
                };
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`add` requires a mutable set place"));
                };
                env.write_place(place, Value::Set(updated))?;
                let _ = inserted;
                Ok(Value::Unit)
            }
            "remove" | "discard" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["value"], values)?;
                let mut updated = set;
                let removed = if let Some(index) = updated
                    .elements
                    .iter()
                    .position(|candidate| *candidate == bound[0].value)
                {
                    updated.elements.remove(index);
                    true
                } else {
                    false
                };
                if !removed && field == "remove" {
                    return Err(
                        Diagnostic::coded("AU4008", "collection value was not found").with_help(
                            "check `value in values` before removing when absence is expected",
                        ),
                    );
                }
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`remove` requires a mutable set place"));
                };
                env.write_place(place, Value::Set(updated))?;
                Ok(Value::Unit)
            }
            "clear" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`clear` does not take arguments"));
                }
                let mut updated = set;
                updated.elements.clear();
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`clear` requires a mutable set place"));
                };
                env.write_place(place, Value::Set(updated))?;
                Ok(Value::Unit)
            }
            "reserve" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["additional"], values)?;
                let additional = expect_i64_value(&bound[0].value, "set.reserve(...)")?;
                let additional = usize::try_from(additional).map_err(|_| {
                    Diagnostic::coded("AU4003", "collection capacity cannot be negative")
                })?;
                let mut updated = set;
                updated
                    .elements
                    .try_reserve(additional)
                    .map_err(|_| Diagnostic::coded("AU4005", "set capacity allocation failed"))?;
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new("`reserve` requires a mutable set place"));
                };
                env.write_place(place, Value::Set(updated))?;
                Ok(Value::Unit)
            }
            "__index_option" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index"], values)?;
                let index = self.mir_index_from_value(bound[0].value.clone())?;
                match set.elements.get(index) {
                    Some(value) => try_clone_mir_value(value).map(option_some),
                    None => Ok(option_none()),
                }
            }
            "__take_index_option" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["index"], values)?;
                let index = self.mir_index_from_value(bound[0].value.clone())?;
                let mut updated = set;
                let value =
                    (index < updated.elements.len()).then(|| updated.elements.remove(index));
                let Some(place) = receiver_place else {
                    return Err(Diagnostic::new(
                        "owned set iteration requires its private source place",
                    ));
                };
                env.write_place(place, Value::Set(updated))?;
                Ok(value.map(option_some).unwrap_or_else(option_none))
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported set method `{}`",
                field
            ))),
        }
    }

    fn mir_slice_args(
        &self,
        values: Vec<EvaluatedMirArg>,
    ) -> Result<(Option<i128>, Option<i128>, Span)> {
        let Ok([start, has_start, end, has_end, line, column]) =
            <Vec<EvaluatedMirArg> as TryInto<[EvaluatedMirArg; 6]>>::try_into(values)
        else {
            return Err(Diagnostic::new(
                "internal slicing requires start, has_start, end, has_end, line, and column operands",
            ));
        };
        let Value::Int(start) = start.value else {
            return Err(Diagnostic::new(
                "internal slice start operand must be an integer",
            ));
        };
        let start = start
            .as_i128()
            .ok_or_else(|| Diagnostic::new("slice start is outside the supported signed range"))?;
        let Value::Bool(has_start) = has_start.value else {
            return Err(Diagnostic::new(
                "internal slice has_start operand must be bool",
            ));
        };
        let Value::Int(end) = end.value else {
            return Err(Diagnostic::new(
                "internal slice end operand must be an integer",
            ));
        };
        let end = end
            .as_i128()
            .ok_or_else(|| Diagnostic::new("slice end is outside the supported signed range"))?;
        let Value::Bool(has_end) = has_end.value else {
            return Err(Diagnostic::new(
                "internal slice has_end operand must be bool",
            ));
        };
        let line = self.mir_index_from_value(line.value)?;
        let column = self.mir_index_from_value(column.value)?;
        Ok((
            has_start.then_some(start),
            has_end.then_some(end),
            Span::new(line, column),
        ))
    }

    fn mir_index_from_value(&self, value: Value) -> Result<usize> {
        let Value::Int(value) = value else {
            return Err(Diagnostic::new("list indices must be integers"));
        };
        let Some(index) = value.as_i128() else {
            return Err(Diagnostic::new(
                "list index is outside the supported signed range",
            ));
        };
        if index < 0 {
            return Err(Diagnostic::new(format!(
                "list index `{}` cannot be negative",
                index
            )));
        }
        usize::try_from(index)
            .map_err(|_| Diagnostic::new("list index does not fit in the MIR address space"))
    }

    fn mir_vec_index_from_value(&self, value: Value, len: usize) -> Result<(i128, Option<usize>)> {
        let Value::Int(value) = value else {
            return Err(Diagnostic::new("list indices must be integers"));
        };
        let Some(supplied) = value.as_i128() else {
            return Err(Diagnostic::new(
                "list index is outside the supported signed range",
            ));
        };
        // Rust's supported pointer widths fit losslessly in i128, so this conversion
        // has no runtime failure case to defend or cover.
        let len = len as i128;
        let normalized = if supplied < 0 {
            // `len` is non-negative and `supplied` is negative, so this sum
            // cannot overflow either end of the i128 range.
            len + supplied
        } else {
            supplied
        };
        Ok((supplied, usize::try_from(normalized).ok()))
    }

    fn evaluate_task_method(
        &mut self,
        task: TaskValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "result" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["timeout"], values)?;
                let timeout =
                    expect_optional_timeout(Some(&bound[0].value), "result(timeout=...)")?;
                self.join_task(task, timeout)
            }
            "result_or_none" => {
                let values = evaluate_named_args(args, env)?;
                let bound = bind_builtin_args(&["timeout"], values)?;
                let timeout =
                    expect_optional_timeout(Some(&bound[0].value), "result_or_none(timeout=...)")?;
                task.claim_result_observation()?;
                let outcome = if args.is_empty() {
                    if self.cancellation.is_cancelled() {
                        TaskWaitStatus::Cancelled
                    } else if let Some(result) = task.completed_result_observed() {
                        TaskWaitStatus::Ready(match result {
                            crate::runtime_value::TaskExecutionResult::Ready(result) => result,
                            crate::runtime_value::TaskExecutionResult::Cancelled => {
                                return Ok(option_none());
                            }
                        })
                    } else {
                        TaskWaitStatus::TimedOut
                    }
                } else {
                    task.wait_result_with_cancellation_observed(timeout, Some(&self.cancellation))
                        .map_err(timer_error_to_diagnostic)?
                };
                Ok(match outcome {
                    TaskWaitStatus::Ready(result) => {
                        result.map(option_some).unwrap_or_else(|_| option_none())
                    }
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => option_none(),
                })
            }
            "result_or" => {
                let values = evaluate_named_args(args, env)?;
                let mut bound = bind_builtin_args(&["default", "timeout"], values)?.into_iter();
                let default = bound
                    .next()
                    .expect("bound Task.result_or default should exist")
                    .value;
                let timeout_arg = bound
                    .next()
                    .expect("bound Task.result_or timeout should exist");
                let timeout =
                    expect_optional_timeout(Some(&timeout_arg.value), "result_or(timeout=...)")?;
                task.claim_result_observation()?;
                let outcome = if args.len() == 1 && args[0].name.as_deref() != Some("timeout") {
                    if self.cancellation.is_cancelled() {
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
                    }
                } else {
                    task.wait_result_with_cancellation_observed(timeout, Some(&self.cancellation))
                        .map_err(timer_error_to_diagnostic)?
                };
                Ok(match outcome {
                    TaskWaitStatus::Ready(result) => result.unwrap_or(default),
                    TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => default,
                })
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported task method `{}`",
                field
            ))),
        }
    }

    fn evaluate_task_group_method(
        &mut self,
        group: TaskGroupValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "cancel" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new("`cancel` does not take arguments"));
                }
                group.cancel();
                Ok(Value::Unit)
            }
            "start" => {
                if args.is_empty() {
                    return Err(Diagnostic::new(format!(
                        "`{}` expects a target function followed by its arguments",
                        field
                    )));
                }
                Err(Diagnostic::new(
                    "task-group start should lower to MIR `Spawn` directly",
                ))
            }
            _ => {
                let _ = env;
                Err(Diagnostic::new(format!(
                    "unsupported task-group method `{}`",
                    field
                )))
            }
        }
    }

    fn evaluate_builtin_io_call(
        &mut self,
        name: &str,
        values: Vec<EvaluatedMirArg>,
    ) -> Result<Value> {
        match name {
            "process::inherit" => {
                bind_builtin_args(&[], values)?;
                Ok(process_stdio_inherit())
            }
            "process::null" => {
                bind_builtin_args(&[], values)?;
                Ok(process_stdio_null())
            }
            "process::pipe" => {
                bind_builtin_args(&[], values)?;
                Ok(process_stdio_pipe())
            }
            "process::supervisor" => {
                bind_builtin_args(&[], values)?;
                Ok(Value::ProcessSupervisor(ProcessSupervisorValue::new()))
            }
            "process::start" => {
                let bound = bind_builtin_args(
                    &[
                        "command", "cwd", "env", "stdin", "stdout", "stderr", "group",
                    ],
                    values,
                )?;
                let command = expect_command_vec(&bound[0].value, "process.start(...)")?;
                if command.is_empty() {
                    return Ok(result_err(process_error_no_command()));
                }
                let cwd = expect_optional_string_value(&bound[1].value, "process.start(...)")?;
                let env = expect_headers_map(&bound[2].value, "process.start(...)")?;
                let stdin = decode_process_stdio(&bound[3].value, "process.start(...)")?;
                let stdout = decode_process_stdio(&bound[4].value, "process.start(...)")?;
                let stderr = decode_process_stdio(&bound[5].value, "process.start(...)")?;
                let group = expect_bool_value(&bound[6].value, "process.start(...)")?;
                match ProcessChildValue::spawn(command, cwd, env, stdin, stdout, stderr, group) {
                    Ok(child) => Ok(result_ok(Value::ProcessChild(child))),
                    Err(error) => Ok(result_err(process_error_spawn(error.to_string()))),
                }
            }
            "process::run" => {
                let bound = bind_builtin_args(
                    &[
                        "command", "cwd", "env", "stdin", "stdout", "stderr", "timeout", "group",
                    ],
                    values,
                )?;
                let command = expect_command_vec(&bound[0].value, "process.run(...)")?;
                if command.is_empty() {
                    return Ok(result_err(process_error_no_command()));
                }
                let cwd = expect_optional_string_value(&bound[1].value, "process.run(...)")?;
                let env = expect_headers_map(&bound[2].value, "process.run(...)")?;
                let stdin = decode_process_stdio(&bound[3].value, "process.run(...)")?;
                let stdout = decode_process_stdio(&bound[4].value, "process.run(...)")?;
                let stderr = decode_process_stdio(&bound[5].value, "process.run(...)")?;
                let timeout =
                    match expect_process_optional_timeout(&bound[6].value, "process.run(...)") {
                        Ok(timeout) => timeout,
                        Err(error) => return Ok(result_err(error)),
                    };
                let group = expect_bool_value(&bound[7].value, "process.run(...)")?;
                let child =
                    match ProcessChildValue::spawn(command, cwd, env, stdin, stdout, stderr, group)
                    {
                        Ok(child) => child,
                        Err(error) => {
                            return Ok(result_err(process_error_spawn(error.to_string())))
                        }
                    };
                let stdout_task = child
                    .stdout()
                    .map(|pipe| {
                        let cancellation = self.cancellation.clone();
                        spawn_lightweight_task(move || {
                            match pipe.read_all_bytes(Some(&cancellation)) {
                                Ok(bytes) => Ok(bytes_vec_value(bytes)),
                                Err(error) => Err(Diagnostic::new(format!(
                                    "process stdout capture failed: {}",
                                    error
                                ))),
                            }
                        })
                    })
                    .transpose()?;
                let stderr_task = child
                    .stderr()
                    .map(|pipe| {
                        let cancellation = self.cancellation.clone();
                        spawn_lightweight_task(move || {
                            match pipe.read_all_bytes(Some(&cancellation)) {
                                Ok(bytes) => Ok(bytes_vec_value(bytes)),
                                Err(error) => Err(Diagnostic::new(format!(
                                    "process stderr capture failed: {}",
                                    error
                                ))),
                            }
                        })
                    })
                    .transpose()?;
                let status = match child.wait(timeout, Some(&self.cancellation)) {
                    ProcessChildWaitStatus::Exited(status) => status,
                    ProcessChildWaitStatus::TimedOut => {
                        child.close();
                        return Ok(result_err(process_error_timed_out()));
                    }
                    ProcessChildWaitStatus::Cancelled => {
                        child.close();
                        return Ok(result_err(process_error_cancelled()));
                    }
                    ProcessChildWaitStatus::Failed(error) => {
                        child.close();
                        return Ok(result_err(process_error_io(error)));
                    }
                };
                let stdout = self.await_process_capture_task(stdout_task, "stdout")?;
                let stderr = self.await_process_capture_task(stderr_task, "stderr")?;
                Ok(result_ok(Value::ProcessCompleted(
                    ProcessCompletedValue::new(
                        crate::runtime_value::process_exit_status(status),
                        stdout,
                        stderr,
                    ),
                )))
            }
            "fs::read_to_string" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "fs.read_to_string(...)")?;
                match run_blocking_io(
                    move || {
                        let bytes = read_file_limited(&path, "fs.read_to_string")?;
                        String::from_utf8(bytes)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
                    },
                    Some(&self.cancellation),
                ) {
                    Ok(text) => Ok(result_ok(Value::String(text))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::read_bytes" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "fs.read_bytes(...)")?;
                match run_blocking_io(
                    move || read_file_limited(&path, "fs.read_bytes"),
                    Some(&self.cancellation),
                ) {
                    Ok(bytes) => Ok(result_ok(bytes_vec_value(bytes))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::write_string" | "fs::append_string" => {
                let bound = bind_builtin_args(&["path", "text"], values)?;
                let (path, text) = match (&bound[0].value, &bound[1].value) {
                    (Value::String(path), Value::String(text)) => (path, text),
                    (other, _) if !matches!(other, Value::String(_)) => {
                        return Err(Diagnostic::new(format!(
                            "`{}` expects `str` for `path`",
                            name
                        )))
                    }
                    (_, other) => {
                        return Err(Diagnostic::new(format!(
                            "`{}` expects `str` for `text`, found `{}`",
                            name,
                            other.render()
                        )))
                    }
                };
                let path = path.clone();
                let text = text.clone();
                let write_name = name.to_string();
                let outcome = run_blocking_io(
                    move || {
                        if write_name == "fs::write_string" {
                            std::fs::write(path, text)
                        } else {
                            use std::io::Write;
                            std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                                .and_then(|mut file| file.write_all(text.as_bytes()))
                        }
                    },
                    Some(&self.cancellation),
                );
                match outcome {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::write_bytes" | "fs::append_bytes" => {
                let bound = bind_builtin_args(&["path", "bytes"], values)?;
                let path = expect_string_value(&bound[0].value, name)?;
                let bytes = expect_bytes_value(&bound[1].value, name)?;
                let write_name = name.to_string();
                let outcome = run_blocking_io(
                    move || {
                        if write_name == "fs::write_bytes" {
                            std::fs::write(path, bytes)
                        } else {
                            use std::io::Write;
                            std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                                .and_then(|mut file| file.write_all(&bytes))
                        }
                    },
                    Some(&self.cancellation),
                );
                match outcome {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::create_dir" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "fs.create_dir(...)")?;
                match run_blocking_io(
                    move || crate::runtime_value::create_dir_once(path),
                    Some(&self.cancellation),
                ) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::read_dir" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "fs.read_dir(...)")?;
                match run_blocking_io(
                    move || {
                        let mut names = std::fs::read_dir(path)?
                            .filter_map(|entry| entry.ok())
                            .map(|entry| entry.file_name().to_string_lossy().to_string())
                            .collect::<Vec<_>>();
                        names.sort();
                        Ok(names)
                    },
                    Some(&self.cancellation),
                ) {
                    Ok(names) => Ok(result_ok(Value::Vec(VecValue {
                        element_type: Type::named("str"),
                        elements: names.into_iter().map(Value::String).collect(),
                    }))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::remove_file" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "fs.remove_file(...)")?;
                match run_blocking_io(
                    move || crate::runtime_value::remove_file_checked(path),
                    Some(&self.cancellation),
                ) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "fs::open" | "fs::create" | "fs::append" => {
                let bound = bind_builtin_args(&["path"], values)?;
                match &bound[0].value {
                    Value::String(path) => {
                        let opened = match name {
                            "fs::open" => FileValue::open(path),
                            "fs::create" => FileValue::create(path),
                            "fs::append" => FileValue::append(path),
                            _ => unreachable!(),
                        };
                        match opened {
                            Ok(file) => Ok(result_ok(Value::File(file))),
                            Err(error) => Ok(result_err(io_error(error))),
                        }
                    }
                    other => Err(Diagnostic::new(format!(
                        "`{}` expects `str`, found `{}`",
                        name,
                        other.render()
                    ))),
                }
            }
            "net::connect" => {
                let bound = bind_builtin_args(&["address"], values)?;
                match &bound[0].value {
                    Value::String(address) => {
                        match TcpStreamValue::connect(address, None, Some(&self.cancellation)) {
                            Ok(stream) => Ok(result_ok(Value::TcpStream(stream))),
                            Err(error) => Ok(result_err(io_error(error))),
                        }
                    }
                    other => Err(Diagnostic::new(format!(
                        "`net.connect(...)` expects `str`, found `{}`",
                        other.render()
                    ))),
                }
            }
            "net::connect_timeout" => {
                let bound = bind_builtin_args(&["address", "timeout"], values)?;
                let address = expect_string_value(&bound[0].value, "net.connect_timeout(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "net.connect_timeout(...)");
                match TcpStreamValue::connect(&address, timeout, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::TcpStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::listen" => {
                let bound = bind_builtin_args(&["address"], values)?;
                match &bound[0].value {
                    Value::String(address) => match TcpListenerValue::bind(address) {
                        Ok(listener) => Ok(result_ok(Value::TcpListener(listener))),
                        Err(error) => Ok(result_err(io_error(error))),
                    },
                    other => Err(Diagnostic::new(format!(
                        "`net.listen(...)` expects `str`, found `{}`",
                        other.render()
                    ))),
                }
            }
            "net::udp_bind" => {
                let bound = bind_builtin_args(&["address"], values)?;
                let address = expect_string_value(&bound[0].value, "net.udp_bind(...)")?;
                match UdpSocketValue::bind(&address) {
                    Ok(socket) => Ok(result_ok(Value::UdpSocket(socket))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::unix_listen" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "net.unix_listen(...)")?;
                match UnixListenerValue::bind(&path) {
                    Ok(listener) => Ok(result_ok(Value::UnixListener(listener))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::unix_connect" => {
                let bound = bind_builtin_args(&["path"], values)?;
                let path = expect_string_value(&bound[0].value, "net.unix_connect(...)")?;
                match UnixStreamValue::connect(&path, None, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::UnixStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::unix_connect_timeout" => {
                let bound = bind_builtin_args(&["path", "timeout"], values)?;
                let path = expect_string_value(&bound[0].value, "net.unix_connect_timeout(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "net.unix_connect_timeout(...)");
                match UnixStreamValue::connect(&path, timeout, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::UnixStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::tls_listen" => {
                let bound =
                    bind_builtin_args(&["address", "cert_pem_path", "key_pem_path"], values)?;
                let address = expect_string_value(&bound[0].value, "net.tls_listen(...)")?;
                let cert = expect_string_value(&bound[1].value, "net.tls_listen(...)")?;
                let key = expect_string_value(&bound[2].value, "net.tls_listen(...)")?;
                match TlsListenerValue::bind(&address, &cert, &key) {
                    Ok(listener) => Ok(result_ok(Value::TlsListener(listener))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::tls_connect" => {
                let bound = bind_builtin_args(&["address", "server_name", "ca_pem_path"], values)?;
                let address = expect_string_value(&bound[0].value, "net.tls_connect(...)")?;
                let server_name = expect_string_value(&bound[1].value, "net.tls_connect(...)")?;
                let ca = expect_string_value(&bound[2].value, "net.tls_connect(...)")?;
                match TlsStreamValue::connect(
                    &address,
                    &server_name,
                    Some(&ca),
                    None,
                    Some(&self.cancellation),
                ) {
                    Ok(stream) => Ok(result_ok(Value::TlsStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::tls_connect_timeout" => {
                let bound = bind_builtin_args(
                    &["address", "server_name", "ca_pem_path", "timeout"],
                    values,
                )?;
                let address = expect_string_value(&bound[0].value, "net.tls_connect_timeout(...)")?;
                let server_name =
                    expect_string_value(&bound[1].value, "net.tls_connect_timeout(...)")?;
                let ca = expect_string_value(&bound[2].value, "net.tls_connect_timeout(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[3].value), "net.tls_connect_timeout(...)");
                match TlsStreamValue::connect(
                    &address,
                    &server_name,
                    Some(&ca),
                    timeout,
                    Some(&self.cancellation),
                ) {
                    Ok(stream) => Ok(result_ok(Value::TlsStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::http_listen" => {
                let bound = bind_builtin_args(&["address"], values)?;
                let address = expect_string_value(&bound[0].value, "net.http_listen(...)")?;
                match HttpListenerValue::bind(&address) {
                    Ok(listener) => Ok(result_ok(Value::HttpListener(listener))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::http_request_text" | "net::http_request_text_timeout" => {
                let expected = if name == "net::http_request_text" {
                    &["method", "url", "body", "headers"][..]
                } else {
                    &["method", "url", "body", "headers", "timeout"][..]
                };
                let bound = bind_builtin_args(expected, values)?;
                let method = expect_string_value(&bound[0].value, name)?;
                let url = expect_string_value(&bound[1].value, name)?;
                let body = expect_string_value(&bound[2].value, name)?;
                let headers = expect_headers_map(&bound[3].value, name)?;
                let timeout = if bound.len() == 5 {
                    io_timeout_or_return!(Some(&bound[4].value), name)
                } else {
                    None
                };
                match HttpResponseValue::request_text(
                    &method,
                    &url,
                    &body,
                    headers,
                    timeout,
                    Some(&self.cancellation),
                ) {
                    Ok(response) => Ok(result_ok(Value::HttpResponse(response))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::http_request_bytes" | "net::http_request_bytes_timeout" => {
                let expected = if name == "net::http_request_bytes" {
                    &["method", "url", "bytes", "headers"][..]
                } else {
                    &["method", "url", "bytes", "headers", "timeout"][..]
                };
                let bound = bind_builtin_args(expected, values)?;
                let method = expect_string_value(&bound[0].value, name)?;
                let url = expect_string_value(&bound[1].value, name)?;
                let bytes = expect_bytes_value(&bound[2].value, name)?;
                let headers = expect_headers_map(&bound[3].value, name)?;
                let timeout = if bound.len() == 5 {
                    io_timeout_or_return!(Some(&bound[4].value), name)
                } else {
                    None
                };
                match HttpResponseValue::request_bytes(
                    &method,
                    &url,
                    &bytes,
                    headers,
                    timeout,
                    Some(&self.cancellation),
                ) {
                    Ok(response) => Ok(result_ok(Value::HttpResponse(response))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::websocket_listen" => {
                let bound = bind_builtin_args(&["address"], values)?;
                let address = expect_string_value(&bound[0].value, "net.websocket_listen(...)")?;
                match WebSocketListenerValue::bind(&address) {
                    Ok(listener) => Ok(result_ok(Value::WebSocketListener(listener))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::websocket_connect" => {
                let bound = bind_builtin_args(&["url"], values)?;
                let url = expect_string_value(&bound[0].value, "net.websocket_connect(...)")?;
                match WebSocketValue::connect(&url, None) {
                    Ok(socket) => Ok(result_ok(Value::WebSocket(socket))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "net::websocket_connect_timeout" => {
                let bound = bind_builtin_args(&["url", "timeout"], values)?;
                let url =
                    expect_string_value(&bound[0].value, "net.websocket_connect_timeout(...)")?;
                let timeout = io_timeout_or_return!(
                    Some(&bound[1].value),
                    "net.websocket_connect_timeout(...)"
                );
                match WebSocketValue::connect(&url, timeout) {
                    Ok(socket) => Ok(result_ok(Value::WebSocket(socket))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported builtin I/O call `{}`",
                name
            ))),
        }
    }

    fn await_process_capture_task(&self, task: Option<TaskValue>, label: &str) -> Result<Vec<u8>> {
        let Some(task) = task else {
            return Ok(Vec::new());
        };
        let outcome = task
            .wait_result_with_cancellation(None, Some(&self.cancellation))
            .map_err(timer_error_to_diagnostic)?;
        match outcome {
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
                            .ok_or_else(|| {
                                Diagnostic::new(format!(
                                    "process {} capture returned a non-byte integer",
                                    label
                                ))
                            }),
                        other => Err(Diagnostic::new(format!(
                            "process {} capture returned `{}` inside `list[uint8]`",
                            label,
                            other.render()
                        ))),
                    })
                    .collect()
            }
            TaskWaitStatus::Ready(Ok(other)) => Err(Diagnostic::new(format!(
                "process {} capture returned `{}` instead of `list[uint8]`",
                label,
                other.render()
            ))),
            TaskWaitStatus::Ready(Err(error)) => Err(error),
            TaskWaitStatus::TimedOut => Err(Diagnostic::new(format!(
                "process {} capture timed out unexpectedly",
                label
            ))),
            TaskWaitStatus::Cancelled => Err(Diagnostic::new(format!(
                "process {} capture was cancelled unexpectedly",
                label
            ))),
        }
    }

    fn evaluate_file_method(
        &mut self,
        file: FileValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "read_all" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match file.read_all() {
                    Ok(text) => Ok(result_ok(Value::String(text))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_bytes" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match file.read_bytes() {
                    Ok(bytes) => Ok(result_ok(bytes_vec_value(bytes))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "write_all" => {
                let bound = bind_builtin_args(&["text"], evaluate_named_args(args, env)?)?;
                match &bound[0].value {
                    Value::String(text) => match file.write_all(text) {
                        Ok(()) => Ok(result_ok(Value::Unit)),
                        Err(error) => Ok(result_err(io_error(error))),
                    },
                    other => Err(Diagnostic::new(format!(
                        "`write_all(...)` expects `str`, found `{}`",
                        other.render()
                    ))),
                }
            }
            "write_bytes" => {
                let bound = bind_builtin_args(&["bytes"], evaluate_named_args(args, env)?)?;
                let bytes = expect_bytes_value(&bound[0].value, "write_bytes(...)")?;
                match file.write_bytes(&bytes) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "flush" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match file.flush() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                file.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR file method `{}`",
                field
            ))),
        }
    }

    fn evaluate_process_child_method(
        &mut self,
        child: ProcessChildValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "stdin" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(child
                    .stdin()
                    .map(Value::ProcessPipe)
                    .map(option_some)
                    .unwrap_or_else(option_none))
            }
            "stdout" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(child
                    .stdout()
                    .map(Value::ProcessPipe)
                    .map(option_some)
                    .unwrap_or_else(option_none))
            }
            "stderr" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(child
                    .stderr()
                    .map(Value::ProcessPipe)
                    .map(option_some)
                    .unwrap_or_else(option_none))
            }
            "wait" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    match expect_process_optional_timeout(&bound[0].value, "wait(timeout=...)") {
                        Ok(timeout) => timeout,
                        Err(error) => return Ok(process_wait_failed(error)),
                    };
                Ok(match child.wait(timeout, Some(&self.cancellation)) {
                    ProcessChildWaitStatus::Exited(status) => process_wait_exited(status),
                    ProcessChildWaitStatus::TimedOut => process_wait_timed_out(),
                    ProcessChildWaitStatus::Cancelled => process_wait_cancelled(),
                    ProcessChildWaitStatus::Failed(error) => {
                        process_wait_failed(process_error_from_io(error))
                    }
                })
            }
            "wait_or_none" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = match expect_process_optional_timeout(
                    &bound[0].value,
                    "wait_or_none(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match child.wait_or_none(timeout, Some(&self.cancellation)) {
                    Ok(Some(status)) => Ok(result_ok(option_some(process_exit_status(status)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(error)),
                }
            }
            "wait_ok" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = match expect_process_optional_timeout(
                    &bound[0].value,
                    "wait_ok(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match child.wait_ok(timeout, Some(&self.cancellation)) {
                    Ok(status) => Ok(result_ok(process_exit_status(status))),
                    Err(error) => Ok(result_err(error)),
                }
            }
            "kill" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match child.kill() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "terminate" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match child.terminate() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                child.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR process child method `{}`",
                field
            ))),
        }
    }

    fn evaluate_process_pipe_method(
        &mut self,
        pipe: ProcessPipeValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "read_all" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match pipe.read_all(Some(&self.cancellation)) {
                    Ok(text) => Ok(result_ok(Value::String(text))),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "read_line" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = match expect_process_optional_timeout(
                    &bound[0].value,
                    "read_line(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match pipe.read_line(timeout, Some(&self.cancellation)) {
                    Ok(Some(line)) => Ok(result_ok(option_some(Value::String(line)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "read_bytes" => {
                let bound =
                    bind_builtin_args(&["max_bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let max_bytes = expect_i32_value(&bound[0].value, "read_bytes(...)")?;
                let max_bytes = usize::try_from(max_bytes).map_err(|_| {
                    Diagnostic::new("`read_bytes(...)` expects a non-negative `max_bytes`")
                })?;
                let timeout = match expect_process_optional_timeout(
                    &bound[1].value,
                    "read_bytes(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match pipe.read_bytes(max_bytes, timeout, Some(&self.cancellation)) {
                    Ok(Some(bytes)) => Ok(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "write_all" => {
                let bound =
                    bind_builtin_args(&["text", "timeout"], evaluate_named_args(args, env)?)?;
                let text = expect_string_value(&bound[0].value, "write_all(...)")?;
                let timeout = match expect_process_optional_timeout(
                    &bound[1].value,
                    "write_all(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match pipe.write_all(&text, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "write_bytes" => {
                let bound =
                    bind_builtin_args(&["bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let bytes = expect_bytes_value(&bound[0].value, "write_bytes(...)")?;
                let timeout = match expect_process_optional_timeout(
                    &bound[1].value,
                    "write_bytes(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match pipe.write_bytes(&bytes, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "flush" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match pipe.flush() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(process_error_from_io(error))),
                }
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                pipe.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR process pipe method `{}`",
                field
            ))),
        }
    }

    fn evaluate_process_completed_method(
        &mut self,
        completed: ProcessCompletedValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "status" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(completed.status())
            }
            "success" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(Value::Bool(completed.success()))
            }
            "stdout" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                completed
                    .stdout()
                    .map(Value::String)
                    .map_err(|error| Diagnostic::coded("AU4005", error.to_string()))
            }
            "stderr" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                completed
                    .stderr()
                    .map(Value::String)
                    .map_err(|error| Diagnostic::coded("AU4005", error.to_string()))
            }
            "stdout_bytes" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(bytes_vec_value(completed.stdout_bytes()))
            }
            "stderr_bytes" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(bytes_vec_value(completed.stderr_bytes()))
            }
            "check" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match completed.check() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(error)),
                }
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR process completed method `{}`",
                field
            ))),
        }
    }

    fn evaluate_process_supervisor_method(
        &mut self,
        supervisor: ProcessSupervisorValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "start" => {
                let mut bound = bind_optional_builtin_args(
                    &[
                        "name",
                        "command",
                        "cwd",
                        "env",
                        "stdin",
                        "stdout",
                        "stderr",
                        "restart",
                        "backoff",
                        "max_restarts",
                        "group",
                    ],
                    evaluate_named_args(args, env)?,
                )?;
                let name = expect_owned_string_value(
                    bound[0]
                        .take()
                        .ok_or_else(|| {
                            Diagnostic::new("missing MIR argument `name` for `start(...)`")
                        })?
                        .value,
                    "start(...)",
                )?;
                let command = expect_owned_command_vec(
                    bound[1]
                        .take()
                        .ok_or_else(|| {
                            Diagnostic::new("missing MIR argument `command` for `start(...)`")
                        })?
                        .value,
                    "start(...)",
                )?;
                let cwd = bound[2]
                    .take()
                    .map(|argument| {
                        expect_owned_optional_string_value(argument.value, "start(...)")
                    })
                    .transpose()?
                    .flatten();
                let env = match bound[3].take() {
                    Some(argument) => expect_owned_headers_map(argument.value, "start(...)")?,
                    None => Vec::new(),
                };
                let stdin = match bound[4].take() {
                    Some(argument) => decode_process_stdio(&argument.value, "start(...)")?,
                    None => crate::runtime_value::ProcessStdioConfig::Null,
                };
                let stdout = match bound[5].take() {
                    Some(argument) => decode_process_stdio(&argument.value, "start(...)")?,
                    None => crate::runtime_value::ProcessStdioConfig::Inherit,
                };
                let stderr = match bound[6].take() {
                    Some(argument) => decode_process_stdio(&argument.value, "start(...)")?,
                    None => crate::runtime_value::ProcessStdioConfig::Inherit,
                };
                let restart = match bound[7].take() {
                    Some(argument) => decode_process_restart_policy(&argument.value, "start(...)")?,
                    None => ProcessRestartPolicy::OnFailure,
                };
                let backoff = match bound[8].take() {
                    Some(argument) => match expect_duration_value(&argument.value, "start(...)") {
                        Ok(backoff) => backoff,
                        Err(error) => return Ok(result_err(error)),
                    },
                    None => StdDuration::from_millis(100),
                };
                let max_restarts = match bound[9].take() {
                    Some(argument) => {
                        expect_supervisor_max_restarts(&argument.value, "start(...)")?
                    }
                    None => None,
                };
                let group = match bound[10].take() {
                    Some(argument) => expect_bool_value(&argument.value, "start(...)")?,
                    None => true,
                };
                match supervisor.start(
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
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(error)),
                }
            }
            "wait" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    match expect_process_optional_timeout(&bound[0].value, "wait(timeout=...)") {
                        Ok(timeout) => timeout,
                        Err(error) => {
                            return Ok(process_supervisor_wait_event(
                                process_supervisor_event_failed(
                                    "<supervisor>".to_string(),
                                    error,
                                    IntegerValue::from_signed(0),
                                ),
                            ))
                        }
                    };
                Ok(match supervisor.wait(timeout, Some(&self.cancellation)) {
                    ProcessSupervisorWaitStatus::Event(event) => {
                        process_supervisor_wait_event(event)
                    }
                    ProcessSupervisorWaitStatus::TimedOut => process_supervisor_wait_timed_out(),
                    ProcessSupervisorWaitStatus::Cancelled => process_supervisor_wait_cancelled(),
                })
            }
            "wait_or_none" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = match expect_process_optional_timeout(
                    &bound[0].value,
                    "wait_or_none(timeout=...)",
                ) {
                    Ok(timeout) => timeout,
                    Err(error) => return Ok(result_err(error)),
                };
                match supervisor.wait_or_none(timeout, Some(&self.cancellation)) {
                    Ok(Some(event)) => Ok(result_ok(option_some(event))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(error)),
                }
            }
            "stop" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match supervisor.stop() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(error)),
                }
            }
            "is_empty" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(Value::Bool(supervisor.is_empty()))
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                supervisor.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR process supervisor method `{}`",
                field
            ))),
        }
    }

    fn evaluate_tcp_listener_method(
        &mut self,
        listener: TcpListenerValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "accept" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(
                    bound.first().map(|argument| &argument.value),
                    "accept(timeout=...)"
                );
                match listener.accept(timeout, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::TcpStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match listener.local_addr() {
                    Ok(address) => Ok(result_ok(Value::String(address))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                listener.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR tcp listener method `{}`",
                field
            ))),
        }
    }

    fn evaluate_tcp_stream_method(
        &mut self,
        stream: TcpStreamValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "read_all" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(
                    bound.first().map(|argument| &argument.value),
                    "read_all(timeout=...)"
                );
                match stream.read_all(timeout, Some(&self.cancellation)) {
                    Ok(text) => Ok(result_ok(Value::String(text))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_line" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(
                    bound.first().map(|argument| &argument.value),
                    "read_line(timeout=...)"
                );
                match stream.read_line(timeout, Some(&self.cancellation)) {
                    Ok(Some(line)) => Ok(result_ok(option_some(Value::String(line)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_bytes" => {
                let bound =
                    bind_builtin_args(&["max_bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let max_bytes =
                    usize::try_from(expect_i32_value(&bound[0].value, "read_bytes(...)")?)
                        .map_err(|_| {
                            Diagnostic::new("`read_bytes(...)` requires a non-negative max_bytes")
                        })?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "read_bytes(timeout=...)");
                match stream.read_bytes(max_bytes, timeout, Some(&self.cancellation)) {
                    Ok(Some(bytes)) => Ok(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_exact" => {
                let bound =
                    bind_builtin_args(&["count", "timeout"], evaluate_named_args(args, env)?)?;
                let count = usize::try_from(expect_i32_value(&bound[0].value, "read_exact(...)")?)
                    .map_err(|_| {
                        Diagnostic::new("`read_exact(...)` requires a non-negative count")
                    })?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "read_exact(timeout=...)");
                match stream.read_exact(count, timeout, Some(&self.cancellation)) {
                    Ok(bytes) => Ok(result_ok(bytes_vec_value(bytes))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "write_all" => {
                let bound =
                    bind_builtin_args(&["text", "timeout"], evaluate_named_args(args, env)?)?;
                match &bound[0].value {
                    Value::String(text) => {
                        let timeout =
                            io_timeout_or_return!(Some(&bound[1].value), "write_all(timeout=...)");
                        match stream.write_all(text, timeout, Some(&self.cancellation)) {
                            Ok(()) => Ok(result_ok(Value::Unit)),
                            Err(error) => Ok(result_err(io_error(error))),
                        }
                    }
                    other => Err(Diagnostic::new(format!(
                        "`write_all(...)` expects `str`, found `{}`",
                        other.render()
                    ))),
                }
            }
            "write_bytes" => {
                let bound =
                    bind_builtin_args(&["bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let bytes = expect_bytes_value(&bound[0].value, "write_bytes(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "write_bytes(timeout=...)");
                match stream.write_bytes(&bytes, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "flush" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.flush() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.local_addr() {
                    Ok(address) => Ok(result_ok(Value::String(address))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "peer_addr" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.peer_addr() {
                    Ok(address) => Ok(result_ok(Value::String(address))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "shutdown_read" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.shutdown_read() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "shutdown_write" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.shutdown_write() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "shutdown_both" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match stream.shutdown_both() {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                stream.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR tcp stream method `{}`",
                field
            ))),
        }
    }

    fn evaluate_udp_socket_method(
        &mut self,
        socket: UdpSocketValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "send_text" => {
                let bound = bind_builtin_args(
                    &["address", "text", "timeout"],
                    evaluate_named_args(args, env)?,
                )?;
                let address = expect_string_value(&bound[0].value, "send_text(...)")?;
                let text = expect_string_value(&bound[1].value, "send_text(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[2].value), "send_text(timeout=...)");
                match socket.send_to_text(&address, &text, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "send_bytes" => {
                let bound = bind_builtin_args(
                    &["address", "bytes", "timeout"],
                    evaluate_named_args(args, env)?,
                )?;
                let address = expect_string_value(&bound[0].value, "send_bytes(...)")?;
                let bytes = expect_bytes_value(&bound[1].value, "send_bytes(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[2].value), "send_bytes(timeout=...)");
                match socket.send_to_bytes(&address, &bytes, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "recv" => {
                let bound =
                    bind_builtin_args(&["max_bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let max_bytes = usize::try_from(expect_i32_value(&bound[0].value, "recv(...)")?)
                    .map_err(|_| {
                        Diagnostic::new("`recv(...)` requires a non-negative max_bytes")
                    })?;
                let timeout = io_timeout_or_return!(Some(&bound[1].value), "recv(timeout=...)");
                match socket.recv(max_bytes, timeout, Some(&self.cancellation)) {
                    Ok(Some(bytes)) => Ok(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "recv_from" => {
                let bound =
                    bind_builtin_args(&["max_bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let max_bytes =
                    usize::try_from(expect_i32_value(&bound[0].value, "recv_from(...)")?).map_err(
                        |_| Diagnostic::new("`recv_from(...)` requires a non-negative max_bytes"),
                    )?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "recv_from(timeout=...)");
                match socket.recv_from(max_bytes, timeout, Some(&self.cancellation)) {
                    Ok(Some(datagram)) => Ok(result_ok(option_some(Value::UdpDatagram(datagram)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => match socket.local_addr() {
                Ok(address) => Ok(result_ok(Value::String(address))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            "peer_addr" => match socket.peer_addr() {
                Ok(address) => Ok(result_ok(Value::String(address))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            "close" => {
                socket.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR udp socket method `{}`",
                field
            ))),
        }
    }

    fn evaluate_udp_datagram_method(
        &mut self,
        datagram: UdpDatagramValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
        match field {
            "address" => Ok(Value::String(datagram.address())),
            "bytes" => Ok(bytes_vec_value(datagram.bytes())),
            "text" => match datagram.text() {
                Ok(text) => Ok(result_ok(Value::String(text))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR udp datagram method `{}`",
                field
            ))),
        }
    }

    fn evaluate_http_listener_method(
        &mut self,
        listener: HttpListenerValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "accept" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(Some(&bound[0].value), "accept(timeout=...)");
                match listener.accept(timeout, Some(&self.cancellation)) {
                    Ok(exchange) => Ok(result_ok(Value::HttpExchange(exchange))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => match listener.local_addr() {
                Ok(address) => Ok(result_ok(Value::String(address))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            "close" => {
                listener.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR http listener method `{}`",
                field
            ))),
        }
    }

    fn evaluate_http_exchange_method(
        &mut self,
        exchange: HttpExchangeValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "method" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(Value::String(exchange.method()))
            }
            "path" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(Value::String(exchange.path()))
            }
            "headers" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(headers_map_value(exchange.headers()))
            }
            "body_text" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                match exchange.body_text() {
                    Ok(text) => Ok(result_ok(Value::String(text))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "body_bytes" => {
                bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
                Ok(bytes_vec_value(exchange.body_bytes()))
            }
            "respond_text" => {
                let bound = bind_builtin_args(
                    &["status", "text", "headers"],
                    evaluate_named_args(args, env)?,
                )?;
                let mut bound = bound.into_iter();
                let status_arg = bound.next().expect("HTTP status argument should exist");
                let status = expect_i32_value(&status_arg.value, "respond_text(...)")?;
                let text = expect_owned_string_value(
                    bound.next().expect("HTTP text argument should exist").value,
                    "respond_text(...)",
                )?;
                let headers = expect_owned_headers_map(
                    bound
                        .next()
                        .expect("HTTP headers argument should exist")
                        .value,
                    "respond_text(...)",
                )?;
                match exchange.respond_text(status, &text, headers) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "respond_bytes" => {
                let bound = bind_builtin_args(
                    &["status", "bytes", "headers"],
                    evaluate_named_args(args, env)?,
                )?;
                let mut bound = bound.into_iter();
                let status_arg = bound.next().expect("HTTP status argument should exist");
                let status = expect_i32_value(&status_arg.value, "respond_bytes(...)")?;
                let bytes = expect_owned_bytes_value(
                    bound
                        .next()
                        .expect("HTTP bytes argument should exist")
                        .value,
                    "respond_bytes(...)",
                )?;
                let headers = expect_owned_headers_map(
                    bound
                        .next()
                        .expect("HTTP headers argument should exist")
                        .value,
                    "respond_bytes(...)",
                )?;
                match exchange.respond_bytes(status, &bytes, headers) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR http exchange method `{}`",
                field
            ))),
        }
    }

    fn evaluate_http_response_method(
        &mut self,
        response: HttpResponseValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        bind_builtin_args(&[], evaluate_named_args(args, env)?)?;
        match field {
            "status" => Ok(Value::Int(IntegerValue::from_signed(
                response.status() as i128
            ))),
            "reason" => Ok(Value::String(response.reason())),
            "headers" => Ok(headers_map_value(response.headers())),
            "text" => match response.text() {
                Ok(text) => Ok(result_ok(Value::String(text))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            "bytes" => Ok(bytes_vec_value(response.bytes())),
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR http response method `{}`",
                field
            ))),
        }
    }

    fn evaluate_websocket_listener_method(
        &mut self,
        listener: WebSocketListenerValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "accept" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(Some(&bound[0].value), "accept(timeout=...)");
                match listener.accept(timeout) {
                    Ok(socket) => Ok(result_ok(Value::WebSocket(socket))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => match listener.local_addr() {
                Ok(address) => Ok(result_ok(Value::String(address))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR websocket listener method `{}`",
                field
            ))),
        }
    }

    fn evaluate_websocket_method(
        &mut self,
        socket: WebSocketValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "send_text" => {
                let bound =
                    bind_builtin_args(&["text", "timeout"], evaluate_named_args(args, env)?)?;
                let text = expect_string_value(&bound[0].value, "send_text(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "send_text(timeout=...)");
                match socket.send_text(&text, timeout) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "send_bytes" => {
                let bound =
                    bind_builtin_args(&["bytes", "timeout"], evaluate_named_args(args, env)?)?;
                let bytes = expect_bytes_value(&bound[0].value, "send_bytes(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "send_bytes(timeout=...)");
                match socket.send_bytes(&bytes, timeout) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "recv_text" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[0].value), "recv_text(timeout=...)");
                match socket.recv_text(timeout) {
                    Ok(Some(text)) => Ok(result_ok(option_some(Value::String(text)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "recv_bytes" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[0].value), "recv_bytes(timeout=...)");
                match socket.recv_bytes(timeout) {
                    Ok(Some(bytes)) => Ok(result_ok(option_some(bytes_vec_value(bytes)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                let _ = socket.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR websocket method `{}`",
                field
            ))),
        }
    }

    fn evaluate_unix_listener_method(
        &mut self,
        listener: UnixListenerValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "accept" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(Some(&bound[0].value), "accept(timeout=...)");
                match listener.accept(timeout, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::UnixStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                listener.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR unix listener method `{}`",
                field
            ))),
        }
    }

    fn evaluate_unix_stream_method(
        &mut self,
        stream: UnixStreamValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "read_line" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[0].value), "read_line(timeout=...)");
                match stream.read_line(timeout, Some(&self.cancellation)) {
                    Ok(Some(text)) => Ok(result_ok(option_some(Value::String(text)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_exact" => {
                let bound =
                    bind_builtin_args(&["count", "timeout"], evaluate_named_args(args, env)?)?;
                let count = usize::try_from(expect_i32_value(&bound[0].value, "read_exact(...)")?)
                    .map_err(|_| {
                        Diagnostic::new("`read_exact(...)` requires a non-negative count")
                    })?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "read_exact(timeout=...)");
                match stream.read_exact(count, timeout, Some(&self.cancellation)) {
                    Ok(bytes) => Ok(result_ok(bytes_vec_value(bytes))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "write_all" => {
                let bound =
                    bind_builtin_args(&["text", "timeout"], evaluate_named_args(args, env)?)?;
                let text = expect_string_value(&bound[0].value, "write_all(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "write_all(timeout=...)");
                match stream.write_all(&text, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                stream.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR unix stream method `{}`",
                field
            ))),
        }
    }

    fn evaluate_tls_listener_method(
        &mut self,
        listener: TlsListenerValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "accept" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout = io_timeout_or_return!(Some(&bound[0].value), "accept(timeout=...)");
                match listener.accept(timeout, Some(&self.cancellation)) {
                    Ok(stream) => Ok(result_ok(Value::TlsStream(stream))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "local_addr" => match listener.local_addr() {
                Ok(address) => Ok(result_ok(Value::String(address))),
                Err(error) => Ok(result_err(io_error(error))),
            },
            "close" => {
                listener.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR tls listener method `{}`",
                field
            ))),
        }
    }

    fn evaluate_tls_stream_method(
        &mut self,
        stream: TlsStreamValue,
        field: &str,
        args: &[MirArg],
        env: &mut Env,
    ) -> Result<Value> {
        match field {
            "read_line" => {
                let bound = bind_builtin_args(&["timeout"], evaluate_named_args(args, env)?)?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[0].value), "read_line(timeout=...)");
                match stream.read_line(timeout, Some(&self.cancellation)) {
                    Ok(Some(text)) => Ok(result_ok(option_some(Value::String(text)))),
                    Ok(None) => Ok(result_ok(option_none())),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "read_exact" => {
                let bound =
                    bind_builtin_args(&["count", "timeout"], evaluate_named_args(args, env)?)?;
                let count = usize::try_from(expect_i32_value(&bound[0].value, "read_exact(...)")?)
                    .map_err(|_| {
                        Diagnostic::new("`read_exact(...)` requires a non-negative count")
                    })?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "read_exact(timeout=...)");
                match stream.read_exact(count, timeout, Some(&self.cancellation)) {
                    Ok(bytes) => Ok(result_ok(bytes_vec_value(bytes))),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "write_all" => {
                let bound =
                    bind_builtin_args(&["text", "timeout"], evaluate_named_args(args, env)?)?;
                let text = expect_string_value(&bound[0].value, "write_all(...)")?;
                let timeout =
                    io_timeout_or_return!(Some(&bound[1].value), "write_all(timeout=...)");
                match stream.write_all(&text, timeout, Some(&self.cancellation)) {
                    Ok(()) => Ok(result_ok(Value::Unit)),
                    Err(error) => Ok(result_err(io_error(error))),
                }
            }
            "close" => {
                stream.close();
                Ok(Value::Unit)
            }
            _ => Err(Diagnostic::new(format!(
                "unsupported MIR tls stream method `{}`",
                field
            ))),
        }
    }

    fn expect_task_list(&self, value: &Value, label: &str) -> Result<Vec<TaskValue>> {
        let Value::Vec(tasks) = value else {
            return Err(Diagnostic::new(format!(
                "`{label}` expects `list[Task[T]]`, found `{}`",
                value.render()
            )));
        };
        let mut resolved = Vec::with_capacity(tasks.elements.len());
        for task in &tasks.elements {
            let Value::Task(task) = task else {
                return Err(Diagnostic::new(format!(
                    "`{label}` expects `list[Task[T]]`, found `{}`",
                    value.render()
                )));
            };
            resolved.push(task.clone());
        }
        Ok(resolved)
    }

    fn join_task(&mut self, task: TaskValue, timeout: Option<StdDuration>) -> Result<Value> {
        task.claim_result_observation()?;
        let outcome = task
            .wait_result_with_cancellation_observed(timeout, Some(&self.cancellation))
            .map_err(timer_error_to_diagnostic)?;
        Ok(match outcome {
            TaskWaitStatus::Ready(result) => match result {
                Ok(value) => task_result_ready(value),
                Err(error) => task_result_error(error.message),
            },
            TaskWaitStatus::TimedOut => task_result_timed_out(),
            TaskWaitStatus::Cancelled => task_result_cancelled(),
        })
    }

    fn wait_any(&mut self, tasks: Vec<TaskValue>, timeout: Option<StdDuration>) -> Result<Value> {
        claim_task_result_observations(&tasks)?;
        if tasks.is_empty() {
            return if poll_cancellation(&self.cancellation) {
                Ok(wait_any_cancelled())
            } else {
                Ok(wait_any_timed_out())
            };
        }
        let deadline = runtime_deadline_after_timeout(timeout)?;
        loop {
            for (index, task) in tasks.iter().enumerate() {
                if let Some(result) = task.completed_result_observed() {
                    // Rust allocations cannot contain more than isize::MAX elements, so
                    // every real list index is representable as Aura's int64.
                    let index = i64::try_from(index).expect("task-list index must fit int64");
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
                Some(&self.cancellation),
            ) {
                RuntimeSchedulerWakeReason::Ready => {}
                RuntimeSchedulerWakeReason::TimedOut => return Ok(wait_any_timed_out()),
                RuntimeSchedulerWakeReason::Cancelled => return Ok(wait_any_cancelled()),
            }
        }
    }

    fn wait_all(&mut self, tasks: Vec<TaskValue>, timeout: Option<StdDuration>) -> Result<Value> {
        claim_task_result_observations(&tasks)?;
        let deadline = runtime_deadline_after_timeout(timeout)?;
        let mut results = Vec::with_capacity(tasks.len());
        for (index, task) in tasks.into_iter().enumerate() {
            let remaining = deadline.and_then(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .or(Some(StdDuration::from_millis(0)))
            });
            let outcome = task
                .wait_result_with_cancellation_observed(remaining, Some(&self.cancellation))
                .map_err(timer_error_to_diagnostic)?;
            match outcome {
                TaskWaitStatus::Ready(result) => match result {
                    Ok(value) => results.push(value),
                    Err(error) => {
                        // Rust allocations cannot contain more than isize::MAX elements, so
                        // every real list index is representable as Aura's int64.
                        let index = i64::try_from(index).expect("task-list index must fit int64");
                        return Ok(wait_all_error(index, error.message));
                    }
                },
                TaskWaitStatus::TimedOut => return Ok(wait_all_timed_out()),
                TaskWaitStatus::Cancelled => return Ok(wait_all_cancelled()),
            }
        }
        Ok(wait_all_ready(results))
    }

    fn close_task_group(
        &mut self,
        group: TaskGroupValue,
        cancel_before_cleanup: bool,
    ) -> Result<()> {
        let tasks = group.drain_tasks();
        let mut cancel_group = cancel_before_cleanup;
        if !cancel_group && task_group_cleanup_should_cancel(&tasks, &self.cancellation) {
            cancel_group = true;
        }
        if cancel_group {
            group.cancel();
        }
        for task in tasks {
            let outcome = task
                .wait_result_with_cancellation(None, Some(&self.cancellation))
                .map_err(timer_error_to_diagnostic)?;
            match outcome {
                TaskWaitStatus::Ready(_result) => {
                    if let Some(error) = task.unobserved_error() {
                        return Err(error);
                    }
                }
                TaskWaitStatus::TimedOut | TaskWaitStatus::Cancelled => {}
            }
        }
        Ok(())
    }

    fn evaluate_operand(&self, operand: &Operand, env: &Env) -> Result<Value> {
        match operand {
            Operand::Place(place) => env.read_place(place),
            Operand::MovePlace(place) => Err(Diagnostic::new(format!(
                "consuming MIR operand `{place}` reached a non-consuming context"
            ))),
            Operand::Function { name, signature } => Ok(mir_function_value(name, signature)),
            Operand::Int(value) => Ok(Value::Int(IntegerValue::from_literal(*value))),
            Operand::Duration(value) => Ok(Value::Duration(*value)),
            Operand::Float(value) => Ok(Value::Float(*value)),
            Operand::Bool(value) => Ok(Value::Bool(*value)),
            Operand::String(value) => Ok(Value::String(value.clone())),
            Operand::Unit => Ok(Value::Unit),
        }
    }

    fn evaluate_owned_operand(&self, operand: &Operand, env: &mut Env) -> Result<Value> {
        match operand {
            Operand::MovePlace(place) => env.take_place(place),
            _ => self.evaluate_operand(operand, env),
        }
    }

    fn eval_array_binary(
        &self,
        op: crate::ast::BinaryOp,
        left: &Value,
        right: &Value,
        span: Option<crate::diag::Span>,
    ) -> Result<Value> {
        use crate::ast::BinaryOp;

        let operation = match op {
            BinaryOp::Add => ArrayBinaryOp::Add,
            BinaryOp::Sub => ArrayBinaryOp::Sub,
            BinaryOp::Mul => ArrayBinaryOp::Mul,
            _ => {
                debug_assert_eq!(op, BinaryOp::Div);
                ArrayBinaryOp::Div
            }
        };
        let result = if let Value::Array(left_array) = left {
            if let Value::Array(right_array) = right {
                left_array.binary(right_array, operation, IntegerArithmeticMode::Checked)
            } else {
                left_array.scalar_binary(right, false, operation, IntegerArithmeticMode::Checked)
            }
        } else {
            checked_mir_array_ref(right).scalar_binary(
                left,
                true,
                operation,
                IntegerArithmeticMode::Checked,
            )
        };
        result
            .map(Value::Array)
            .map_err(|error| with_optional_diagnostic_span(error, span))
    }

    fn eval_binary(
        &self,
        op: crate::ast::BinaryOp,
        left: Value,
        right: Value,
        span: Option<crate::diag::Span>,
    ) -> Result<Value> {
        use crate::ast::BinaryOp;

        let arithmetic_error = |message: &'static str| match span {
            Some(span) => Diagnostic::at(span, message),
            None => Diagnostic::new(message),
        };

        match op {
            BinaryOp::And => match (left, right) {
                (Value::Bool(left), Value::Bool(right)) => Ok(Value::Bool(left && right)),
                _ => Err(Diagnostic::new(
                    "MIR logical operands must both have type `bool`",
                )),
            },
            BinaryOp::Or => match (left, right) {
                (Value::Bool(left), Value::Bool(right)) => Ok(Value::Bool(left || right)),
                _ => Err(Diagnostic::new(
                    "MIR logical operands must both have type `bool`",
                )),
            },
            BinaryOp::Eq => Ok(Value::Bool(left == right)),
            BinaryOp::NotEq => Ok(Value::Bool(left != right)),
            BinaryOp::Add => match (left, right) {
                (Value::Int(left), Value::Int(right)) => match left.checked_add(right) {
                    Some(value) => Ok(Value::Int(value)),
                    None => Err(match span {
                        Some(span) => Diagnostic::at(span, "integer overflow"),
                        None => Diagnostic::new("integer overflow"),
                    }),
                },
                (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left + right)),
                (Value::String(left), Value::String(right)) => {
                    Ok(Value::String(concat_strings_checked(left, &right)?))
                }
                (Value::Duration(left), Value::Duration(right)) => left
                    .checked_add(right)
                    .map(Value::Duration)
                    .ok_or_else(|| arithmetic_error("duration overflow")),
                _ => Err(Diagnostic::new(
                    "MIR binary add requires matching supported operand types",
                )),
            },
            BinaryOp::Sub => match (left, right) {
                (Value::Int(left), Value::Int(right)) => match left.checked_sub(right) {
                    Some(value) => Ok(Value::Int(value)),
                    None => Err(match span {
                        Some(span) => Diagnostic::at(span, "integer overflow"),
                        None => Diagnostic::new("integer overflow"),
                    }),
                },
                (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left - right)),
                (Value::Duration(left), Value::Duration(right)) => left
                    .checked_sub(right)
                    .map(Value::Duration)
                    .ok_or_else(|| arithmetic_error("duration overflow")),
                _ => Err(Diagnostic::new(
                    "MIR binary subtraction requires matching numeric operands",
                )),
            },
            BinaryOp::Mul => match (left, right) {
                (Value::Int(left), Value::Int(right)) => match left.checked_mul(right) {
                    Some(value) => Ok(Value::Int(value)),
                    None => Err(match span {
                        Some(span) => Diagnostic::at(span, "integer overflow"),
                        None => Diagnostic::new("integer overflow"),
                    }),
                },
                (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left * right)),
                (Value::Duration(duration), Value::Int(multiplier))
                | (Value::Int(multiplier), Value::Duration(duration)) => {
                    let Some(multiplier) = duration_int64_scalar(multiplier) else {
                        return Err(arithmetic_error(
                            "Duration multiplication requires an int64 scalar",
                        ));
                    };
                    duration
                        .checked_mul(multiplier)
                        .map(Value::Duration)
                        .ok_or_else(|| arithmetic_error("duration overflow"))
                }
                _ => Err(Diagnostic::new(
                    "MIR binary multiplication requires matching numeric operands",
                )),
            },
            BinaryOp::Div => match (left, right) {
                (Value::Int(_left), Value::Int(right)) if right.is_zero() => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                    left.checked_div(right)
                        .expect("non-zero integer division is total"),
                )),
                (Value::Float(_left), Value::Float(0.0)) => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left / right)),
                _ => Err(Diagnostic::new(
                    "MIR binary division requires matching numeric operands",
                )),
            },
            BinaryOp::FloorDiv => match (left, right) {
                (Value::Int(_), Value::Int(right)) if right.is_zero() => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                    left.checked_floor_div(right)
                        .expect("non-zero matching integer floor division is total"),
                )),
                (Value::Float(_), Value::Float(0.0)) => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Float(left), Value::Float(right)) => {
                    Ok(Value::Float(float_floor_divmod(left, right).0))
                }
                (Value::Duration(_), Value::Int(divisor)) if divisor.is_zero() => {
                    Err(arithmetic_error("division by zero"))
                }
                (Value::Duration(duration), Value::Int(divisor)) => {
                    let Some(divisor) = duration_int64_scalar(divisor) else {
                        return Err(arithmetic_error(
                            "Duration floor division requires an int64 divisor",
                        ));
                    };
                    checked_duration_floor_div(duration, divisor)
                        .map(Value::Duration)
                        .ok_or_else(|| arithmetic_error("duration overflow"))
                }
                _ => Err(Diagnostic::new(MIR_FLOOR_DIVISION_OPERANDS_ERROR)),
            },
            BinaryOp::Mod => match (left, right) {
                (Value::Int(_left), Value::Int(right)) if right.is_zero() => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Int(left), Value::Int(right)) => Ok(Value::Int(
                    left.checked_floor_rem(right)
                        .expect("non-zero integer remainder is total"),
                )),
                (Value::Float(_left), Value::Float(0.0)) => Err(match span {
                    Some(span) => Diagnostic::at(span, "division by zero"),
                    None => Diagnostic::new("division by zero"),
                }),
                (Value::Float(left), Value::Float(right)) => {
                    Ok(Value::Float(float_floor_divmod(left, right).1))
                }
                _ => Err(Diagnostic::new(
                    "MIR binary remainder requires matching numeric operands",
                )),
            },
            BinaryOp::Pow => match (left, right) {
                (Value::Int(left), Value::Int(right)) => left
                    .checked_pow(right)
                    .map(Value::Int)
                    .map_err(|error| integer_power_diagnostic(error, span)),
                (Value::Float(left), Value::Float(right)) => {
                    match float_power(left, right, FloatPowerWidth::Float64) {
                        Ok(value) => Ok(Value::Float(value)),
                        Err(error) => Err(with_optional_diagnostic_span(error, span)),
                    }
                }
                _ => Err(Diagnostic::coded(
                    "AU2003",
                    "MIR power requires matching numeric operands",
                )),
            },
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => match (left, right) {
                (Value::Int(left), Value::Int(right)) => {
                    let result = match op {
                        BinaryOp::BitAnd => left.checked_bitand(right),
                        BinaryOp::BitOr => left.checked_bitor(right),
                        BinaryOp::BitXor => left.checked_bitxor(right),
                        _ => unreachable!(),
                    };
                    let Some(result) = result else {
                        return Err(Diagnostic::coded(
                            "AU2002",
                            "bitwise integer operand types must match",
                        ));
                    };
                    Ok(Value::Int(result))
                }
                _ => Err(Diagnostic::coded(
                    "AU2003",
                    "bitwise operators require matching integer operands",
                )),
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
                        .map_err(|error| integer_shift_diagnostic(error, span))
                }
                _ => Err(Diagnostic::coded(
                    "AU2003",
                    "shift operators require matching integer operands",
                )),
            },
            BinaryOp::Less => eval_ordering(BinaryOp::Less, left, right),
            BinaryOp::LessEq => eval_ordering(BinaryOp::LessEq, left, right),
            BinaryOp::Greater => eval_ordering(BinaryOp::Greater, left, right),
            BinaryOp::GreaterEq => eval_ordering(BinaryOp::GreaterEq, left, right),
        }
    }
}

fn integer_power_diagnostic(error: IntegerPowerError, span: Option<Span>) -> Diagnostic {
    let (code, message) = match error {
        IntegerPowerError::MismatchedKinds => {
            ("AU2002", "integer power operand types must match")
        }
        IntegerPowerError::NegativeExponent => (
            "AU4001",
            "runtime negative integer exponent; use explicit floating operands for fractional power",
        ),
        IntegerPowerError::Overflow => ("AU4002", "integer power overflow"),
    };
    match span {
        Some(span) => Diagnostic::coded_at(code, span, message),
        None => Diagnostic::coded(code, message),
    }
}

fn integer_shift_diagnostic(error: IntegerShiftError, span: Option<Span>) -> Diagnostic {
    let (code, message) = match error {
        IntegerShiftError::MismatchedKinds => {
            ("AU2002", "shift operand types must match".to_string())
        }
        IntegerShiftError::InvalidCount { count, width } => (
            "AU4002",
            format!("integer shift count `{count}` is outside the required range `0..{width}`"),
        ),
        IntegerShiftError::Overflow => ("AU4002", "integer left shift overflow".to_string()),
    };
    match span {
        Some(span) => Diagnostic::coded_at(code, span, message),
        None => Diagnostic::coded(code, message),
    }
}

fn with_optional_diagnostic_span(mut diagnostic: Diagnostic, span: Option<Span>) -> Diagnostic {
    if diagnostic.span.is_none() {
        diagnostic.span = span;
    }
    diagnostic
}

fn runtime_deadline_after_timeout(timeout: Option<StdDuration>) -> Result<Option<Instant>> {
    match timeout {
        Some(timeout) => Instant::now()
            .checked_add(timeout)
            .map(Some)
            .ok_or_else(|| {
                Diagnostic::coded("AU4001", "timeout overflows the MIR runtime deadline range")
            }),
        None => Ok(None),
    }
}

enum BlockOutcome {
    Return(Value),
    Goto(String),
}

fn ffi_type_for_extern_param(param: &MirExternParam) -> Result<FfiType> {
    match (&param.ty, param.passing) {
        (Type::Named(name, args), MirReceiverKind::BorrowMut)
            if name == "list" && args.as_slice() == [Type::named("uint8")] =>
        {
            Ok(FfiType::BytesViewMut)
        }
        (ty, _) => ffi_type_for_extern_result(ty),
    }
}

fn ffi_type_for_extern_result(ty: &Type) -> Result<FfiType> {
    let ffi_type = match ty {
        Type::Unit => FfiType::Unit,
        Type::Named(name, args) if args.is_empty() => match name.as_str() {
            "bool" => FfiType::Bool,
            "int8" => FfiType::I8,
            "int16" => FfiType::I16,
            "int32" => FfiType::I32,
            "int" | "int64" => FfiType::I64,
            "uint8" => FfiType::U8,
            "uint16" => FfiType::U16,
            "uint32" => FfiType::U32,
            "uint64" => FfiType::U64,
            "float32" => FfiType::F32,
            "float64" => FfiType::F64,
            "str" => FfiType::StringView,
            // Every other argument-less nominal admitted by semantic analysis
            // is a declared extern opaque handle.
            _ => FfiType::OpaqueHandle,
        },
        Type::Named(name, args) if name == "list" && args.as_slice() == [Type::named("uint8")] => {
            FfiType::BytesView
        }
        other => {
            return Err(Diagnostic::coded(
                "AU4005",
                format!("unsupported FFI source type `{other}` reached MIR execution"),
            ))
        }
    };
    Ok(ffi_type)
}

fn ffi_value_from_runtime(value: &Value, ty: &Type) -> Result<FfiValue> {
    let mismatch = || {
        Diagnostic::coded(
            "AU4005",
            format!(
                "FFI value for source type `{ty}` has incompatible runtime shape `{}`",
                value.render()
            ),
        )
    };
    match ty {
        Type::Unit => matches!(value, Value::Unit)
            .then_some(FfiValue::Unit)
            .ok_or_else(mismatch),
        Type::Named(name, args) if args.is_empty() => match name.as_str() {
            "bool" => match value {
                Value::Bool(value) => Ok(FfiValue::Bool(*value)),
                _ => Err(mismatch()),
            },
            "int8" => runtime_signed_integer(value)
                .and_then(|value| i8::try_from(value).ok())
                .map(FfiValue::I8)
                .ok_or_else(mismatch),
            "int16" => runtime_signed_integer(value)
                .and_then(|value| i16::try_from(value).ok())
                .map(FfiValue::I16)
                .ok_or_else(mismatch),
            "int32" => runtime_signed_integer(value)
                .and_then(|value| i32::try_from(value).ok())
                .map(FfiValue::I32)
                .ok_or_else(mismatch),
            "int" | "int64" => runtime_signed_integer(value)
                .and_then(|value| i64::try_from(value).ok())
                .map(FfiValue::I64)
                .ok_or_else(mismatch),
            "uint8" => runtime_unsigned_integer(value)
                .and_then(|value| u8::try_from(value).ok())
                .map(FfiValue::U8)
                .ok_or_else(mismatch),
            "uint16" => runtime_unsigned_integer(value)
                .and_then(|value| u16::try_from(value).ok())
                .map(FfiValue::U16)
                .ok_or_else(mismatch),
            "uint32" => runtime_unsigned_integer(value)
                .and_then(|value| u32::try_from(value).ok())
                .map(FfiValue::U32)
                .ok_or_else(mismatch),
            "uint64" => runtime_unsigned_integer(value)
                .and_then(|value| u64::try_from(value).ok())
                .map(FfiValue::U64)
                .ok_or_else(mismatch),
            "float32" => match value {
                Value::Float(value) => Ok(FfiValue::F32(*value as f32)),
                _ => Err(mismatch()),
            },
            "float64" => match value {
                Value::Float(value) => Ok(FfiValue::F64(*value)),
                _ => Err(mismatch()),
            },
            "str" => match value {
                Value::String(value) => Ok(FfiValue::String(value.clone())),
                _ => Err(mismatch()),
            },
            _ => opaque_handle_from_runtime(value).ok_or_else(mismatch),
        },
        Type::Named(name, args) if name == "list" && args.as_slice() == [Type::named("uint8")] => {
            let Value::Vec(vector) = value else {
                return Err(mismatch());
            };
            let bytes = vector
                .elements
                .iter()
                .map(|element| {
                    runtime_unsigned_integer(element)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(mismatch)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(FfiValue::Bytes(bytes))
        }
        _ => Err(mismatch()),
    }
}

fn runtime_signed_integer(value: &Value) -> Option<i128> {
    let Value::Int(value) = value else {
        return None;
    };
    value.as_i128()
}

fn runtime_unsigned_integer(value: &Value) -> Option<u128> {
    let Value::Int(value) = value else {
        return None;
    };
    match value.representation() {
        IntegerRepresentation::Signed(value) => u128::try_from(value).ok(),
        IntegerRepresentation::Unsigned(value) => Some(value),
    }
}

fn opaque_handle_from_runtime(value: &Value) -> Option<FfiValue> {
    let Value::FfiHandle(handle) = value else {
        return None;
    };
    let handle = crate::ffi::OpaqueHandle::new(handle.as_ptr())?;
    Some(FfiValue::OpaqueHandle(handle))
}

fn bytes_runtime_value(bytes: &[u8]) -> Value {
    Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: bytes
            .iter()
            .copied()
            .map(|value| {
                Value::Int(
                    IntegerValue::from_typed_unsigned(value.into(), IntegerKind::Uint8)
                        .expect("every byte is representable as uint8"),
                )
            })
            .collect(),
    })
}

fn runtime_value_from_ffi(value: FfiValue, ty: &Type) -> Result<Value> {
    let mismatch = |actual: FfiType| {
        Diagnostic::coded(
            "AU4005",
            format!("FFI result `{actual}` does not match source return type `{ty}`"),
        )
    };
    match (value, ty) {
        (FfiValue::Unit, Type::Unit) => Ok(Value::Unit),
        (FfiValue::Bool(value), Type::Named(name, args)) if name == "bool" && args.is_empty() => {
            Ok(Value::Bool(value))
        }
        (FfiValue::I8(value), Type::Named(name, args)) if name == "int8" && args.is_empty() => {
            Ok(Value::Int(
                IntegerValue::from_typed_signed(value.into(), IntegerKind::Int8)
                    .expect("every i8 is representable as int8"),
            ))
        }
        (FfiValue::I16(value), Type::Named(name, args)) if name == "int16" && args.is_empty() => {
            Ok(Value::Int(
                IntegerValue::from_typed_signed(value.into(), IntegerKind::Int16)
                    .expect("every i16 is representable as int16"),
            ))
        }
        (FfiValue::I32(value), Type::Named(name, args)) if name == "int32" && args.is_empty() => {
            Ok(Value::Int(IntegerValue::from_i32(value)))
        }
        (FfiValue::I64(value), Type::Named(name, args))
            if matches!(name.as_str(), "int" | "int64") && args.is_empty() =>
        {
            Ok(Value::Int(IntegerValue::from_i64(value)))
        }
        (FfiValue::U8(value), Type::Named(name, args)) if name == "uint8" && args.is_empty() => {
            Ok(Value::Int(
                IntegerValue::from_typed_unsigned(value.into(), IntegerKind::Uint8)
                    .expect("every u8 is representable as uint8"),
            ))
        }
        (FfiValue::U16(value), Type::Named(name, args)) if name == "uint16" && args.is_empty() => {
            Ok(Value::Int(
                IntegerValue::from_typed_unsigned(value.into(), IntegerKind::Uint16)
                    .expect("every u16 is representable as uint16"),
            ))
        }
        (FfiValue::U32(value), Type::Named(name, args)) if name == "uint32" && args.is_empty() => {
            Ok(Value::Int(
                IntegerValue::from_typed_unsigned(value.into(), IntegerKind::Uint32)
                    .expect("every u32 is representable as uint32"),
            ))
        }
        (FfiValue::U64(value), Type::Named(name, args)) if name == "uint64" && args.is_empty() => {
            Ok(Value::Int(IntegerValue::from_u64(value)))
        }
        (FfiValue::F32(value), Type::Named(name, args)) if name == "float32" && args.is_empty() => {
            Ok(Value::Float(f64::from(value)))
        }
        (FfiValue::F64(value), Type::Named(name, args)) if name == "float64" && args.is_empty() => {
            Ok(Value::Float(value))
        }
        (FfiValue::OpaqueHandle(handle), Type::Named(class_name, args)) if args.is_empty() => {
            FfiHandleValue::new(class_name.clone(), handle.as_ptr())
                .map(Value::FfiHandle)
                .ok_or_else(|| {
                    Diagnostic::coded("AU4005", "FFI function returned a null opaque handle")
                })
        }
        (value, _) => Err(mismatch(value.ffi_type())),
    }
}

fn ffi_runtime_diagnostic(symbol: &str, error: FfiError) -> Diagnostic {
    let code = if matches!(error, FfiError::NonCanonicalBoolReturn(_)) {
        "AU4001"
    } else {
        "AU4005"
    };
    Diagnostic::coded(code, format!("FFI call to `{symbol}` failed: {error}"))
}

fn array_shape_from_runtime(value: &Value) -> Result<Box<[usize]>> {
    let vector = checked_mir_vec_ref(value);
    debug_assert_eq!(vector.element_type, Type::named("int64"));
    vector
        .elements
        .iter()
        .enumerate()
        .map(|(axis, value)| {
            let value = checked_mir_integer_ref(value);
            debug_assert!(matches!(
                value.runtime_kind(),
                None | Some(IntegerKind::Int64)
            ));
            let dimension = value
                .as_i128()
                .expect("int64 runtime values always fit i128");
            usize::try_from(dimension).map_err(|_| {
                Diagnostic::coded(
                    "AU4007",
                    format!("Array shape axis {axis} cannot be negative, found {dimension}"),
                )
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(Vec::into_boxed_slice)
}

fn array_coordinates_from_runtime(value: &Value) -> Result<Box<[i64]>> {
    fn coordinate(value: &Value) -> i64 {
        let value = checked_mir_integer_ref(value);
        debug_assert!(matches!(
            value.runtime_kind(),
            None | Some(IntegerKind::Int64)
        ));
        i64::try_from(
            value
                .as_i128()
                .expect("int64 runtime values always fit i128"),
        )
        .expect("semantic analysis validates int64 coordinate literal bounds")
    }

    if matches!(value, Value::Int(_)) {
        return Ok(vec![coordinate(value)].into_boxed_slice());
    }
    let elements = if let Value::Vec(vector) = value {
        debug_assert_eq!(vector.element_type, Type::named("int64"));
        vector.elements.as_slice()
    } else {
        let tuple = checked_mir_tuple_ref(value);
        debug_assert_eq!(tuple.element_types.len(), tuple.elements.len());
        debug_assert!(tuple
            .element_types
            .iter()
            .all(|ty| *ty == Type::named("int64")));
        tuple.elements.as_slice()
    };
    Ok(elements
        .iter()
        .map(coordinate)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}

fn evaluate_named_args(args: &[MirArg], env: &mut Env) -> Result<Vec<EvaluatedMirArg>> {
    args.iter()
        .map(|arg| {
            let ty = match &arg.value {
                Operand::Place(place) | Operand::MovePlace(place) => env.place_type(place).cloned(),
                Operand::Function { signature, .. } => Some(signature.as_ref().clone()),
                Operand::Int(_) => Some(Type::named("int64")),
                Operand::Duration(_) => Some(Type::named("Duration")),
                Operand::Float(_) => Some(Type::named("float64")),
                Operand::Bool(_) => Some(Type::named("bool")),
                Operand::String(_) => Some(Type::named("str")),
                Operand::Unit => Some(Type::Unit),
            };
            let value = match &arg.value {
                Operand::Place(place) => env.read_place(place)?,
                Operand::MovePlace(place) => env.take_place(place)?,
                Operand::Function { name, signature } => mir_function_value(name, signature),
                Operand::Int(value) => Value::Int(IntegerValue::from_literal(*value)),
                Operand::Duration(value) => Value::Duration(*value),
                Operand::Float(value) => Value::Float(*value),
                Operand::Bool(value) => Value::Bool(*value),
                Operand::String(value) => Value::String(value.clone()),
                Operand::Unit => Value::Unit,
            };
            Ok(EvaluatedMirArg {
                name: arg.name.clone(),
                value,
                ty,
                writeback_place: arg.writeback_place.clone(),
            })
        })
        .collect()
}

#[inline(never)]
fn evaluate_round_builtin(args: &[MirArg], env: &mut Env) -> Result<Value> {
    let values = evaluate_named_args(args, env)?;
    let bound = bind_builtin_args(&["value"], values)?;
    round_numeric_value(&bound[0].value)
}

#[inline(never)]
fn evaluate_divmod_builtin(args: &[MirArg], env: &mut Env) -> Result<Value> {
    let values = evaluate_named_args(args, env)?;
    let bound = bind_builtin_args(&["left", "right"], values)?;
    let operand_type = match bound[0].ty.clone() {
        Some(ty) => ty,
        None => match &bound[0].value {
            Value::Int(value) => Type::named(value.runtime_type_name().unwrap_or("int64")),
            Value::Float(_) => Type::named("float64"),
            _ => Type::named("Unknown"),
        },
    };
    divmod_numeric_values(&bound[0].value, &bound[1].value, &operand_type)
}

fn validate_mir_select_sources(args: Vec<EvaluatedMirArg>) -> Result<Vec<Value>> {
    let mut queue_payload_type = None;
    let mut task_result_type = None;
    let mut sources = Vec::with_capacity(args.len());

    for (index, argument) in args.into_iter().enumerate() {
        if argument.name.is_some() {
            return Err(Diagnostic::coded(
                "AU4001",
                "`select` expects positional source values in MIR runtime",
            ));
        }

        let ty = argument.ty.ok_or_else(|| {
            Diagnostic::coded(
                "AU4001",
                format!(
                    "`select` MIR source {index} is missing source type metadata; \
                     expected `Queue[T]`, `Task[T]`, or `Duration`"
                ),
            )
        })?;

        match (&ty, &argument.value) {
            (Type::Named(name, args), Value::Channel(_)) if name == "Queue" && args.len() == 1 => {
                let payload_type = &args[0];
                if let Some(expected) = queue_payload_type.as_ref() {
                    if payload_type != expected {
                        return Err(Diagnostic::coded(
                            "AU4001",
                            format!(
                                "`select` MIR sources require a common Queue payload type; \
                                 source {index} uses `{payload_type}`, expected `{expected}`"
                            ),
                        ));
                    }
                } else {
                    queue_payload_type = Some(payload_type.clone());
                }
            }
            (Type::Named(name, args), Value::Task(_)) if name == "Task" && args.len() == 1 => {
                let result_type = &args[0];
                if let Some(expected) = task_result_type.as_ref() {
                    if result_type != expected {
                        return Err(Diagnostic::coded(
                            "AU4001",
                            format!(
                                "`select` MIR sources require a common Task result type; \
                                 source {index} uses `{result_type}`, expected `{expected}`"
                            ),
                        ));
                    }
                } else {
                    task_result_type = Some(result_type.clone());
                }
            }
            (Type::Named(name, args), Value::Duration(_))
                if name == "Duration" && args.is_empty() => {}
            (Type::Named(name, args), _) if name == "Queue" && args.len() != 1 => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} has malformed Queue descriptor `{ty}`; \
                         expected `Queue[T]`"
                    ),
                ));
            }
            (Type::Named(name, args), _) if name == "Task" && args.len() != 1 => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} has malformed Task descriptor `{ty}`; \
                         expected `Task[T]`"
                    ),
                ));
            }
            (Type::Named(name, args), _) if name == "Duration" && !args.is_empty() => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} has malformed Duration descriptor `{ty}`; \
                         expected `Duration`"
                    ),
                ));
            }
            (Type::Named(name, args), _) if name == "Queue" && args.len() == 1 => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} is described as `{ty}` but is not a queue \
                         runtime value"
                    ),
                ));
            }
            (Type::Named(name, args), _) if name == "Task" && args.len() == 1 => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} is described as `{ty}` but is not a task \
                         runtime value"
                    ),
                ));
            }
            (Type::Named(name, args), _) if name == "Duration" && args.is_empty() => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} is described as `Duration` but is not a \
                         duration runtime value"
                    ),
                ));
            }
            _ => {
                return Err(Diagnostic::coded(
                    "AU4001",
                    format!(
                        "`select` MIR source {index} has type `{ty}`; expected `Queue[T]`, \
                         `Task[T]`, or `Duration`"
                    ),
                ));
            }
        }

        sources.push(argument.value);
    }

    Ok(sources)
}

fn bind_optional_function_args(
    params: &[MirParam],
    args: Vec<EvaluatedMirArg>,
) -> Result<Vec<Option<EvaluatedMirArg>>> {
    let names = params
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    let mut values = vec![None; names.len()];
    let mut next_positional = 0usize;
    let mut saw_named = false;
    for argument in args {
        if let Some(name) = argument.name.as_deref() {
            saw_named = true;
            let Some(index) = names.iter().position(|candidate| *candidate == name) else {
                return Err(Diagnostic::new(format!("unknown MIR argument `{name}`")));
            };
            if values[index].is_some() {
                return Err(Diagnostic::new(format!("duplicate MIR argument `{name}`")));
            }
            values[index] = Some(argument);
            continue;
        }
        if saw_named {
            return Err(Diagnostic::new(
                "positional MIR argument cannot follow a named argument",
            ));
        }
        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err(Diagnostic::new("too many MIR arguments"));
        }
        values[next_positional] = Some(argument);
        next_positional += 1;
    }
    Ok(values)
}

#[cfg(test)]
fn bind_args(params: &[MirParam], args: Vec<EvaluatedMirArg>) -> Result<Vec<EvaluatedMirArg>> {
    bind_optional_function_args(params, args)?
        .into_iter()
        .enumerate()
        .map(|(index, argument)| {
            argument.ok_or_else(|| {
                Diagnostic::new(format!(
                    "missing MIR argument `{}`",
                    params
                        .get(index)
                        .map(|param| param.name.as_str())
                        .unwrap_or("<unknown>")
                ))
            })
        })
        .collect()
}

fn bind_function_writeback_places(
    params: &[MirParam],
    args: &[EvaluatedMirArg],
) -> Result<Vec<Option<String>>> {
    let names = params
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    let mut places = vec![None; names.len()];
    let mut occupied = vec![false; names.len()];
    let mut next_positional = 0usize;
    let mut saw_named = false;
    for argument in args {
        let index = if let Some(name) = argument.name.as_deref() {
            saw_named = true;
            names
                .iter()
                .position(|candidate| *candidate == name)
                .ok_or_else(|| Diagnostic::new(format!("unknown MIR argument `{name}`")))?
        } else {
            if saw_named {
                return Err(Diagnostic::new(
                    "positional MIR argument cannot follow a named argument",
                ));
            }
            while next_positional < occupied.len() && occupied[next_positional] {
                next_positional += 1;
            }
            if next_positional >= occupied.len() {
                return Err(Diagnostic::new("too many MIR arguments"));
            }
            let index = next_positional;
            next_positional += 1;
            index
        };
        if occupied[index] {
            let name = argument
                .name
                .as_deref()
                .unwrap_or(names.get(index).copied().unwrap_or("<unknown>"));
            return Err(Diagnostic::new(format!("duplicate MIR argument `{name}`")));
        }
        occupied[index] = true;
        places[index] = argument.writeback_place.clone();
    }
    Ok(places)
}

fn bind_builtin_args(
    expected_names: &[&str],
    args: Vec<EvaluatedMirArg>,
) -> Result<Vec<EvaluatedMirArg>> {
    let mut values = vec![None; expected_names.len()];
    let mut next_positional = 0;

    for argument in args {
        let EvaluatedMirArg {
            name,
            value,
            ty,
            writeback_place,
        } = argument;
        if let Some(name) = name {
            let Some(index) = expected_names
                .iter()
                .position(|candidate| *candidate == name)
            else {
                return Err(Diagnostic::new(format!("unknown MIR argument `{}`", name)));
            };
            values[index] = Some(EvaluatedMirArg {
                name: Some(name),
                value,
                ty,
                writeback_place,
            });
            continue;
        }

        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err(Diagnostic::new("too many MIR arguments"));
        }
        values[next_positional] = Some(EvaluatedMirArg {
            name: None,
            value,
            ty,
            writeback_place,
        });
        next_positional += 1;
    }

    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .or_else(|| {
                    (expected_names.get(index) == Some(&"timeout")).then(|| EvaluatedMirArg {
                        name: Some("timeout".to_string()),
                        value: Value::Unit,
                        ty: Some(Type::Unit),
                        writeback_place: None,
                    })
                })
                .ok_or_else(|| Diagnostic::new("missing MIR argument"))
        })
        .collect()
}

fn bind_optional_builtin_args(
    expected_names: &[&str],
    args: Vec<EvaluatedMirArg>,
) -> Result<Vec<Option<EvaluatedMirArg>>> {
    let mut values = vec![None; expected_names.len()];
    let mut next_positional = 0;

    for argument in args {
        if let Some(name) = argument.name.as_deref() {
            let Some(index) = expected_names
                .iter()
                .position(|candidate| *candidate == name)
            else {
                return Err(Diagnostic::new(format!("unknown MIR argument `{}`", name)));
            };
            values[index] = Some(argument);
            continue;
        }

        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err(Diagnostic::new("too many MIR arguments"));
        }
        values[next_positional] = Some(argument);
        next_positional += 1;
    }

    Ok(values)
}

fn expect_string_value(value: &Value, label: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `str`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_owned_string_value(value: Value, label: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `str`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_command_vec(value: &Value, label: &str) -> Result<Vec<String>> {
    match value {
        Value::Vec(vector) if vector.element_type == Type::named("str") => vector
            .elements
            .iter()
            .map(|element| expect_string_value(element, label))
            .collect(),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `list[str]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_owned_command_vec(value: Value, label: &str) -> Result<Vec<String>> {
    match value {
        Value::Vec(vector) if vector.element_type == Type::named("str") => vector
            .elements
            .into_iter()
            .map(|element| expect_owned_string_value(element, label))
            .collect(),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `list[str]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_bytes_value(value: &Value, label: &str) -> Result<Vec<u8>> {
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
                    unreachable!()
                };
                let byte = value
                    .as_i128()
                    .ok_or_else(|| Diagnostic::new(format!("`{}` expects `list[uint8]`", label)))?;
                let byte = u8::try_from(byte)
                    .map_err(|_| Diagnostic::new(format!("`{}` expects `list[uint8]`", label)))?;
                bytes.push(byte);
            }
            Ok(bytes)
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `list[uint8]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_owned_bytes_value(value: Value, label: &str) -> Result<Vec<u8>> {
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
            for element in vector.elements {
                let Value::Int(value) = element else {
                    unreachable!()
                };
                let byte = value
                    .as_i128()
                    .ok_or_else(|| Diagnostic::new(format!("`{}` expects `list[uint8]`", label)))?;
                let byte = u8::try_from(byte)
                    .map_err(|_| Diagnostic::new(format!("`{}` expects `list[uint8]`", label)))?;
                bytes.push(byte);
            }
            Ok(bytes)
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `list[uint8]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_bool_value(value: &Value, label: &str) -> Result<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `bool`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_optional_string_value(value: &Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::Unit => Ok(None),
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None" =>
        {
            Ok(None)
        }
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "Some" =>
        {
            match variant.payloads.as_slice() {
                [text] => Ok(Some(expect_string_value(text, label)?)),
                _ => Err(Diagnostic::new(format!(
                    "`{}` expects `Option[str]`, found malformed option payload",
                    label
                ))),
            }
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `Option[str]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_owned_optional_string_value(value: Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::Unit => Ok(None),
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None" =>
        {
            Ok(None)
        }
        Value::EnumVariant(mut variant)
            if variant.enum_name == "Option" && variant.variant_name == "Some" =>
        {
            if variant.payloads.len() != 1 {
                return Err(Diagnostic::new(format!(
                    "`{}` expects `Option[str]`, found malformed option payload",
                    label
                )));
            }
            Ok(Some(expect_owned_string_value(
                variant.payloads.pop().expect("one payload remains"),
                label,
            )?))
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `Option[str]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_i32_value(value: &Value, label: &str) -> Result<i32> {
    match value {
        Value::Int(number) => {
            let value = number
                .as_i128()
                .ok_or_else(|| Diagnostic::new(format!("`{}` expects `int32`", label)))?;
            i32::try_from(value)
                .map_err(|_| Diagnostic::new(format!("`{}` expects `int32`", label)))
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `int32`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_i64_value(value: &Value, label: &str) -> Result<i64> {
    match value {
        Value::Int(number) => {
            let value = number
                .as_i128()
                .ok_or_else(|| Diagnostic::new(format!("`{label}` expects `int64`")))?;
            i64::try_from(value).map_err(|_| Diagnostic::new(format!("`{label}` expects `int64`")))
        }
        other => Err(Diagnostic::new(format!(
            "`{label}` expects `int64`, found `{}`",
            other.render()
        ))),
    }
}

fn invalid_random_bounds_diagnostic(lo: i64, hi: i64) -> Diagnostic {
    Diagnostic::coded(
        "AU4003",
        format!("random bounds require `lo < hi`, found `{lo} >= {hi}`"),
    )
}

fn random_resource_error_to_diagnostic(
    error: SecureRandomError,
    bounds: Option<(i64, i64)>,
) -> Diagnostic {
    match error {
        SecureRandomError::InvalidRange => match bounds {
            Some((lo, hi)) => invalid_random_bounds_diagnostic(lo, hi),
            None => Diagnostic::coded("AU4003", "random bounds require `lo < hi`"),
        },
        error @ SecureRandomError::RequestExceedsCeiling { .. } => {
            Diagnostic::coded("AU4005", error.to_string())
        }
        SecureRandomError::Allocation(error) => Diagnostic::coded(
            "AU4005",
            format!("secure random allocation failed: {error}"),
        ),
        SecureRandomError::Entropy(error) => Diagnostic::coded(
            "AU4005",
            format!("operating-system random source failed: {error}"),
        ),
    }
}

fn expect_process_optional_timeout(
    value: &Value,
    label: &str,
) -> std::result::Result<Option<StdDuration>, Value> {
    match value {
        Value::Unit => Ok(None),
        Value::Duration(duration) => duration_to_host_timer(*duration, label)
            .map(Some)
            .map_err(process_error_from_io),
        other => Err(process_error_from_io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` expects `Duration`, found `{}`", label, other.render()),
        ))),
    }
}

fn expect_duration_value(value: &Value, label: &str) -> std::result::Result<StdDuration, Value> {
    match value {
        Value::Duration(duration) => {
            duration_to_host_timer(*duration, label).map_err(process_error_from_io)
        }
        other => Err(process_error_from_io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` expects `Duration`, found `{}`", label, other.render()),
        ))),
    }
}

fn expect_io_optional_timeout(
    value: Option<&Value>,
    label: &str,
) -> std::result::Result<Option<StdDuration>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Unit => Ok(None),
        Value::Duration(duration) => duration_to_host_timer(*duration, label)
            .map(Some)
            .map_err(io_error),
        other => Err(io_error(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` expects `Duration`, found `{}`", label, other.render()),
        ))),
    }
}

fn timer_error_to_diagnostic(error: io::Error) -> Diagnostic {
    Diagnostic::coded("AU4001", error.to_string())
}

fn expect_supervisor_max_restarts(value: &Value, label: &str) -> Result<Option<i32>> {
    let value = expect_i32_value(value, label)?;
    if value < -1 {
        return Err(Diagnostic::new(format!(
            "`{}` expects `max_restarts` to be -1 or greater",
            label
        )));
    }
    Ok((value >= 0).then_some(value))
}

fn expect_optional_timeout(value: Option<&Value>, label: &str) -> Result<Option<StdDuration>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Unit => Ok(None),
        Value::Duration(duration) => duration_to_host_timer(*duration, label)
            .map(Some)
            .map_err(|error| Diagnostic::coded("AU4001", error.to_string())),
        other => Err(Diagnostic::new(format!(
            "`{}` expects `Duration`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn process_error_from_io(error: std::io::Error) -> Value {
    match error.kind() {
        std::io::ErrorKind::TimedOut => process_error_timed_out(),
        std::io::ErrorKind::Interrupted => process_error_cancelled(),
        _ => process_error_io(error),
    }
}

fn expect_headers_map(value: &Value, label: &str) -> Result<Vec<(String, String)>> {
    match value {
        Value::Map(map)
            if (map.key_type == Type::named("str") && map.value_type == Type::named("str"))
                || map.entries.is_empty() =>
        {
            let mut headers = Vec::with_capacity(map.entries.len());
            for (key, value) in &map.entries {
                headers.push((
                    expect_string_value(key, label)?,
                    expect_string_value(value, label)?,
                ));
            }
            Ok(headers)
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `dict[str, str]`, found `{}`",
            label,
            other.render()
        ))),
    }
}

fn expect_owned_headers_map(value: Value, label: &str) -> Result<Vec<(String, String)>> {
    match value {
        Value::Map(map)
            if (map.key_type == Type::named("str") && map.value_type == Type::named("str"))
                || map.entries.is_empty() =>
        {
            let mut headers = Vec::with_capacity(map.entries.len());
            for (key, value) in map.entries {
                headers.push((
                    expect_owned_string_value(key, label)?,
                    expect_owned_string_value(value, label)?,
                ));
            }
            Ok(headers)
        }
        other => Err(Diagnostic::new(format!(
            "`{}` expects `dict[str, str]`, found `{}`",
            label,
            other.render()
        ))),
    }
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

fn collect_runtime_type_substitutions(
    pattern: &Type,
    actual: &Type,
    substitutions: &mut HashMap<String, Type>,
) {
    match pattern {
        Type::TypeParam(name) => {
            substitutions
                .entry(name.clone())
                .or_insert_with(|| actual.clone());
        }
        Type::Named(name, pattern_args) => {
            let Type::Named(actual_name, actual_args) = actual else {
                return;
            };
            if name != actual_name || pattern_args.len() != actual_args.len() {
                return;
            }
            for (pattern_arg, actual_arg) in pattern_args.iter().zip(actual_args.iter()) {
                collect_runtime_type_substitutions(pattern_arg, actual_arg, substitutions);
            }
        }
        Type::Tuple(pattern_elements) => {
            let Type::Tuple(actual_elements) = actual else {
                return;
            };
            if pattern_elements.len() != actual_elements.len() {
                return;
            }
            for (pattern_element, actual_element) in pattern_elements.iter().zip(actual_elements) {
                collect_runtime_type_substitutions(pattern_element, actual_element, substitutions);
            }
        }
        Type::Function {
            params: pattern_params,
            return_type: pattern_return,
            ..
        } => {
            let Type::Function {
                params: actual_params,
                return_type: actual_return,
                ..
            } = actual
            else {
                return;
            };
            if pattern_params.len() != actual_params.len() {
                return;
            }
            for (pattern_param, actual_param) in pattern_params.iter().zip(actual_params.iter()) {
                if pattern_param.passing != actual_param.passing {
                    return;
                }
                collect_runtime_type_substitutions(
                    &pattern_param.ty,
                    &actual_param.ty,
                    substitutions,
                );
            }
            collect_runtime_type_substitutions(pattern_return, actual_return, substitutions);
        }
        Type::Closure {
            params: pattern_params,
            return_type: pattern_return,
            captures: pattern_captures,
            ..
        } => {
            let Type::Closure {
                params: actual_params,
                return_type: actual_return,
                captures: actual_captures,
                ..
            } = actual
            else {
                return;
            };
            if pattern_params.len() != actual_params.len()
                || pattern_captures.len() != actual_captures.len()
            {
                return;
            }
            for (pattern_param, actual_param) in pattern_params.iter().zip(actual_params.iter()) {
                if pattern_param.passing != actual_param.passing {
                    return;
                }
                collect_runtime_type_substitutions(
                    &pattern_param.ty,
                    &actual_param.ty,
                    substitutions,
                );
            }
            for (pattern_capture, actual_capture) in
                pattern_captures.iter().zip(actual_captures.iter())
            {
                collect_runtime_type_substitutions(
                    &pattern_capture.ty,
                    &actual_capture.ty,
                    substitutions,
                );
            }
            collect_runtime_type_substitutions(pattern_return, actual_return, substitutions);
        }
        Type::Unit | Type::Module(_) => {}
    }
}

fn collect_function_signature_substitutions(
    function: &MirFunction,
    concrete_function_type: Option<&Type>,
    substitutions: &mut HashMap<String, Type>,
) {
    let Some(Type::Function {
        params,
        return_type,
    }) = concrete_function_type
    else {
        return;
    };
    if function.params.len() != params.len() {
        return;
    }
    for (declared, concrete) in function.params.iter().zip(params) {
        let concrete_passing = match concrete.passing {
            crate::ast::ReceiverKind::Borrow => MirReceiverKind::Borrow,
            crate::ast::ReceiverKind::BorrowMut => MirReceiverKind::BorrowMut,
            crate::ast::ReceiverKind::Value => MirReceiverKind::Value,
        };
        if declared.passing != concrete_passing {
            return;
        }
        collect_runtime_type_substitutions(&declared.ty, &concrete.ty, substitutions);
    }
    collect_runtime_type_substitutions(&function.return_type, return_type, substitutions);
}

fn public_runtime_function_name(name: &str) -> String {
    name.split_once("::__default_")
        .map_or_else(|| name.to_string(), |(public, _)| public.to_string())
}

fn collect_type_params_from_type(ty: &Type, collected: &mut std::collections::BTreeSet<String>) {
    match ty {
        Type::TypeParam(name) => {
            collected.insert(name.clone());
        }
        Type::Named(_, args) => {
            for arg in args {
                collect_type_params_from_type(arg, collected);
            }
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_type_params_from_type(element, collected);
            }
        }
        Type::Function {
            params,
            return_type,
            ..
        } => {
            for param in params.iter() {
                collect_type_params_from_type(&param.ty, collected);
            }
            collect_type_params_from_type(return_type, collected);
        }
        Type::Closure {
            params,
            return_type,
            captures,
            ..
        } => {
            for param in params.iter() {
                collect_type_params_from_type(&param.ty, collected);
            }
            for capture in captures.iter() {
                collect_type_params_from_type(&capture.ty, collected);
            }
            collect_type_params_from_type(return_type, collected);
        }
        Type::Unit | Type::Module(_) => {}
    }
}

fn build_range(args: Vec<EvaluatedMirArg>) -> Result<Value> {
    let mut start = None;
    let mut stop = None;
    let mut next_positional = 0;

    for argument in args {
        let EvaluatedMirArg { name, value, .. } = argument;
        let Value::Int(value) = value else {
            return Err(Diagnostic::new(
                "`range` requires integer arguments in MIR runtime",
            ));
        };
        match name.as_deref() {
            Some("start") => start = Some(value),
            Some("stop") => stop = Some(value),
            Some(other) => {
                return Err(Diagnostic::new(format!(
                    "unknown MIR `range` argument `{}`",
                    other
                )))
            }
            None => {
                if next_positional == 0 {
                    stop = Some(value);
                } else if next_positional == 1 {
                    start = stop.take();
                    stop = Some(value);
                } else {
                    return Err(Diagnostic::new("`range` takes at most two arguments"));
                }
                next_positional += 1;
            }
        }
    }

    let (start, stop) = match (start, stop) {
        (Some(start), Some(stop)) => (start, stop),
        (None, Some(stop)) => (IntegerValue::zero(), stop),
        _ => return Err(Diagnostic::new("`range` requires `stop` in MIR runtime")),
    };

    Ok(Value::Range(RangeValue {
        start: start.as_i128().ok_or_else(|| {
            Diagnostic::new("`range` start must fit in signed index space in MIR runtime")
        })?,
        end: stop.as_i128().ok_or_else(|| {
            Diagnostic::new("`range` stop must fit in signed index space in MIR runtime")
        })?,
    }))
}

fn eval_ordering(op: crate::ast::BinaryOp, left: Value, right: Value) -> Result<Value> {
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => Ok(Value::Bool(match op {
            crate::ast::BinaryOp::Less => left < right,
            crate::ast::BinaryOp::LessEq => left <= right,
            crate::ast::BinaryOp::Greater => left > right,
            crate::ast::BinaryOp::GreaterEq => left >= right,
            _ => unreachable!("non-ordering op passed to eval_ordering"),
        })),
        (Value::Float(left), Value::Float(right)) => Ok(Value::Bool(match op {
            crate::ast::BinaryOp::Less => left < right,
            crate::ast::BinaryOp::LessEq => left <= right,
            crate::ast::BinaryOp::Greater => left > right,
            crate::ast::BinaryOp::GreaterEq => left >= right,
            _ => unreachable!("non-ordering op passed to eval_ordering"),
        })),
        (Value::Duration(left), Value::Duration(right)) => Ok(Value::Bool(match op {
            crate::ast::BinaryOp::Less => left < right,
            crate::ast::BinaryOp::LessEq => left <= right,
            crate::ast::BinaryOp::Greater => left > right,
            crate::ast::BinaryOp::GreaterEq => left >= right,
            _ => unreachable!("non-ordering op passed to eval_ordering"),
        })),
        _ => Err(Diagnostic::new(
            "MIR ordering comparisons require matching numeric or Duration operands",
        )),
    }
}

fn duration_int64_scalar(value: IntegerValue) -> Option<i128> {
    value
        .as_i128()
        .filter(|value| i64::try_from(*value).is_ok())
}

fn checked_duration_floor_div(dividend: i128, divisor: i128) -> Option<i128> {
    let quotient = dividend.checked_div(divisor)?;
    let remainder = dividend.checked_rem(divisor)?;
    if remainder != 0 && (remainder < 0) != (divisor < 0) {
        quotient.checked_sub(1)
    } else {
        Some(quotient)
    }
}

#[cfg(test)]
#[path = "mir_runtime_tests.rs"]
mod tests;
