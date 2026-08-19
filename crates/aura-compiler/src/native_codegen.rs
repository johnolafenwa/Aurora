use std::collections::{BTreeSet, HashMap, HashSet};

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::immediates::Ieee64;
use cranelift_codegen::ir::TrapCode;
use cranelift_codegen::ir::{
    types, AbiParam, FuncRef, InstBuilder, MemFlags, Signature, StackSlotData, StackSlotKind,
    UserFuncName, Value,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{default_libcall_names, DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ast::{BinaryOp, ReceiverKind, UnaryOp};
use crate::builtin_modules::host_builtin_metadata;
use crate::call::{BuiltinAssociatedFunction, BuiltinMember};
use crate::diag::Span;
use crate::ffi::FfiType;
use crate::mir::{
    BasicBlock, CallTarget, Instruction, MirArg, MirClass, MirExternCall, MirFormatPart,
    MirFunction, MirMapEntry, MirMethod, MirModule, MirReceiverKind, MirTraitImpl, Operand, Rvalue,
    Terminator, NATIVE_LOOP_SAFEPOINT_INTERVAL,
};
use crate::native_runtime::{
    encode_direct_ffi_call_spec, DirectFfiCallSpec, DirectFfiParam, DirectFfiType,
};
use crate::sema::{substitute_type, FunctionParamContract, Type};

const DIRECT_TO_FLOAT_ARITY_ERROR: &str =
    "direct backend expected `to_float()` to take no arguments";
const WIDE_INTEGER_BINARY_ERROR: &str =
    "direct backend does not support wide integer binary operation";
const DIRECT_INTERNAL_SLICE_ARITY_ERROR: &str =
    "direct backend expected internal slicing to receive start, start presence, end, end presence, line, and column";

pub fn emit_host_object(module: &MirModule) -> std::result::Result<Vec<u8>, String> {
    emit_host_object_with_metadata(module, "<aura>", "")
}

pub fn emit_host_object_with_metadata(
    module: &MirModule,
    program_path: &str,
    program_source: &str,
) -> std::result::Result<Vec<u8>, String> {
    let context = NativeCodegen::new(module, program_path, program_source)?;
    context.emit()
}

fn set_native_codegen_flag(
    flag_builder: &mut settings::Builder,
    name: &str,
    value: &str,
) -> std::result::Result<(), String> {
    match flag_builder.set(name, value) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("failed to configure native backend: {error}")),
    }
}

fn native_codegen_flags() -> std::result::Result<settings::Flags, String> {
    let mut flag_builder = settings::builder();
    set_native_codegen_flag(&mut flag_builder, "is_pic", "true")?;
    set_native_codegen_flag(&mut flag_builder, "unwind_info", "true")?;
    // Aura's direct-call ABI is private to one generated object and flattens
    // mutable receiver/parameter writeback into additional result values. On
    // x86-64 that can exceed the two integer return registers. Cranelift must
    // lower the overflow through its implicit return area so every Aura
    // caller and callee keeps the same internal signature.
    set_native_codegen_flag(&mut flag_builder, "enable_multi_ret_implicit_sret", "true")?;
    Ok(settings::Flags::new(flag_builder))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScalarKind {
    Int32,
    Int64,
    Uint64,
    Float32,
    Float64,
    Bool,
    Unit,
}

impl ScalarKind {
    fn signature_type(self) -> cranelift_codegen::ir::Type {
        match self {
            ScalarKind::Int32
            | ScalarKind::Int64
            | ScalarKind::Uint64
            | ScalarKind::Bool
            | ScalarKind::Unit => types::I64,
            ScalarKind::Float32 | ScalarKind::Float64 => types::F64,
        }
    }

    fn zero_value(self, builder: &mut FunctionBuilder<'_>) -> Value {
        match self {
            ScalarKind::Int32
            | ScalarKind::Int64
            | ScalarKind::Uint64
            | ScalarKind::Bool
            | ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
            ScalarKind::Float32 | ScalarKind::Float64 => {
                builder.ins().f64const(Ieee64::with_float(0.0))
            }
        }
    }

    fn is_float(self) -> bool {
        matches!(self, ScalarKind::Float32 | ScalarKind::Float64)
    }

    fn is_integer(self) -> bool {
        matches!(
            self,
            ScalarKind::Int32 | ScalarKind::Int64 | ScalarKind::Uint64
        )
    }
}

fn direct_array_dtype_code(ty: &Type) -> std::result::Result<i64, String> {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "int32" => Ok(0),
            "int64" => Ok(1),
            "float32" => Ok(2),
            "float64" => Ok(3),
            _ => Err(format!(
                "direct backend does not support Array dtype `{ty}`"
            )),
        },
        _ => Err(format!(
            "direct backend does not support Array dtype `{ty}`"
        )),
    }
}

fn direct_array_binary_opcode(op: BinaryOp) -> std::result::Result<i64, String> {
    match op {
        BinaryOp::Add => Ok(0),
        BinaryOp::Sub => Ok(1),
        BinaryOp::Mul => Ok(2),
        BinaryOp::Div => Ok(3),
        _ => Err(format!(
            "direct backend does not support Array binary operation `{op:?}`"
        )),
    }
}

fn direct_array_element_type(ty: &DirectType) -> Option<&Type> {
    match ty {
        DirectType::Opaque(Type::Named(name, arguments))
            if name == "Array" && arguments.len() == 1 =>
        {
            arguments.first()
        }
        _ => None,
    }
}

fn is_fixed_width_integer_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named(name, args)
            if args.is_empty()
                && matches!(
                    name.as_str(),
                    "int8"
                        | "int16"
                        | "int32"
                        | "int64"
                        | "int128"
                        | "intsize"
                        | "uint8"
                        | "uint16"
                        | "uint32"
                        | "uint64"
                        | "uint128"
                        | "uintsize"
                )
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
enum WideIntegerKind {
    Int64 = 0,
    Uint64 = 1,
}

impl WideIntegerKind {
    fn scalar_kind(self) -> ScalarKind {
        match self {
            WideIntegerKind::Int64 => ScalarKind::Int64,
            WideIntegerKind::Uint64 => ScalarKind::Uint64,
        }
    }

    fn is_signed(self) -> bool {
        matches!(self, WideIntegerKind::Int64)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
enum WideOverflowOp {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DirectType {
    Scalar(ScalarKind),
    PlainClass(PlainClassType),
    Opaque(Type),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlainClassType {
    class_name: String,
    fields: Vec<PlainClassField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlainClassField {
    name: String,
    ty: DirectType,
}

impl DirectType {
    fn abi_types(&self) -> Vec<cranelift_codegen::ir::Type> {
        match self {
            DirectType::Scalar(kind) => vec![kind.signature_type()],
            DirectType::PlainClass(class) => {
                let mut types = Vec::new();
                for field in &class.fields {
                    types.extend(field.ty.abi_types());
                }
                types
            }
            DirectType::Opaque(_) => vec![types::I64],
        }
    }

    fn value_count(&self) -> usize {
        self.abi_types().len()
    }

    fn scalar_kind(&self) -> Option<ScalarKind> {
        match self {
            DirectType::Scalar(kind) => Some(*kind),
            DirectType::PlainClass(_) | DirectType::Opaque(_) => None,
        }
    }

    fn zero_values(&self, builder: &mut FunctionBuilder<'_>) -> Vec<Value> {
        match self {
            DirectType::Scalar(kind) => vec![kind.zero_value(builder)],
            DirectType::PlainClass(class) => {
                let mut values = Vec::new();
                for field in &class.fields {
                    values.extend(field.ty.zero_values(builder));
                }
                values
            }
            DirectType::Opaque(_) => vec![builder.ins().iconst(types::I64, 0)],
        }
    }

    fn field_slice(&self, field_name: &str) -> Option<(usize, usize, DirectType)> {
        let DirectType::PlainClass(class) = self else {
            return None;
        };

        let mut start = 0usize;
        for field in &class.fields {
            let end = start + field.ty.value_count();
            if field.name == field_name {
                return Some((start, end, field.ty.clone()));
            }
            start = end;
        }
        None
    }
}

#[derive(Clone)]
struct ValueRef {
    values: Vec<Value>,
    ty: DirectType,
}

#[derive(Clone, Copy)]
struct TaskStartMode {
    returns_handle: bool,
    result_is_copy: bool,
}

struct TaskStart<'a> {
    mode: TaskStartMode,
    stack_size: Option<&'a Operand>,
    task_group: &'a Operand,
    function: &'a Operand,
    args: &'a [MirArg],
    spawn_span: Span,
    target: &'a DirectType,
}

struct BoundFunctionValueArgs<'a> {
    slots: Vec<Option<&'a MirArg>>,
    source_slots: Vec<usize>,
}

fn bind_function_value_args<'a>(
    params: &[FunctionParamContract],
    args: &'a [MirArg],
    unknown_named_prefix: &str,
    duplicate_error: &str,
) -> std::result::Result<BoundFunctionValueArgs<'a>, String> {
    let mut slots = vec![None; params.len()];
    let mut source_slots = Vec::with_capacity(args.len());
    let mut next_positional = 0usize;
    for argument in args {
        let slot = if let Some(name) = argument.name.as_deref() {
            let mut named_slot = None;
            for (index, param) in params.iter().enumerate() {
                if param.name == name {
                    named_slot = Some(index);
                    break;
                }
            }
            let Some(named_slot) = named_slot else {
                return Err(format!("{unknown_named_prefix} `{name}`"));
            };
            named_slot
        } else {
            while next_positional < slots.len() && slots[next_positional].is_some() {
                next_positional += 1;
            }
            let slot = next_positional;
            next_positional += 1;
            slot
        };
        if slot >= slots.len() || slots[slot].replace(argument).is_some() {
            return Err(duplicate_error.to_string());
        }
        source_slots.push(slot);
    }
    Ok(BoundFunctionValueArgs {
        slots,
        source_slots,
    })
}

fn function_value_param_types(
    params: &[FunctionParamContract],
    classes: &HashMap<String, MirClass>,
    context: &str,
) -> std::result::Result<Vec<DirectType>, String> {
    let mut direct_types = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        direct_types.push(ensure_direct_type(
            &param.ty,
            classes,
            &format!("{context} {}", index + 1),
        )?);
    }
    Ok(direct_types)
}

struct NativeCodegen<'a> {
    module: &'a MirModule,
    reachable_blocks: HashMap<String, HashSet<String>>,
    safepoints_enabled: bool,
    program_path: String,
    program_source: String,
    object: ObjectModule,
    functions: HashMap<String, FuncId>,
    function_thunks: HashMap<String, FuncId>,
    function_default_binders: HashMap<String, FuncId>,
    cleanup_thunks: HashMap<(String, String), FuncId>,
    classes: HashMap<String, MirClass>,
    trait_impls: Vec<MirTraitImpl>,
    function_return_types: HashMap<String, DirectType>,
    function_param_types: HashMap<String, Vec<DirectType>>,
    function_writeback_types: HashMap<String, Vec<DirectType>>,
    call_conv: CallConv,
    runtime_init: FuncId,
    run_root: FuncId,
    enter_call: FuncId,
    exit_call: FuncId,
    set_returned_view_projection: FuncId,
    take_returned_view_projection: FuncId,
    print_i64: FuncId,
    print_u64: FuncId,
    print_f32: FuncId,
    print_f64: FuncId,
    print_bool: FuncId,
    print_value: FuncId,
    sqrt_f64: FuncId,
    assert_fail: FuncId,
    assert_fail_detailed: FuncId,
    fail_division_by_zero: FuncId,
    fail_int32_overflow: FuncId,
    fail_integer_overflow: FuncId,
    register_cleanup: FuncId,
    unregister_cleanup: FuncId,
    refresh_cleanup: FuncId,
    set_next_mutable_sinks: FuncId,
    set_next_indirect_mutable_sinks: FuncId,
    current_mutable_sink: FuncId,
    mutable_sink_new: FuncId,
    mutable_sink_project: FuncId,
    mutable_sink_store_owned: FuncId,
    mutable_sink_release: FuncId,
    close_value: FuncId,
    tag_value_type: FuncId,
    box_i32: FuncId,
    box_i64: FuncId,
    box_u64: FuncId,
    box_uint_literal: FuncId,
    box_f64: FuncId,
    box_bool: FuncId,
    function_value: FuncId,
    module_constant: FuncId,
    closure_value: FuncId,
    closure_capture: FuncId,
    function_call: FuncId,
    function_bind_defaults: FuncId,
    box_unit: FuncId,
    string_literal: FuncId,
    string_len: FuncId,
    string_byte_len: FuncId,
    string_slice: FuncId,
    string_contains: FuncId,
    string_starts_with: FuncId,
    string_ends_with: FuncId,
    string_split: FuncId,
    string_replace: FuncId,
    string_to_lower: FuncId,
    string_to_upper: FuncId,
    string_strip_prefix: FuncId,
    string_strip_suffix: FuncId,
    string_trim: FuncId,
    string_join: FuncId,
    stringify_value: FuncId,
    format_value: FuncId,
    abs_value: FuncId,
    min_value: FuncId,
    max_value: FuncId,
    sqrt_value: FuncId,
    round_value: FuncId,
    divmod_value: FuncId,
    parse_int32: FuncId,
    parse_int64: FuncId,
    parse_float64: FuncId,
    duration_literal: FuncId,
    duration_from_i64: FuncId,
    duration_to_float: FuncId,
    rng_new: FuncId,
    rng_next_int: FuncId,
    rng_next_float: FuncId,
    rng_shuffle: FuncId,
    random_secure_int: FuncId,
    random_secure_bytes: FuncId,
    range_new: FuncId,
    range_current: FuncId,
    range_end: FuncId,
    range_advance: FuncId,
    vec_empty: FuncId,
    vec_len: FuncId,
    vec_is_empty: FuncId,
    vec_push_in_place: FuncId,
    vec_pop_in_place: FuncId,
    vec_get: FuncId,
    vec_set_in_place: FuncId,
    vec_remove_in_place: FuncId,
    vec_swap_in_place: FuncId,
    vec_contains: FuncId,
    vec_extend_in_place: FuncId,
    vec_insert_in_place: FuncId,
    vec_clear_in_place: FuncId,
    vec_reverse_in_place: FuncId,
    collection_operation: FuncId,
    vec_index: FuncId,
    vec_slice: FuncId,
    vec_index_option: FuncId,
    vec_take_index_in_place: FuncId,
    vec_set_index_in_place: FuncId,
    array_zeros: FuncId,
    array_full: FuncId,
    array_from_vec: FuncId,
    array_clone: FuncId,
    array_shape: FuncId,
    array_len: FuncId,
    array_get: FuncId,
    array_set_in_place: FuncId,
    array_fill_in_place: FuncId,
    array_index: FuncId,
    array_set_index_in_place: FuncId,
    array_slice: FuncId,
    array_binary: FuncId,
    array_map: FuncId,
    array_reduce: FuncId,
    map_empty: FuncId,
    map_len: FuncId,
    map_is_empty: FuncId,
    map_get: FuncId,
    map_set_in_place: FuncId,
    map_remove_in_place: FuncId,
    map_contains_key: FuncId,
    map_keys: FuncId,
    map_values: FuncId,
    map_items: FuncId,
    map_clear_in_place: FuncId,
    map_extend_in_place: FuncId,
    map_index: FuncId,
    map_set_index_in_place: FuncId,
    set_empty: FuncId,
    set_len: FuncId,
    set_is_empty: FuncId,
    set_contains: FuncId,
    set_insert_in_place: FuncId,
    set_remove_in_place: FuncId,
    set_index_option: FuncId,
    set_take_index_in_place: FuncId,
    retain_value: FuncId,
    release_value: FuncId,
    clone_value: FuncId,
    unbox_i64: FuncId,
    unbox_int64: FuncId,
    integer_to_float: FuncId,
    integer_width_binary: FuncId,
    unbox_u64: FuncId,
    unbox_f64: FuncId,
    unbox_bool: FuncId,
    value_as_condition: FuncId,
    unary_value: FuncId,
    binary_value: FuncId,
    cast_value: FuncId,
    cast_integer_to_integer: FuncId,
    cast_integer_to_float: FuncId,
    cast_float_to_integer: FuncId,
    value_type_matches: FuncId,
    value_has_runtime_type: FuncId,
    tuple_new: FuncId,
    tuple_element: FuncId,
    tuple_take_element: FuncId,
    enum_variant: FuncId,
    variant_matches: FuncId,
    variant_payload: FuncId,
    variant_take_payload: FuncId,
    instance_empty: FuncId,
    instance_get_field: FuncId,
    instance_take_field: FuncId,
    instance_set_field_owned: FuncId,
    arg_buffer_new: FuncId,
    arg_buffer_store: FuncId,
    arg_buffer_store_owned: FuncId,
    task_arg_buffer_guard: FuncId,
    task_arg_buffer_disarm: FuncId,
    host_builtin: FuncId,
    ffi_call: FuncId,
    monotonic_time_ms: FuncId,
    channel_new: FuncId,
    channel_send: FuncId,
    channel_send_timeout_value: FuncId,
    channel_try_send: FuncId,
    channel_recv: FuncId,
    channel_recv_in_task_group: FuncId,
    channel_recv_with_registered_producers: FuncId,
    channel_recv_timeout_value: FuncId,
    channel_recv_or_none: FuncId,
    channel_recv_or_none_timeout_value: FuncId,
    channel_recv_or_value: FuncId,
    channel_recv_or_value_timeout_value: FuncId,
    channel_close: FuncId,
    task_group_new: FuncId,
    task_group_cancel: FuncId,
    task_group_close: FuncId,
    task_join: FuncId,
    task_join_timeout_value: FuncId,
    task_join_or_none: FuncId,
    task_join_or_none_timeout_value: FuncId,
    task_join_or_value: FuncId,
    task_join_or_value_timeout_value: FuncId,
    wait_any: FuncId,
    wait_any_timeout_value: FuncId,
    wait_all: FuncId,
    wait_all_timeout_value: FuncId,
    select: FuncId,
    io_write: FuncId,
    io_flush: FuncId,
    io_read_line: FuncId,
    fs_exists: FuncId,
    fs_read_to_string: FuncId,
    fs_read_bytes: FuncId,
    fs_write_string: FuncId,
    fs_write_bytes: FuncId,
    fs_append_string: FuncId,
    fs_append_bytes: FuncId,
    fs_create_dir: FuncId,
    fs_read_dir: FuncId,
    fs_remove_file: FuncId,
    fs_open: FuncId,
    fs_create: FuncId,
    fs_append: FuncId,
    file_read_all: FuncId,
    file_read_bytes: FuncId,
    file_write_all: FuncId,
    file_write_bytes: FuncId,
    file_flush: FuncId,
    file_close: FuncId,
    process_inherit: FuncId,
    process_null: FuncId,
    process_pipe: FuncId,
    process_supervisor: FuncId,
    process_start: FuncId,
    process_run: FuncId,
    process_child_stdin: FuncId,
    process_child_stdout: FuncId,
    process_child_stderr: FuncId,
    process_child_wait: FuncId,
    process_child_wait_or_none: FuncId,
    process_child_wait_ok: FuncId,
    process_child_kill: FuncId,
    process_child_terminate: FuncId,
    process_child_close: FuncId,
    process_pipe_read_all: FuncId,
    process_pipe_read_line: FuncId,
    process_pipe_read_bytes: FuncId,
    process_pipe_write_all: FuncId,
    process_pipe_write_bytes: FuncId,
    process_pipe_flush: FuncId,
    process_pipe_close: FuncId,
    process_completed_status: FuncId,
    process_completed_success: FuncId,
    process_completed_stdout: FuncId,
    process_completed_stderr: FuncId,
    process_completed_stdout_bytes: FuncId,
    process_completed_stderr_bytes: FuncId,
    process_completed_check: FuncId,
    process_supervisor_start: FuncId,
    process_supervisor_wait: FuncId,
    process_supervisor_wait_or_none: FuncId,
    process_supervisor_stop: FuncId,
    process_supervisor_is_empty: FuncId,
    process_supervisor_close: FuncId,
    net_connect: FuncId,
    net_connect_timeout: FuncId,
    net_listen: FuncId,
    net_udp_bind: FuncId,
    net_unix_listen: FuncId,
    net_unix_connect: FuncId,
    net_unix_connect_timeout: FuncId,
    net_tls_listen: FuncId,
    net_tls_connect: FuncId,
    net_tls_connect_timeout: FuncId,
    net_http_listen: FuncId,
    net_http_request_text: FuncId,
    net_http_request_text_timeout: FuncId,
    net_http_request_bytes: FuncId,
    net_http_request_bytes_timeout: FuncId,
    net_websocket_listen: FuncId,
    net_websocket_connect: FuncId,
    net_websocket_connect_timeout: FuncId,
    tcp_listener_accept: FuncId,
    tcp_listener_local_addr: FuncId,
    tcp_listener_close: FuncId,
    tcp_stream_read_all: FuncId,
    tcp_stream_read_line: FuncId,
    tcp_stream_read_bytes: FuncId,
    tcp_stream_read_exact: FuncId,
    tcp_stream_write_all: FuncId,
    tcp_stream_write_bytes: FuncId,
    tcp_stream_flush: FuncId,
    tcp_stream_local_addr: FuncId,
    tcp_stream_peer_addr: FuncId,
    tcp_stream_shutdown_read: FuncId,
    tcp_stream_shutdown_write: FuncId,
    tcp_stream_shutdown_both: FuncId,
    tcp_stream_close: FuncId,
    udp_socket_send_text: FuncId,
    udp_socket_send_bytes: FuncId,
    udp_socket_recv: FuncId,
    udp_socket_recv_from: FuncId,
    udp_socket_local_addr: FuncId,
    udp_socket_peer_addr: FuncId,
    udp_socket_close: FuncId,
    udp_datagram_address: FuncId,
    udp_datagram_bytes: FuncId,
    udp_datagram_text: FuncId,
    http_listener_accept: FuncId,
    http_listener_local_addr: FuncId,
    http_listener_close: FuncId,
    http_exchange_method: FuncId,
    http_exchange_path: FuncId,
    http_exchange_headers: FuncId,
    http_exchange_body_text: FuncId,
    http_exchange_body_bytes: FuncId,
    http_exchange_respond_text: FuncId,
    http_exchange_respond_bytes: FuncId,
    http_response_status: FuncId,
    http_response_reason: FuncId,
    http_response_headers: FuncId,
    http_response_text: FuncId,
    http_response_bytes: FuncId,
    websocket_listener_accept: FuncId,
    websocket_listener_local_addr: FuncId,
    websocket_send_text: FuncId,
    websocket_send_bytes: FuncId,
    websocket_recv_text: FuncId,
    websocket_recv_bytes: FuncId,
    websocket_close: FuncId,
    unix_listener_accept: FuncId,
    unix_listener_close: FuncId,
    unix_stream_read_line: FuncId,
    unix_stream_read_exact: FuncId,
    unix_stream_write_all: FuncId,
    unix_stream_close: FuncId,
    tls_listener_accept: FuncId,
    tls_listener_local_addr: FuncId,
    tls_listener_close: FuncId,
    tls_stream_read_line: FuncId,
    tls_stream_read_exact: FuncId,
    tls_stream_write_all: FuncId,
    tls_stream_close: FuncId,
    cancelled: FuncId,
    yield_now: FuncId,
    sleep_value_void: FuncId,
    start_task_call: FuncId,
    string_data: HashMap<Vec<u8>, DataId>,
}

macro_rules! declare_runtime_functions {
    ($object:expr, $( $var:ident => ($name:literal, [$($param:expr),* $(,)?], $ret:expr) ),+ $(,)?) => {
        $(
            let $var = declare_runtime_function($object, $name, &[$($param),*], $ret)?;
        )+
    };
}

macro_rules! try_or_string_error {
    ($expr:expr, $($fmt:tt)+) => {
        match $expr {
            Ok(value) => value,
            Err(error) => return Err(format!($($fmt)+, error)),
        }
    };
}

fn split_field_path_segments<'a>(
    segments: &'a [&'a str],
) -> std::result::Result<(&'a str, &'a [&'a str]), String> {
    match segments.split_first() {
        Some((head, rest)) => Ok((*head, rest)),
        None => Err("internal error: direct backend received an empty field path".to_string()),
    }
}

fn ordered_named_args<'a>(
    expected_names: &[&str],
    args: &'a [MirArg],
) -> std::result::Result<Vec<&'a MirArg>, String> {
    let mut values = vec![None; expected_names.len()];
    let mut next_positional = 0usize;
    for argument in args {
        if let Some(name) = argument.name.as_deref() {
            let mut index = None;
            for (candidate_index, candidate) in expected_names.iter().enumerate() {
                if *candidate == name {
                    index = Some(candidate_index);
                    break;
                }
            }
            let Some(index) = index else {
                return Err(format!(
                    "direct backend does not recognize builtin argument `{}`",
                    name
                ));
            };
            if values[index].is_some() {
                return Err(format!(
                    "direct backend received duplicate builtin argument `{}`",
                    name
                ));
            }
            values[index] = Some(argument);
            continue;
        }
        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err("direct backend received too many builtin arguments".to_string());
        }
        values[next_positional] = Some(argument);
        next_positional += 1;
    }
    let mut ordered = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = value else {
            return Err("direct backend is missing a builtin argument".to_string());
        };
        ordered.push(value);
    }
    Ok(ordered)
}

fn ordered_optional_named_args<'a>(
    expected_names: &[&str],
    args: &'a [MirArg],
) -> std::result::Result<Vec<Option<&'a MirArg>>, String> {
    let mut values = vec![None; expected_names.len()];
    let mut next_positional = 0usize;
    for argument in args {
        if let Some(name) = argument.name.as_deref() {
            let mut index = None;
            for (candidate_index, candidate) in expected_names.iter().enumerate() {
                if *candidate == name {
                    index = Some(candidate_index);
                    break;
                }
            }
            let Some(index) = index else {
                return Err(format!(
                    "direct backend does not recognize builtin argument `{}`",
                    name
                ));
            };
            if values[index].is_some() {
                return Err(format!(
                    "direct backend received duplicate builtin argument `{}`",
                    name
                ));
            }
            values[index] = Some(argument);
            continue;
        }
        while next_positional < values.len() && values[next_positional].is_some() {
            next_positional += 1;
        }
        if next_positional >= values.len() {
            return Err("direct backend received too many builtin arguments".to_string());
        }
        values[next_positional] = Some(argument);
        next_positional += 1;
    }
    Ok(values)
}

fn required_named_arg<'a>(
    argument: Option<&'a MirArg>,
    message: &str,
) -> std::result::Result<&'a MirArg, String> {
    argument.ok_or(message.to_string())
}

fn direct_internal_slice_args(args: &[MirArg]) -> std::result::Result<&[MirArg; 6], String> {
    args.try_into()
        .map_err(|_| DIRECT_INTERNAL_SLICE_ARITY_ERROR.to_string())
}

fn required_direct_field_slice(
    ty: &DirectType,
    field: &str,
) -> std::result::Result<(usize, usize, DirectType), String> {
    ty.field_slice(field).ok_or(format!(
        "direct backend does not know field `{}` on `{}`",
        field,
        render_direct_type(ty)
    ))
}

impl<'a> NativeCodegen<'a> {
    fn new(
        module: &'a MirModule,
        program_path: &str,
        program_source: &str,
    ) -> std::result::Result<Self, String> {
        let reachable_blocks = validate_module(module)?;
        // A program with no task-start operation cannot have a runnable sibling
        // for a loop to starve. Keep explicit MIR markers for portability, but
        // elide their native fast-path cost for provably sequential programs.
        let safepoints_enabled = !collect_task_start_targets(module, &reachable_blocks).is_empty();
        let mut classes = HashMap::new();
        for class in &module.classes {
            classes.insert(class.name.clone(), class.clone());
        }
        let trait_impls = module.trait_impls.clone();

        let flags = native_codegen_flags()?;
        let isa_builder =
            try_or_string_error!(cranelift_native::builder(), "failed to detect host ISA: {}");
        let isa = try_or_string_error!(isa_builder.finish(flags), "failed to build host ISA: {}");
        let call_conv = isa.default_call_conv();
        let builder = try_or_string_error!(
            ObjectBuilder::new(isa, "aura_direct".to_string(), default_libcall_names()),
            "failed to initialize object builder: {}"
        );
        let mut object = ObjectModule::new(builder);

        declare_runtime_functions!(
            &mut object,
            runtime_init => ("aura_direct_runtime_init", [types::I64, types::I64, types::I64, types::I64], None),
            run_root => ("aura_direct_run_root", [types::I64], Some(types::I32)),
            enter_call => ("aura_direct_enter_call_with_frame", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], None),
            exit_call => ("aura_direct_exit_call", [], None),
            set_returned_view_projection => ("aura_direct_set_returned_view_projection", [types::I64, types::I64], None),
            take_returned_view_projection => ("aura_direct_take_returned_view_projection", [types::I64, types::I64], Some(types::I64)),
            print_i64 => ("aura_direct_print_i64", [types::I64], None),
            print_u64 => ("aura_direct_print_u64", [types::I64], None),
            print_f32 => ("aura_direct_print_f32", [types::F64], None),
            print_f64 => ("aura_direct_print_f64", [types::F64], None),
            print_bool => ("aura_direct_print_bool", [types::I64], None),
            print_value => ("aura_direct_print_value", [types::I64], None),
            sqrt_f64 => ("aura_direct_sqrt_f64", [types::F64], Some(types::F64)),
            assert_fail => ("aura_direct_assert_fail", [types::I64, types::I64, types::I64], None),
            assert_fail_detailed => ("aura_direct_assert_fail_detailed", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], None),
            fail_division_by_zero => ("aura_direct_fail_division_by_zero", [types::I64, types::I64], None),
            fail_int32_overflow => ("aura_direct_fail_int32_overflow", [types::I64, types::I64, types::I64], None),
            fail_integer_overflow => ("aura_direct_fail_integer_overflow", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], None),
            register_cleanup => ("aura_direct_register_cleanup", [types::I64, types::I64, types::I64], Some(types::I64)),
            unregister_cleanup => ("aura_direct_unregister_cleanup", [types::I64], None),
            refresh_cleanup => ("aura_direct_refresh_cleanup", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            set_next_mutable_sinks => ("aura_direct_set_next_mutable_sinks", [types::I64, types::I64], None),
            set_next_indirect_mutable_sinks => ("aura_direct_set_next_indirect_mutable_sinks", [types::I64, types::I64, types::I64, types::I64, types::I64], None),
            current_mutable_sink => ("aura_direct_current_mutable_sink", [types::I64], Some(types::I64)),
            mutable_sink_new => ("aura_direct_mutable_sink_new", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            mutable_sink_project => ("aura_direct_mutable_sink_project", [types::I64, types::I64, types::I64], Some(types::I64)),
            mutable_sink_store_owned => ("aura_direct_mutable_sink_store_owned", [types::I64, types::I64], None),
            mutable_sink_release => ("aura_direct_mutable_sink_release", [types::I64], None),
            close_value => ("aura_direct_close_value", [types::I64, types::I64], Some(types::I64)),
            tag_value_type => ("aura_direct_tag_value_type", [types::I64, types::I64, types::I64], None),
            box_i32 => ("aura_direct_box_i32", [types::I64], Some(types::I64)),
            box_i64 => ("aura_direct_box_i64", [types::I64], Some(types::I64)),
            box_u64 => ("aura_direct_box_u64", [types::I64], Some(types::I64)),
            box_uint_literal => ("aura_direct_box_uint_literal", [types::I64, types::I64], Some(types::I64)),
            box_f64 => ("aura_direct_box_f64", [types::F64], Some(types::I64)),
            box_bool => ("aura_direct_box_bool", [types::I64], Some(types::I64)),
            function_value => ("aura_direct_function_value", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            module_constant => ("aura_direct_module_constant", [types::I64, types::I64, types::I64], Some(types::I64)),
            closure_value => ("aura_direct_closure_value", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            closure_capture => ("aura_direct_closure_capture", [types::I64, types::I64], Some(types::I64)),
            function_call => ("aura_direct_function_call", [types::I64, types::I64, types::I64], Some(types::I64)),
            function_bind_defaults => ("aura_direct_function_bind_defaults", [types::I64, types::I64, types::I64, types::I64], None),
            box_unit => ("aura_direct_box_unit", [], Some(types::I64)),
            string_literal => ("aura_direct_string_literal", [types::I64, types::I64], Some(types::I64)),
            string_len => ("aura_direct_string_len", [types::I64], Some(types::I64)),
            string_byte_len => ("aura_direct_string_byte_len", [types::I64], Some(types::I64)),
            string_slice => ("aura_direct_string_slice", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            string_contains => ("aura_direct_string_contains", [types::I64, types::I64], Some(types::I64)),
            string_starts_with => ("aura_direct_string_starts_with", [types::I64, types::I64], Some(types::I64)),
            string_ends_with => ("aura_direct_string_ends_with", [types::I64, types::I64], Some(types::I64)),
            string_split => ("aura_direct_string_split", [types::I64, types::I64], Some(types::I64)),
            string_replace => ("aura_direct_string_replace", [types::I64, types::I64, types::I64], Some(types::I64)),
            string_to_lower => ("aura_direct_string_to_lower", [types::I64], Some(types::I64)),
            string_to_upper => ("aura_direct_string_to_upper", [types::I64], Some(types::I64)),
            string_strip_prefix => ("aura_direct_string_strip_prefix", [types::I64, types::I64], Some(types::I64)),
            string_strip_suffix => ("aura_direct_string_strip_suffix", [types::I64, types::I64], Some(types::I64)),
            string_trim => ("aura_direct_string_trim", [types::I64], Some(types::I64)),
            string_join => ("aura_direct_string_join", [types::I64, types::I64], Some(types::I64)),
            stringify_value => ("aura_direct_stringify_value", [types::I64], Some(types::I64)),
            format_value => ("aura_direct_format_value", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            abs_value => ("aura_direct_abs", [types::I64], Some(types::I64)),
            min_value => ("aura_direct_min", [types::I64, types::I64], Some(types::I64)),
            max_value => ("aura_direct_max", [types::I64, types::I64], Some(types::I64)),
            sqrt_value => ("aura_direct_sqrt", [types::I64], Some(types::I64)),
            round_value => ("aura_direct_round", [types::I64], Some(types::I64)),
            divmod_value => ("aura_direct_divmod", [types::I64, types::I64], Some(types::I64)),
            parse_int32 => ("aura_direct_parse_int32", [types::I64], Some(types::I64)),
            parse_int64 => ("aura_direct_parse_int64", [types::I64], Some(types::I64)),
            parse_float64 => ("aura_direct_parse_float64", [types::I64], Some(types::I64)),
            duration_literal => ("aura_direct_duration_literal", [types::I64, types::I64], Some(types::I64)),
            duration_from_i64 => ("aura_direct_duration_from_i64", [types::I64, types::I64], Some(types::I64)),
            duration_to_float => ("aura_direct_duration_to_float", [types::I64, types::I64], Some(types::F64)),
            rng_new => ("aura_direct_rng_new", [types::I64], Some(types::I64)),
            rng_next_int => ("aura_direct_rng_next_int", [types::I64, types::I64, types::I64], Some(types::I64)),
            rng_next_float => ("aura_direct_rng_next_float", [types::I64], Some(types::F64)),
            rng_shuffle => ("aura_direct_rng_shuffle", [types::I64, types::I64], None),
            random_secure_int => ("aura_direct_random_secure_int", [types::I64, types::I64], Some(types::I64)),
            random_secure_bytes => ("aura_direct_random_secure_bytes", [types::I64], Some(types::I64)),
            range_new => ("aura_direct_range_new", [types::I64, types::I64], Some(types::I64)),
            range_current => ("aura_direct_range_current", [types::I64], Some(types::I64)),
            range_end => ("aura_direct_range_end", [types::I64], Some(types::I64)),
            range_advance => ("aura_direct_range_advance", [types::I64], Some(types::I64)),
            vec_empty => ("aura_direct_vec_empty", [], Some(types::I64)),
            vec_len => ("aura_direct_vec_len", [types::I64], Some(types::I64)),
            vec_is_empty => ("aura_direct_vec_is_empty", [types::I64], Some(types::I64)),
            vec_push_in_place => ("aura_direct_vec_push_in_place", [types::I64, types::I64], Some(types::I64)),
            vec_pop_in_place => ("aura_direct_vec_pop_in_place", [types::I64], Some(types::I64)),
            vec_get => ("aura_direct_vec_get", [types::I64, types::I64], Some(types::I64)),
            vec_set_in_place => ("aura_direct_vec_set_in_place", [types::I64, types::I64, types::I64], Some(types::I64)),
            vec_remove_in_place => ("aura_direct_vec_remove_in_place", [types::I64, types::I64], Some(types::I64)),
            vec_swap_in_place => ("aura_direct_vec_swap_in_place", [types::I64, types::I64, types::I64], Some(types::I64)),
            vec_contains => ("aura_direct_vec_contains", [types::I64, types::I64], Some(types::I64)),
            vec_extend_in_place => ("aura_direct_vec_extend_in_place", [types::I64, types::I64], Some(types::I64)),
            vec_insert_in_place => ("aura_direct_vec_insert_in_place", [types::I64, types::I64, types::I64], Some(types::I64)),
            vec_clear_in_place => ("aura_direct_vec_clear_in_place", [types::I64], Some(types::I64)),
            vec_reverse_in_place => ("aura_direct_vec_reverse_in_place", [types::I64], Some(types::I64)),
            collection_operation => ("aura_direct_collection_operation", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            vec_index => ("aura_direct_vec_index", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            vec_slice => ("aura_direct_vec_slice", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            vec_index_option => ("aura_direct_vec_index_option", [types::I64, types::I64], Some(types::I64)),
            vec_take_index_in_place => ("aura_direct_vec_take_index_in_place", [types::I64, types::I64], Some(types::I64)),
            vec_set_index_in_place => ("aura_direct_vec_set_index_in_place", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_zeros => ("aura_direct_array_zeros", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_full => ("aura_direct_array_full", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_from_vec => ("aura_direct_array_from_vec", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_clone => ("aura_direct_array_clone", [types::I64, types::I64, types::I64], Some(types::I64)),
            array_shape => ("aura_direct_array_shape", [types::I64], Some(types::I64)),
            array_len => ("aura_direct_array_len", [types::I64], Some(types::I64)),
            array_get => ("aura_direct_array_get", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_set_in_place => ("aura_direct_array_set_in_place", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_fill_in_place => ("aura_direct_array_fill_in_place", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_index => ("aura_direct_array_index", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_set_index_in_place => ("aura_direct_array_set_index_in_place", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_slice => ("aura_direct_array_slice", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_binary => ("aura_direct_array_binary", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_map => ("aura_direct_array_map", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            array_reduce => ("aura_direct_array_reduce", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            map_empty => ("aura_direct_map_empty", [], Some(types::I64)),
            map_len => ("aura_direct_map_len", [types::I64], Some(types::I64)),
            map_is_empty => ("aura_direct_map_is_empty", [types::I64], Some(types::I64)),
            map_get => ("aura_direct_map_get", [types::I64, types::I64], Some(types::I64)),
            map_set_in_place => ("aura_direct_map_set_in_place", [types::I64, types::I64, types::I64], Some(types::I64)),
            map_remove_in_place => ("aura_direct_map_remove_in_place", [types::I64, types::I64], Some(types::I64)),
            map_contains_key => ("aura_direct_map_contains_key", [types::I64, types::I64], Some(types::I64)),
            map_keys => ("aura_direct_map_keys", [types::I64], Some(types::I64)),
            map_values => ("aura_direct_map_values", [types::I64], Some(types::I64)),
            map_items => ("aura_direct_map_items", [types::I64], Some(types::I64)),
            map_clear_in_place => ("aura_direct_map_clear_in_place", [types::I64], Some(types::I64)),
            map_extend_in_place => ("aura_direct_map_extend_in_place", [types::I64, types::I64], Some(types::I64)),
            map_index => ("aura_direct_map_index", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            map_set_index_in_place => ("aura_direct_map_set_index_in_place", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            set_empty => ("aura_direct_set_empty", [], Some(types::I64)),
            set_len => ("aura_direct_set_len", [types::I64], Some(types::I64)),
            set_is_empty => ("aura_direct_set_is_empty", [types::I64], Some(types::I64)),
            set_contains => ("aura_direct_set_contains", [types::I64, types::I64], Some(types::I64)),
            set_insert_in_place => ("aura_direct_set_insert_in_place", [types::I64, types::I64], Some(types::I64)),
            set_remove_in_place => ("aura_direct_set_remove_in_place", [types::I64, types::I64], Some(types::I64)),
            set_index_option => ("aura_direct_set_index_option", [types::I64, types::I64], Some(types::I64)),
            set_take_index_in_place => ("aura_direct_set_take_index_in_place", [types::I64, types::I64], Some(types::I64)),
            retain_value => ("aura_direct_retain_value", [types::I64], Some(types::I64)),
            release_value => ("aura_direct_release_value", [types::I64], None),
            clone_value => ("aura_direct_clone_value", [types::I64], Some(types::I64)),
            unbox_i64 => ("aura_direct_unbox_i64", [types::I64], Some(types::I64)),
            unbox_int64 => ("aura_direct_unbox_int64", [types::I64], Some(types::I64)),
            integer_to_float => ("aura_direct_integer_to_float", [types::I64], Some(types::F64)),
            integer_width_binary => ("aura_direct_integer_width_binary", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            unbox_u64 => ("aura_direct_unbox_u64", [types::I64], Some(types::I64)),
            unbox_f64 => ("aura_direct_unbox_f64", [types::I64], Some(types::F64)),
            unbox_bool => ("aura_direct_unbox_bool", [types::I64], Some(types::I64)),
            value_as_condition => ("aura_direct_value_as_condition", [types::I64], Some(types::I64)),
            unary_value => ("aura_direct_unary_value_at", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            binary_value => ("aura_direct_binary_value_at", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            cast_value => ("aura_direct_cast_value_at", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            cast_integer_to_integer => ("aura_direct_cast_integer_to_integer", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            cast_integer_to_float => ("aura_direct_cast_integer_to_float", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::F64)),
            cast_float_to_integer => ("aura_direct_cast_float_to_integer", [types::F64, types::I64, types::I64, types::I64], Some(types::I64)),
            value_type_matches => ("aura_direct_value_type_matches", [types::I64, types::I64, types::I64], Some(types::I64)),
            value_has_runtime_type => ("aura_direct_value_has_runtime_type", [types::I64], Some(types::I64)),
            tuple_new => ("aura_direct_tuple_new", [types::I64, types::I64], Some(types::I64)),
            tuple_element => ("aura_direct_tuple_element", [types::I64, types::I64], Some(types::I64)),
            tuple_take_element => ("aura_direct_tuple_take_element", [types::I64, types::I64], Some(types::I64)),
            enum_variant => ("aura_direct_enum_variant", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            variant_matches => ("aura_direct_variant_matches", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            variant_payload => ("aura_direct_variant_payload", [types::I64, types::I64], Some(types::I64)),
            variant_take_payload => ("aura_direct_variant_take_payload", [types::I64, types::I64], Some(types::I64)),
            instance_empty => ("aura_direct_instance_empty", [types::I64, types::I64], Some(types::I64)),
            instance_get_field => ("aura_direct_instance_get_field", [types::I64, types::I64, types::I64], Some(types::I64)),
            instance_take_field => ("aura_direct_instance_take_field", [types::I64, types::I64, types::I64], Some(types::I64)),
            instance_set_field_owned => ("aura_direct_instance_set_field_owned", [types::I64, types::I64, types::I64, types::I64], None),
            arg_buffer_new => ("aura_direct_arg_buffer_new", [types::I64], Some(types::I64)),
            arg_buffer_store => ("aura_direct_arg_buffer_store", [types::I64, types::I64, types::I64], None),
            arg_buffer_store_owned => ("aura_direct_arg_buffer_store_owned", [types::I64, types::I64, types::I64], None),
            task_arg_buffer_guard => ("aura_direct_task_arg_buffer_guard", [types::I64, types::I64], Some(types::I64)),
            task_arg_buffer_disarm => ("aura_direct_task_arg_buffer_disarm", [types::I64], None),
            host_builtin => ("aura_direct_host_builtin", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            ffi_call => ("aura_direct_ffi_call", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            monotonic_time_ms => ("aura_direct_monotonic_time_ms", [], Some(types::I64)),
            channel_new => ("aura_direct_channel_new", [types::I64], Some(types::I64)),
            channel_send => ("aura_direct_channel_send", [types::I64, types::I64], Some(types::I64)),
            channel_send_timeout_value => ("aura_direct_channel_send_timeout_value", [types::I64, types::I64, types::I64], Some(types::I64)),
            channel_try_send => ("aura_direct_channel_try_send", [types::I64, types::I64], Some(types::I64)),
            channel_recv => ("aura_direct_channel_recv", [types::I64], Some(types::I64)),
            channel_recv_in_task_group => ("aura_direct_channel_recv_in_task_group", [types::I64, types::I64], Some(types::I64)),
            channel_recv_with_registered_producers => ("aura_direct_channel_recv_with_registered_producers", [types::I64], Some(types::I64)),
            channel_recv_timeout_value => ("aura_direct_channel_recv_timeout_value", [types::I64, types::I64], Some(types::I64)),
            channel_recv_or_none => ("aura_direct_channel_recv_or_none", [types::I64], Some(types::I64)),
            channel_recv_or_none_timeout_value => ("aura_direct_channel_recv_or_none_timeout_value", [types::I64, types::I64], Some(types::I64)),
            channel_recv_or_value => ("aura_direct_channel_recv_or_value", [types::I64, types::I64], Some(types::I64)),
            channel_recv_or_value_timeout_value => ("aura_direct_channel_recv_or_value_timeout_value", [types::I64, types::I64, types::I64], Some(types::I64)),
            channel_close => ("aura_direct_channel_close", [types::I64], Some(types::I64)),
            task_group_new => ("aura_direct_task_group_new", [], Some(types::I64)),
            task_group_cancel => ("aura_direct_task_group_cancel", [types::I64], Some(types::I64)),
            task_group_close => ("aura_direct_task_group_close", [types::I64, types::I64], Some(types::I64)),
            task_join => ("aura_direct_task_join", [types::I64], Some(types::I64)),
            task_join_timeout_value => ("aura_direct_task_join_timeout_value", [types::I64, types::I64], Some(types::I64)),
            task_join_or_none => ("aura_direct_task_join_or_none", [types::I64], Some(types::I64)),
            task_join_or_none_timeout_value => ("aura_direct_task_join_or_none_timeout_value", [types::I64, types::I64], Some(types::I64)),
            task_join_or_value => ("aura_direct_task_join_or_value", [types::I64, types::I64], Some(types::I64)),
            task_join_or_value_timeout_value => ("aura_direct_task_join_or_value_timeout_value", [types::I64, types::I64, types::I64], Some(types::I64)),
            wait_any => ("aura_direct_wait_any", [types::I64], Some(types::I64)),
            wait_any_timeout_value => ("aura_direct_wait_any_timeout_value", [types::I64, types::I64], Some(types::I64)),
            wait_all => ("aura_direct_wait_all", [types::I64], Some(types::I64)),
            wait_all_timeout_value => ("aura_direct_wait_all_timeout_value", [types::I64, types::I64], Some(types::I64)),
            select => ("aura_direct_select", [types::I64], Some(types::I64)),
            io_write => ("aura_direct_io_write", [types::I64], Some(types::I64)),
            io_flush => ("aura_direct_io_flush", [], Some(types::I64)),
            io_read_line => ("aura_direct_io_read_line", [], Some(types::I64)),
            fs_exists => ("aura_direct_fs_exists", [types::I64], Some(types::I64)),
            fs_read_to_string => ("aura_direct_fs_read_to_string", [types::I64], Some(types::I64)),
            fs_read_bytes => ("aura_direct_fs_read_bytes", [types::I64], Some(types::I64)),
            fs_write_string => ("aura_direct_fs_write_string", [types::I64, types::I64], Some(types::I64)),
            fs_write_bytes => ("aura_direct_fs_write_bytes", [types::I64, types::I64], Some(types::I64)),
            fs_append_string => ("aura_direct_fs_append_string", [types::I64, types::I64], Some(types::I64)),
            fs_append_bytes => ("aura_direct_fs_append_bytes", [types::I64, types::I64], Some(types::I64)),
            fs_create_dir => ("aura_direct_fs_create_dir", [types::I64], Some(types::I64)),
            fs_read_dir => ("aura_direct_fs_read_dir", [types::I64], Some(types::I64)),
            fs_remove_file => ("aura_direct_fs_remove_file", [types::I64], Some(types::I64)),
            fs_open => ("aura_direct_fs_open", [types::I64], Some(types::I64)),
            fs_create => ("aura_direct_fs_create", [types::I64], Some(types::I64)),
            fs_append => ("aura_direct_fs_append", [types::I64], Some(types::I64)),
            file_read_all => ("aura_direct_file_read_all", [types::I64], Some(types::I64)),
            file_read_bytes => ("aura_direct_file_read_bytes", [types::I64], Some(types::I64)),
            file_write_all => ("aura_direct_file_write_all", [types::I64, types::I64], Some(types::I64)),
            file_write_bytes => ("aura_direct_file_write_bytes", [types::I64, types::I64], Some(types::I64)),
            file_flush => ("aura_direct_file_flush", [types::I64], Some(types::I64)),
            file_close => ("aura_direct_file_close", [types::I64], Some(types::I64)),
            process_inherit => ("aura_direct_process_inherit", [], Some(types::I64)),
            process_null => ("aura_direct_process_null", [], Some(types::I64)),
            process_pipe => ("aura_direct_process_pipe", [], Some(types::I64)),
            process_supervisor => ("aura_direct_process_supervisor", [], Some(types::I64)),
            process_start => ("aura_direct_process_start", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            process_run => ("aura_direct_process_run", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            process_child_stdin => ("aura_direct_process_child_stdin", [types::I64], Some(types::I64)),
            process_child_stdout => ("aura_direct_process_child_stdout", [types::I64], Some(types::I64)),
            process_child_stderr => ("aura_direct_process_child_stderr", [types::I64], Some(types::I64)),
            process_child_wait => ("aura_direct_process_child_wait", [types::I64, types::I64], Some(types::I64)),
            process_child_wait_or_none => ("aura_direct_process_child_wait_or_none", [types::I64, types::I64], Some(types::I64)),
            process_child_wait_ok => ("aura_direct_process_child_wait_ok", [types::I64, types::I64], Some(types::I64)),
            process_child_kill => ("aura_direct_process_child_kill", [types::I64], Some(types::I64)),
            process_child_terminate => ("aura_direct_process_child_terminate", [types::I64], Some(types::I64)),
            process_child_close => ("aura_direct_process_child_close", [types::I64], Some(types::I64)),
            process_pipe_read_all => ("aura_direct_process_pipe_read_all", [types::I64], Some(types::I64)),
            process_pipe_read_line => ("aura_direct_process_pipe_read_line", [types::I64, types::I64], Some(types::I64)),
            process_pipe_read_bytes => ("aura_direct_process_pipe_read_bytes", [types::I64, types::I64, types::I64], Some(types::I64)),
            process_pipe_write_all => ("aura_direct_process_pipe_write_all", [types::I64, types::I64, types::I64], Some(types::I64)),
            process_pipe_write_bytes => ("aura_direct_process_pipe_write_bytes", [types::I64, types::I64, types::I64], Some(types::I64)),
            process_pipe_flush => ("aura_direct_process_pipe_flush", [types::I64], Some(types::I64)),
            process_pipe_close => ("aura_direct_process_pipe_close", [types::I64], Some(types::I64)),
            process_completed_status => ("aura_direct_process_completed_status", [types::I64], Some(types::I64)),
            process_completed_success => ("aura_direct_process_completed_success", [types::I64], Some(types::I64)),
            process_completed_stdout => ("aura_direct_process_completed_stdout", [types::I64], Some(types::I64)),
            process_completed_stderr => ("aura_direct_process_completed_stderr", [types::I64], Some(types::I64)),
            process_completed_stdout_bytes => ("aura_direct_process_completed_stdout_bytes", [types::I64], Some(types::I64)),
            process_completed_stderr_bytes => ("aura_direct_process_completed_stderr_bytes", [types::I64], Some(types::I64)),
            process_completed_check => ("aura_direct_process_completed_check", [types::I64], Some(types::I64)),
            process_supervisor_start => ("aura_direct_process_supervisor_start", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            process_supervisor_wait => ("aura_direct_process_supervisor_wait", [types::I64, types::I64], Some(types::I64)),
            process_supervisor_wait_or_none => ("aura_direct_process_supervisor_wait_or_none", [types::I64, types::I64], Some(types::I64)),
            process_supervisor_stop => ("aura_direct_process_supervisor_stop", [types::I64], Some(types::I64)),
            process_supervisor_is_empty => ("aura_direct_process_supervisor_is_empty", [types::I64], Some(types::I64)),
            process_supervisor_close => ("aura_direct_process_supervisor_close", [types::I64], Some(types::I64)),
            net_connect => ("aura_direct_net_connect", [types::I64], Some(types::I64)),
            net_connect_timeout => ("aura_direct_net_connect_timeout", [types::I64, types::I64], Some(types::I64)),
            net_listen => ("aura_direct_net_listen", [types::I64], Some(types::I64)),
            net_udp_bind => ("aura_direct_net_udp_bind", [types::I64], Some(types::I64)),
            net_unix_listen => ("aura_direct_net_unix_listen", [types::I64], Some(types::I64)),
            net_unix_connect => ("aura_direct_net_unix_connect", [types::I64], Some(types::I64)),
            net_unix_connect_timeout => ("aura_direct_net_unix_connect_timeout", [types::I64, types::I64], Some(types::I64)),
            net_tls_listen => ("aura_direct_net_tls_listen", [types::I64, types::I64, types::I64], Some(types::I64)),
            net_tls_connect => ("aura_direct_net_tls_connect", [types::I64, types::I64, types::I64], Some(types::I64)),
            net_tls_connect_timeout => ("aura_direct_net_tls_connect_timeout", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            net_http_listen => ("aura_direct_net_http_listen", [types::I64], Some(types::I64)),
            net_http_request_text => ("aura_direct_net_http_request_text", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            net_http_request_text_timeout => ("aura_direct_net_http_request_text_timeout", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            net_http_request_bytes => ("aura_direct_net_http_request_bytes", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            net_http_request_bytes_timeout => ("aura_direct_net_http_request_bytes_timeout", [types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            net_websocket_listen => ("aura_direct_net_websocket_listen", [types::I64], Some(types::I64)),
            net_websocket_connect => ("aura_direct_net_websocket_connect", [types::I64], Some(types::I64)),
            net_websocket_connect_timeout => ("aura_direct_net_websocket_connect_timeout", [types::I64, types::I64], Some(types::I64)),
            tcp_listener_accept => ("aura_direct_tcp_listener_accept", [types::I64, types::I64], Some(types::I64)),
            tcp_listener_local_addr => ("aura_direct_tcp_listener_local_addr", [types::I64], Some(types::I64)),
            tcp_listener_close => ("aura_direct_tcp_listener_close", [types::I64], Some(types::I64)),
            tcp_stream_read_all => ("aura_direct_tcp_stream_read_all", [types::I64, types::I64], Some(types::I64)),
            tcp_stream_read_line => ("aura_direct_tcp_stream_read_line", [types::I64, types::I64], Some(types::I64)),
            tcp_stream_read_bytes => ("aura_direct_tcp_stream_read_bytes", [types::I64, types::I64, types::I64], Some(types::I64)),
            tcp_stream_read_exact => ("aura_direct_tcp_stream_read_exact", [types::I64, types::I64, types::I64], Some(types::I64)),
            tcp_stream_write_all => ("aura_direct_tcp_stream_write_all", [types::I64, types::I64, types::I64], Some(types::I64)),
            tcp_stream_write_bytes => ("aura_direct_tcp_stream_write_bytes", [types::I64, types::I64, types::I64], Some(types::I64)),
            tcp_stream_flush => ("aura_direct_tcp_stream_flush", [types::I64], Some(types::I64)),
            tcp_stream_local_addr => ("aura_direct_tcp_stream_local_addr", [types::I64], Some(types::I64)),
            tcp_stream_peer_addr => ("aura_direct_tcp_stream_peer_addr", [types::I64], Some(types::I64)),
            tcp_stream_shutdown_read => ("aura_direct_tcp_stream_shutdown_read", [types::I64], Some(types::I64)),
            tcp_stream_shutdown_write => ("aura_direct_tcp_stream_shutdown_write", [types::I64], Some(types::I64)),
            tcp_stream_shutdown_both => ("aura_direct_tcp_stream_shutdown_both", [types::I64], Some(types::I64)),
            tcp_stream_close => ("aura_direct_tcp_stream_close", [types::I64], Some(types::I64)),
            udp_socket_send_text => ("aura_direct_udp_socket_send_text", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            udp_socket_send_bytes => ("aura_direct_udp_socket_send_bytes", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            udp_socket_recv => ("aura_direct_udp_socket_recv", [types::I64, types::I64, types::I64], Some(types::I64)),
            udp_socket_recv_from => ("aura_direct_udp_socket_recv_from", [types::I64, types::I64, types::I64], Some(types::I64)),
            udp_socket_local_addr => ("aura_direct_udp_socket_local_addr", [types::I64], Some(types::I64)),
            udp_socket_peer_addr => ("aura_direct_udp_socket_peer_addr", [types::I64], Some(types::I64)),
            udp_socket_close => ("aura_direct_udp_socket_close", [types::I64], Some(types::I64)),
            udp_datagram_address => ("aura_direct_udp_datagram_address", [types::I64], Some(types::I64)),
            udp_datagram_bytes => ("aura_direct_udp_datagram_bytes", [types::I64], Some(types::I64)),
            udp_datagram_text => ("aura_direct_udp_datagram_text", [types::I64], Some(types::I64)),
            http_listener_accept => ("aura_direct_http_listener_accept", [types::I64, types::I64], Some(types::I64)),
            http_listener_local_addr => ("aura_direct_http_listener_local_addr", [types::I64], Some(types::I64)),
            http_listener_close => ("aura_direct_http_listener_close", [types::I64], Some(types::I64)),
            http_exchange_method => ("aura_direct_http_exchange_method", [types::I64], Some(types::I64)),
            http_exchange_path => ("aura_direct_http_exchange_path", [types::I64], Some(types::I64)),
            http_exchange_headers => ("aura_direct_http_exchange_headers", [types::I64], Some(types::I64)),
            http_exchange_body_text => ("aura_direct_http_exchange_body_text", [types::I64], Some(types::I64)),
            http_exchange_body_bytes => ("aura_direct_http_exchange_body_bytes", [types::I64], Some(types::I64)),
            http_exchange_respond_text => ("aura_direct_http_exchange_respond_text", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            http_exchange_respond_bytes => ("aura_direct_http_exchange_respond_bytes", [types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
            http_response_status => ("aura_direct_http_response_status", [types::I64], Some(types::I64)),
            http_response_reason => ("aura_direct_http_response_reason", [types::I64], Some(types::I64)),
            http_response_headers => ("aura_direct_http_response_headers", [types::I64], Some(types::I64)),
            http_response_text => ("aura_direct_http_response_text", [types::I64], Some(types::I64)),
            http_response_bytes => ("aura_direct_http_response_bytes", [types::I64], Some(types::I64)),
            websocket_listener_accept => ("aura_direct_websocket_listener_accept", [types::I64, types::I64], Some(types::I64)),
            websocket_listener_local_addr => ("aura_direct_websocket_listener_local_addr", [types::I64], Some(types::I64)),
            websocket_send_text => ("aura_direct_websocket_send_text", [types::I64, types::I64, types::I64], Some(types::I64)),
            websocket_send_bytes => ("aura_direct_websocket_send_bytes", [types::I64, types::I64, types::I64], Some(types::I64)),
            websocket_recv_text => ("aura_direct_websocket_recv_text", [types::I64, types::I64], Some(types::I64)),
            websocket_recv_bytes => ("aura_direct_websocket_recv_bytes", [types::I64, types::I64], Some(types::I64)),
            websocket_close => ("aura_direct_websocket_close", [types::I64], Some(types::I64)),
            unix_listener_accept => ("aura_direct_unix_listener_accept", [types::I64, types::I64], Some(types::I64)),
            unix_listener_close => ("aura_direct_unix_listener_close", [types::I64], Some(types::I64)),
            unix_stream_read_line => ("aura_direct_unix_stream_read_line", [types::I64, types::I64], Some(types::I64)),
            unix_stream_read_exact => ("aura_direct_unix_stream_read_exact", [types::I64, types::I64, types::I64], Some(types::I64)),
            unix_stream_write_all => ("aura_direct_unix_stream_write_all", [types::I64, types::I64, types::I64], Some(types::I64)),
            unix_stream_close => ("aura_direct_unix_stream_close", [types::I64], Some(types::I64)),
            tls_listener_accept => ("aura_direct_tls_listener_accept", [types::I64, types::I64], Some(types::I64)),
            tls_listener_local_addr => ("aura_direct_tls_listener_local_addr", [types::I64], Some(types::I64)),
            tls_listener_close => ("aura_direct_tls_listener_close", [types::I64], Some(types::I64)),
            tls_stream_read_line => ("aura_direct_tls_stream_read_line", [types::I64, types::I64], Some(types::I64)),
            tls_stream_read_exact => ("aura_direct_tls_stream_read_exact", [types::I64, types::I64, types::I64], Some(types::I64)),
            tls_stream_write_all => ("aura_direct_tls_stream_write_all", [types::I64, types::I64, types::I64], Some(types::I64)),
            tls_stream_close => ("aura_direct_tls_stream_close", [types::I64], Some(types::I64)),
            cancelled => ("aura_direct_cancelled", [], Some(types::I64)),
            yield_now => ("aura_direct_yield_now", [], None),
            sleep_value_void => ("aura_direct_sleep_value_void", [types::I64], None),
            start_task_call => ("aura_direct_start_task_function_with_frames", [types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64], Some(types::I64)),
        );

        let mut functions = HashMap::new();
        let mut function_thunks = HashMap::new();
        let mut function_default_binders = HashMap::new();
        let mut cleanup_thunks = HashMap::new();
        let mut function_return_types = HashMap::new();
        let mut function_param_types = HashMap::new();
        let mut function_writeback_types = HashMap::new();
        for function in module.functions.iter().chain(module.top_level.iter()) {
            let function_reachable = reachable_blocks.get(&function.name).ok_or_else(|| {
                format!(
                    "direct backend is missing reachable-block metadata for `{}`",
                    function.name
                )
            })?;
            let signature = signature_for(function, &classes, call_conv)?;
            let func_id = try_or_string_error!(
                object.declare_function(&mangle_symbol(&function.name), Linkage::Local, &signature),
                "failed to declare function `{}`: {}",
                function.name
            );
            functions.insert(function.name.clone(), func_id);
            let thunk_signature = thunk_signature(call_conv);
            let thunk_id = try_or_string_error!(
                object.declare_function(
                    &mangle_thunk_symbol(&function.name),
                    Linkage::Local,
                    &thunk_signature,
                ),
                "failed to declare function thunk `{}`: {}",
                function.name
            );
            function_thunks.insert(function.name.clone(), thunk_id);
            let binder_signature = default_binder_signature(call_conv);
            let binder_id = try_or_string_error!(
                object.declare_function(
                    &mangle_default_binder_symbol(&function.name),
                    Linkage::Local,
                    &binder_signature,
                ),
                "failed to declare function default binder `{}`: {}",
                function.name
            );
            function_default_binders.insert(function.name.clone(), binder_id);
            for (cleanup_index, place) in collect_cleanup_places(function, function_reachable)
                .into_iter()
                .enumerate()
            {
                let cleanup_id = try_or_string_error!(
                    object.declare_function(
                        &mangle_cleanup_thunk_symbol(&function.name, &place, cleanup_index),
                        Linkage::Local,
                        &thunk_signature,
                    ),
                    "failed to declare cleanup thunk for `{}` in `{}`: {}",
                    place,
                    function.name
                );
                cleanup_thunks.insert((function.name.clone(), place), cleanup_id);
            }
            function_return_types.insert(
                function.name.clone(),
                ensure_direct_type(
                    &function.return_type,
                    &classes,
                    &format!("return type of `{}`", function.name),
                )?,
            );
            let mut params = Vec::new();
            let mut writebacks = Vec::new();
            if function.receiver == Some(MirReceiverKind::BorrowMut) {
                writebacks.push(receiver_type(function, &classes)?);
            }
            if function.receiver.is_some() {
                params.push(receiver_type(function, &classes)?);
            }
            for param in &function.params {
                if param.passing == MirReceiverKind::BorrowMut {
                    writebacks.push(ensure_direct_type(
                        &param.ty,
                        &classes,
                        &format!("parameter `{}` on `{}`", param.name, function.name),
                    )?);
                }
                params.push(ensure_direct_type(
                    &param.ty,
                    &classes,
                    &format!("parameter `{}` on `{}`", param.name, function.name),
                )?);
            }
            function_param_types.insert(function.name.clone(), params);
            function_writeback_types.insert(function.name.clone(), writebacks);
        }

        Ok(Self {
            module,
            reachable_blocks,
            safepoints_enabled,
            program_path: program_path.to_string(),
            program_source: program_source.to_string(),
            object,
            functions,
            function_thunks,
            function_default_binders,
            cleanup_thunks,
            classes,
            trait_impls,
            function_return_types,
            function_param_types,
            function_writeback_types,
            call_conv,
            runtime_init,
            run_root,
            enter_call,
            exit_call,
            set_returned_view_projection,
            take_returned_view_projection,
            print_i64,
            print_u64,
            print_f32,
            print_f64,
            print_bool,
            print_value,
            sqrt_f64,
            assert_fail,
            assert_fail_detailed,
            fail_division_by_zero,
            fail_int32_overflow,
            fail_integer_overflow,
            register_cleanup,
            unregister_cleanup,
            refresh_cleanup,
            set_next_mutable_sinks,
            set_next_indirect_mutable_sinks,
            current_mutable_sink,
            mutable_sink_new,
            mutable_sink_project,
            mutable_sink_store_owned,
            mutable_sink_release,
            close_value,
            tag_value_type,
            box_i32,
            box_i64,
            box_u64,
            box_uint_literal,
            box_f64,
            box_bool,
            function_value,
            module_constant,
            closure_value,
            closure_capture,
            function_call,
            function_bind_defaults,
            box_unit,
            string_literal,
            string_len,
            string_byte_len,
            string_slice,
            string_contains,
            string_starts_with,
            string_ends_with,
            string_split,
            string_replace,
            string_to_lower,
            string_to_upper,
            string_strip_prefix,
            string_strip_suffix,
            string_trim,
            string_join,
            stringify_value,
            format_value,
            abs_value,
            min_value,
            max_value,
            sqrt_value,
            round_value,
            divmod_value,
            parse_int32,
            parse_int64,
            parse_float64,
            duration_literal,
            duration_from_i64,
            duration_to_float,
            rng_new,
            rng_next_int,
            rng_next_float,
            rng_shuffle,
            random_secure_int,
            random_secure_bytes,
            range_new,
            range_current,
            range_end,
            range_advance,
            vec_empty,
            vec_len,
            vec_is_empty,
            vec_push_in_place,
            vec_pop_in_place,
            vec_get,
            vec_set_in_place,
            vec_remove_in_place,
            vec_swap_in_place,
            vec_contains,
            vec_extend_in_place,
            vec_insert_in_place,
            vec_clear_in_place,
            vec_reverse_in_place,
            collection_operation,
            vec_index,
            vec_slice,
            vec_index_option,
            vec_take_index_in_place,
            vec_set_index_in_place,
            array_zeros,
            array_full,
            array_from_vec,
            array_clone,
            array_shape,
            array_len,
            array_get,
            array_set_in_place,
            array_fill_in_place,
            array_index,
            array_set_index_in_place,
            array_slice,
            array_binary,
            array_map,
            array_reduce,
            map_empty,
            map_len,
            map_is_empty,
            map_get,
            map_set_in_place,
            map_remove_in_place,
            map_contains_key,
            map_keys,
            map_values,
            map_items,
            map_clear_in_place,
            map_extend_in_place,
            map_index,
            map_set_index_in_place,
            set_empty,
            set_len,
            set_is_empty,
            set_contains,
            set_insert_in_place,
            set_remove_in_place,
            set_index_option,
            set_take_index_in_place,
            retain_value,
            release_value,
            clone_value,
            unbox_i64,
            unbox_int64,
            integer_to_float,
            integer_width_binary,
            unbox_u64,
            unbox_f64,
            unbox_bool,
            value_as_condition,
            unary_value,
            binary_value,
            cast_value,
            cast_integer_to_integer,
            cast_integer_to_float,
            cast_float_to_integer,
            value_type_matches,
            value_has_runtime_type,
            tuple_new,
            tuple_element,
            tuple_take_element,
            enum_variant,
            variant_matches,
            variant_payload,
            variant_take_payload,
            instance_empty,
            instance_get_field,
            instance_take_field,
            instance_set_field_owned,
            arg_buffer_new,
            arg_buffer_store,
            arg_buffer_store_owned,
            task_arg_buffer_guard,
            task_arg_buffer_disarm,
            host_builtin,
            ffi_call,
            monotonic_time_ms,
            channel_new,
            channel_send,
            channel_send_timeout_value,
            channel_try_send,
            channel_recv,
            channel_recv_in_task_group,
            channel_recv_with_registered_producers,
            channel_recv_timeout_value,
            channel_recv_or_none,
            channel_recv_or_none_timeout_value,
            channel_recv_or_value,
            channel_recv_or_value_timeout_value,
            channel_close,
            task_group_new,
            task_group_cancel,
            task_group_close,
            task_join,
            task_join_timeout_value,
            task_join_or_none,
            task_join_or_none_timeout_value,
            task_join_or_value,
            task_join_or_value_timeout_value,
            wait_any,
            wait_any_timeout_value,
            wait_all,
            wait_all_timeout_value,
            select,
            io_write,
            io_flush,
            io_read_line,
            fs_exists,
            fs_read_to_string,
            fs_read_bytes,
            fs_write_string,
            fs_write_bytes,
            fs_append_string,
            fs_append_bytes,
            fs_create_dir,
            fs_read_dir,
            fs_remove_file,
            fs_open,
            fs_create,
            fs_append,
            file_read_all,
            file_read_bytes,
            file_write_all,
            file_write_bytes,
            file_flush,
            file_close,
            process_inherit,
            process_null,
            process_pipe,
            process_supervisor,
            process_start,
            process_run,
            process_child_stdin,
            process_child_stdout,
            process_child_stderr,
            process_child_wait,
            process_child_wait_or_none,
            process_child_wait_ok,
            process_child_kill,
            process_child_terminate,
            process_child_close,
            process_pipe_read_all,
            process_pipe_read_line,
            process_pipe_read_bytes,
            process_pipe_write_all,
            process_pipe_write_bytes,
            process_pipe_flush,
            process_pipe_close,
            process_completed_status,
            process_completed_success,
            process_completed_stdout,
            process_completed_stderr,
            process_completed_stdout_bytes,
            process_completed_stderr_bytes,
            process_completed_check,
            process_supervisor_start,
            process_supervisor_wait,
            process_supervisor_wait_or_none,
            process_supervisor_stop,
            process_supervisor_is_empty,
            process_supervisor_close,
            net_connect,
            net_connect_timeout,
            net_listen,
            net_udp_bind,
            net_unix_listen,
            net_unix_connect,
            net_unix_connect_timeout,
            net_tls_listen,
            net_tls_connect,
            net_tls_connect_timeout,
            net_http_listen,
            net_http_request_text,
            net_http_request_text_timeout,
            net_http_request_bytes,
            net_http_request_bytes_timeout,
            net_websocket_listen,
            net_websocket_connect,
            net_websocket_connect_timeout,
            tcp_listener_accept,
            tcp_listener_local_addr,
            tcp_listener_close,
            tcp_stream_read_all,
            tcp_stream_read_line,
            tcp_stream_read_bytes,
            tcp_stream_read_exact,
            tcp_stream_write_all,
            tcp_stream_write_bytes,
            tcp_stream_flush,
            tcp_stream_local_addr,
            tcp_stream_peer_addr,
            tcp_stream_shutdown_read,
            tcp_stream_shutdown_write,
            tcp_stream_shutdown_both,
            tcp_stream_close,
            udp_socket_send_text,
            udp_socket_send_bytes,
            udp_socket_recv,
            udp_socket_recv_from,
            udp_socket_local_addr,
            udp_socket_peer_addr,
            udp_socket_close,
            udp_datagram_address,
            udp_datagram_bytes,
            udp_datagram_text,
            http_listener_accept,
            http_listener_local_addr,
            http_listener_close,
            http_exchange_method,
            http_exchange_path,
            http_exchange_headers,
            http_exchange_body_text,
            http_exchange_body_bytes,
            http_exchange_respond_text,
            http_exchange_respond_bytes,
            http_response_status,
            http_response_reason,
            http_response_headers,
            http_response_text,
            http_response_bytes,
            websocket_listener_accept,
            websocket_listener_local_addr,
            websocket_send_text,
            websocket_send_bytes,
            websocket_recv_text,
            websocket_recv_bytes,
            websocket_close,
            unix_listener_accept,
            unix_listener_close,
            unix_stream_read_line,
            unix_stream_read_exact,
            unix_stream_write_all,
            unix_stream_close,
            tls_listener_accept,
            tls_listener_local_addr,
            tls_listener_close,
            tls_stream_read_line,
            tls_stream_read_exact,
            tls_stream_write_all,
            tls_stream_close,
            cancelled,
            yield_now,
            sleep_value_void,
            start_task_call,
            string_data: HashMap::new(),
        })
    }

    fn emit(mut self) -> std::result::Result<Vec<u8>, String> {
        for function in self
            .module
            .functions
            .iter()
            .chain(self.module.top_level.iter())
        {
            self.define_function(function)?;
            if function.receiver.is_none() {
                self.define_function_thunk(function)?;
                self.define_function_default_binder(function)?;
            }
        }
        for function in self
            .module
            .functions
            .iter()
            .chain(self.module.top_level.iter())
        {
            self.define_cleanup_thunks(function)?;
        }
        self.define_main_wrapper()?;
        let product = self.object.finish();
        match product.emit() {
            Ok(bytes) => Ok(bytes),
            Err(error) => Err(format!("failed to emit direct backend object: {}", error)),
        }
    }

    fn define_function(&mut self, function: &MirFunction) -> std::result::Result<(), String> {
        let func_id = self.functions[&function.name];
        let mut ctx = self.object.make_context();
        ctx.func.signature = signature_for(function, &self.classes, self.call_conv)?;
        ctx.func.name = UserFuncName::user(0, func_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let reachable_blocks = self
            .reachable_blocks
            .get(&function.name)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "direct backend is missing reachable-block metadata for `{}`",
                    function.name
                )
            })?;
        let mut blocks = HashMap::new();
        for block in &function.blocks {
            if !reachable_blocks.contains(&block.label) {
                continue;
            }
            blocks.insert(block.label.clone(), builder.create_block());
        }

        let entry = match blocks.get(&function.entry) {
            Some(entry) => *entry,
            None => {
                return Err(format!(
                    "direct backend could not find entry block `{}`",
                    function.entry
                ));
            }
        };
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let enter_call = self
            .object
            .declare_func_in_func(self.enter_call, builder.func);
        let exit_call = self
            .object
            .declare_func_in_func(self.exit_call, builder.func);
        let set_returned_view_projection = self
            .object
            .declare_func_in_func(self.set_returned_view_projection, builder.func);
        let take_returned_view_projection = self
            .object
            .declare_func_in_func(self.take_returned_view_projection, builder.func);
        let line = builder.ins().iconst(types::I64, function.span.line as i64);
        let column = builder
            .ins()
            .iconst(types::I64, function.span.column as i64);
        let public_function_name = public_direct_function_name(&function.name);
        let (function_ptr, function_len) = declare_string_constant(
            &mut self.object,
            &mut self.string_data,
            &mut builder,
            public_function_name.as_bytes(),
        )?;
        let function_path = function
            .source_path
            .as_deref()
            .unwrap_or(&self.program_path);
        let (path_ptr, path_len) = declare_string_constant(
            &mut self.object,
            &mut self.string_data,
            &mut builder,
            function_path.as_bytes(),
        )?;
        builder.ins().call(
            enter_call,
            &[line, column, path_ptr, path_len, function_ptr, function_len],
        );

        let mut variable_index = 0usize;
        let mut variables = HashMap::new();
        let mut variable_types = HashMap::new();
        let entry_values = builder.block_params(entry).to_vec();
        let mut entry_index = 0usize;

        if function.receiver.is_some() {
            let receiver_ty = receiver_type(function, &self.classes)?;
            let end = entry_index + receiver_ty.value_count();
            declare_root_variables(
                &mut builder,
                &mut variable_index,
                &mut variables,
                &mut variable_types,
                "self".to_string(),
                receiver_ty,
                Some(&entry_values[entry_index..end]),
            );
            entry_index = end;
        }

        for param in &function.params {
            let ty = ensure_direct_type(
                &param.ty,
                &self.classes,
                &format!("parameter `{}` on `{}`", param.name, function.name),
            )?;
            let end = entry_index + ty.value_count();
            declare_root_variables(
                &mut builder,
                &mut variable_index,
                &mut variables,
                &mut variable_types,
                param.name.clone(),
                ty,
                Some(&entry_values[entry_index..end]),
            );
            entry_index = end;
        }

        for local in &function.local_types {
            if variables.contains_key(&local.name) {
                continue;
            }
            let ty = ensure_direct_type(
                &local.ty,
                &self.classes,
                &format!("local `{}` on `{}`", local.name, function.name),
            )?;
            declare_root_variables(
                &mut builder,
                &mut variable_index,
                &mut variables,
                &mut variable_types,
                local.name.clone(),
                ty,
                None,
            );
        }

        let temporary_assignments = function
            .blocks
            .iter()
            .filter(|block| reachable_blocks.contains(&block.label))
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| {
                let Instruction::Assign { target, value } = instruction else {
                    return None;
                };
                if target.contains('.') {
                    return None;
                }
                if variables.contains_key(target)
                    && !variable_types
                        .get(target)
                        .is_some_and(direct_type_contains_unknown)
                {
                    return None;
                }
                Some((target, value))
            })
            .collect::<Vec<_>>();
        for _ in 0..=temporary_assignments.len() {
            let mut changed = false;
            for (target, value) in &temporary_assignments {
                let Some(inferred) = infer_rvalue_type(
                    value,
                    &variable_types,
                    &self.function_return_types,
                    &self.classes,
                ) else {
                    continue;
                };
                if variable_types.get(*target) != Some(&inferred) {
                    variable_types.insert((*target).clone(), inferred);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (target, _) in temporary_assignments {
            if variables.contains_key(target) {
                continue;
            }
            let ty = variable_types.get(target).cloned().ok_or_else(|| {
                format!(
                    "direct backend could not infer direct type for temporary `{}` in `{}`",
                    target, function.name
                )
            })?;
            declare_root_variables(
                &mut builder,
                &mut variable_index,
                &mut variables,
                &mut variable_types,
                target.clone(),
                ty,
                None,
            );
        }

        let mut cleanup_places = Vec::<String>::new();
        for block in &function.blocks {
            if !reachable_blocks.contains(&block.label) {
                continue;
            }
            for instruction in &block.instructions {
                let Instruction::PushCleanup { place } = instruction else {
                    continue;
                };
                if !cleanup_places.contains(place) {
                    cleanup_places.push(place.clone());
                }
            }
        }
        let mut cleanup_active_vars = HashMap::new();
        let mut cleanup_registration_vars = HashMap::new();
        for place in &cleanup_places {
            let variable = Variable::from_u32(variable_index as u32);
            variable_index += 1;
            builder.declare_var(variable, types::I64);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(variable, zero);
            cleanup_active_vars.insert(place.clone(), variable);

            let registration_variable = Variable::from_u32(variable_index as u32);
            variable_index += 1;
            builder.declare_var(registration_variable, types::I64);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(registration_variable, zero);
            cleanup_registration_vars.insert(place.clone(), registration_variable);
        }

        let safepoint_fuel = if self.safepoints_enabled
            && function.blocks.iter().any(|block| {
                reachable_blocks.contains(&block.label)
                    && block
                        .instructions
                        .iter()
                        .any(|instruction| matches!(instruction, Instruction::Safepoint))
            }) {
            let variable = Variable::from_u32(variable_index as u32);
            variable_index += 1;
            builder.declare_var(variable, types::I64);
            let initial = builder
                .ins()
                .iconst(types::I64, NATIVE_LOOP_SAFEPOINT_INTERVAL as i64);
            builder.def_var(variable, initial);
            Some(variable)
        } else {
            None
        };

        let view_selector_tags = direct_view_selector_tags(function, &reachable_blocks);
        let mut view_selector_vars = HashMap::new();
        let mut selector_loans = view_selector_tags.keys().cloned().collect::<Vec<_>>();
        selector_loans.sort();
        for loan in selector_loans {
            let variable = Variable::from_u32(variable_index as u32);
            variable_index += 1;
            builder.declare_var(variable, types::I64);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(variable, zero);
            view_selector_vars.insert(loan, variable);
        }

        let mut closure_selector_tags = HashMap::new();
        let mut closure_selector_counts = HashMap::<String, i64>::new();
        for block in &function.blocks {
            if !reachable_blocks.contains(&block.label) {
                continue;
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let Instruction::Assign {
                    target,
                    value: Rvalue::Closure { captures, .. },
                } = instruction
                else {
                    continue;
                };
                if !captures
                    .iter()
                    .any(|capture| capture.passing == MirReceiverKind::BorrowMut)
                {
                    continue;
                }
                let root = target.split('.').next().unwrap_or(target).to_string();
                let next = closure_selector_counts.entry(root).or_default();
                closure_selector_tags.insert((block.label.clone(), instruction_index), *next);
                *next = next.checked_add(1).ok_or_else(|| {
                    format!(
                        "direct backend closure selector count overflows in `{}`",
                        function.name
                    )
                })?;
            }
        }
        let mut closure_selector_vars = HashMap::new();
        let mut closure_roots = closure_selector_counts.into_keys().collect::<Vec<_>>();
        closure_roots.sort();
        for root in closure_roots {
            let variable = Variable::from_u32(variable_index as u32);
            variable_index += 1;
            builder.declare_var(variable, types::I64);
            let unset = builder.ins().iconst(types::I64, -1);
            builder.def_var(variable, unset);
            closure_selector_vars.insert(root, variable);
        }

        let mut writeback_locals = Vec::new();
        let mut mutable_param_indices = HashMap::new();
        let mut call_slot = 0usize;
        if function.receiver.is_some() {
            if function.receiver == Some(MirReceiverKind::BorrowMut) {
                mutable_param_indices.insert("self".to_string(), call_slot);
            }
            call_slot += 1;
        }
        for param in &function.params {
            if param.passing == MirReceiverKind::BorrowMut {
                mutable_param_indices.insert(param.name.clone(), call_slot);
            }
            call_slot += 1;
        }
        if function.receiver == Some(MirReceiverKind::BorrowMut) {
            let receiver_ty = receiver_type(function, &self.classes)?;
            writeback_locals.push(("self".to_string(), receiver_ty));
        }
        for param in &function.params {
            if param.passing == MirReceiverKind::BorrowMut {
                let ty = ensure_direct_type(
                    &param.ty,
                    &self.classes,
                    &format!("parameter `{}` on `{}`", param.name, function.name),
                )?;
                writeback_locals.push((param.name.clone(), ty));
            }
        }

        let mut function_refs = HashMap::new();
        for (name, func_id) in &self.functions {
            let func_ref = self.object.declare_func_in_func(*func_id, builder.func);
            function_refs.insert(name.clone(), func_ref);
        }
        let mut function_thunk_refs = HashMap::new();
        for (name, func_id) in &self.function_thunks {
            let func_ref = self.object.declare_func_in_func(*func_id, builder.func);
            function_thunk_refs.insert(name.clone(), func_ref);
        }
        let mut function_default_binder_refs = HashMap::new();
        for (name, func_id) in &self.function_default_binders {
            let func_ref = self.object.declare_func_in_func(*func_id, builder.func);
            function_default_binder_refs.insert(name.clone(), func_ref);
        }
        let mut cleanup_thunk_refs = HashMap::new();
        for place in &cleanup_places {
            if let Some(func_id) = self
                .cleanup_thunks
                .get(&(function.name.clone(), place.clone()))
            {
                let func_ref = self.object.declare_func_in_func(*func_id, builder.func);
                cleanup_thunk_refs.insert(place.clone(), func_ref);
            }
        }

        let print_i64 = self
            .object
            .declare_func_in_func(self.print_i64, builder.func);
        let print_u64 = self
            .object
            .declare_func_in_func(self.print_u64, builder.func);
        let print_f32 = self
            .object
            .declare_func_in_func(self.print_f32, builder.func);
        let print_f64 = self
            .object
            .declare_func_in_func(self.print_f64, builder.func);
        let print_bool = self
            .object
            .declare_func_in_func(self.print_bool, builder.func);
        let print_value = self
            .object
            .declare_func_in_func(self.print_value, builder.func);
        let sqrt_f64 = self
            .object
            .declare_func_in_func(self.sqrt_f64, builder.func);
        let assert_fail = self
            .object
            .declare_func_in_func(self.assert_fail, builder.func);
        let assert_fail_detailed = self
            .object
            .declare_func_in_func(self.assert_fail_detailed, builder.func);
        let fail_division_by_zero = self
            .object
            .declare_func_in_func(self.fail_division_by_zero, builder.func);
        let fail_int32_overflow = self
            .object
            .declare_func_in_func(self.fail_int32_overflow, builder.func);
        let fail_integer_overflow = self
            .object
            .declare_func_in_func(self.fail_integer_overflow, builder.func);
        let register_cleanup = self
            .object
            .declare_func_in_func(self.register_cleanup, builder.func);
        let unregister_cleanup = self
            .object
            .declare_func_in_func(self.unregister_cleanup, builder.func);
        let refresh_cleanup = self
            .object
            .declare_func_in_func(self.refresh_cleanup, builder.func);
        let set_next_mutable_sinks = self
            .object
            .declare_func_in_func(self.set_next_mutable_sinks, builder.func);
        let set_next_indirect_mutable_sinks = self
            .object
            .declare_func_in_func(self.set_next_indirect_mutable_sinks, builder.func);
        let current_mutable_sink = self
            .object
            .declare_func_in_func(self.current_mutable_sink, builder.func);
        let mutable_sink_new = self
            .object
            .declare_func_in_func(self.mutable_sink_new, builder.func);
        let mutable_sink_project = self
            .object
            .declare_func_in_func(self.mutable_sink_project, builder.func);
        let mutable_sink_store_owned = self
            .object
            .declare_func_in_func(self.mutable_sink_store_owned, builder.func);
        let mutable_sink_release = self
            .object
            .declare_func_in_func(self.mutable_sink_release, builder.func);
        let tag_value_type = self
            .object
            .declare_func_in_func(self.tag_value_type, builder.func);
        let box_i32 = self.object.declare_func_in_func(self.box_i32, builder.func);
        let box_i64 = self.object.declare_func_in_func(self.box_i64, builder.func);
        let box_u64 = self.object.declare_func_in_func(self.box_u64, builder.func);
        let box_uint_literal = self
            .object
            .declare_func_in_func(self.box_uint_literal, builder.func);
        let box_f64 = self.object.declare_func_in_func(self.box_f64, builder.func);
        let box_bool = self
            .object
            .declare_func_in_func(self.box_bool, builder.func);
        let function_value = self
            .object
            .declare_func_in_func(self.function_value, builder.func);
        let module_constant = self
            .object
            .declare_func_in_func(self.module_constant, builder.func);
        let closure_value = self
            .object
            .declare_func_in_func(self.closure_value, builder.func);
        let closure_capture = self
            .object
            .declare_func_in_func(self.closure_capture, builder.func);
        let function_call = self
            .object
            .declare_func_in_func(self.function_call, builder.func);
        let function_bind_defaults = self
            .object
            .declare_func_in_func(self.function_bind_defaults, builder.func);
        let box_unit = self
            .object
            .declare_func_in_func(self.box_unit, builder.func);
        let string_literal = self
            .object
            .declare_func_in_func(self.string_literal, builder.func);
        let string_len = self
            .object
            .declare_func_in_func(self.string_len, builder.func);
        let string_byte_len = self
            .object
            .declare_func_in_func(self.string_byte_len, builder.func);
        let string_slice = self
            .object
            .declare_func_in_func(self.string_slice, builder.func);
        let string_contains = self
            .object
            .declare_func_in_func(self.string_contains, builder.func);
        let string_starts_with = self
            .object
            .declare_func_in_func(self.string_starts_with, builder.func);
        let string_ends_with = self
            .object
            .declare_func_in_func(self.string_ends_with, builder.func);
        let string_split = self
            .object
            .declare_func_in_func(self.string_split, builder.func);
        let string_replace = self
            .object
            .declare_func_in_func(self.string_replace, builder.func);
        let string_to_lower = self
            .object
            .declare_func_in_func(self.string_to_lower, builder.func);
        let string_to_upper = self
            .object
            .declare_func_in_func(self.string_to_upper, builder.func);
        let string_strip_prefix = self
            .object
            .declare_func_in_func(self.string_strip_prefix, builder.func);
        let string_strip_suffix = self
            .object
            .declare_func_in_func(self.string_strip_suffix, builder.func);
        let string_trim = self
            .object
            .declare_func_in_func(self.string_trim, builder.func);
        let string_join = self
            .object
            .declare_func_in_func(self.string_join, builder.func);
        let stringify_value = self
            .object
            .declare_func_in_func(self.stringify_value, builder.func);
        let format_value = self
            .object
            .declare_func_in_func(self.format_value, builder.func);
        let abs_value = self
            .object
            .declare_func_in_func(self.abs_value, builder.func);
        let min_value = self
            .object
            .declare_func_in_func(self.min_value, builder.func);
        let max_value = self
            .object
            .declare_func_in_func(self.max_value, builder.func);
        let sqrt_value = self
            .object
            .declare_func_in_func(self.sqrt_value, builder.func);
        let round_value = self
            .object
            .declare_func_in_func(self.round_value, builder.func);
        let divmod_value = self
            .object
            .declare_func_in_func(self.divmod_value, builder.func);
        let parse_int32 = self
            .object
            .declare_func_in_func(self.parse_int32, builder.func);
        let parse_int64 = self
            .object
            .declare_func_in_func(self.parse_int64, builder.func);
        let parse_float64 = self
            .object
            .declare_func_in_func(self.parse_float64, builder.func);
        let duration_literal = self
            .object
            .declare_func_in_func(self.duration_literal, builder.func);
        let duration_from_i64 = self
            .object
            .declare_func_in_func(self.duration_from_i64, builder.func);
        let duration_to_float = self
            .object
            .declare_func_in_func(self.duration_to_float, builder.func);
        let rng_new = self.object.declare_func_in_func(self.rng_new, builder.func);
        let rng_next_int = self
            .object
            .declare_func_in_func(self.rng_next_int, builder.func);
        let rng_next_float = self
            .object
            .declare_func_in_func(self.rng_next_float, builder.func);
        let rng_shuffle = self
            .object
            .declare_func_in_func(self.rng_shuffle, builder.func);
        let random_secure_int = self
            .object
            .declare_func_in_func(self.random_secure_int, builder.func);
        let random_secure_bytes = self
            .object
            .declare_func_in_func(self.random_secure_bytes, builder.func);
        let range_new = self
            .object
            .declare_func_in_func(self.range_new, builder.func);
        let range_current = self
            .object
            .declare_func_in_func(self.range_current, builder.func);
        let range_end = self
            .object
            .declare_func_in_func(self.range_end, builder.func);
        let range_advance = self
            .object
            .declare_func_in_func(self.range_advance, builder.func);
        let vec_empty = self
            .object
            .declare_func_in_func(self.vec_empty, builder.func);
        let vec_len = self.object.declare_func_in_func(self.vec_len, builder.func);
        let vec_is_empty = self
            .object
            .declare_func_in_func(self.vec_is_empty, builder.func);
        let vec_push_in_place = self
            .object
            .declare_func_in_func(self.vec_push_in_place, builder.func);
        let vec_pop_in_place = self
            .object
            .declare_func_in_func(self.vec_pop_in_place, builder.func);
        let vec_get = self.object.declare_func_in_func(self.vec_get, builder.func);
        let vec_set_in_place = self
            .object
            .declare_func_in_func(self.vec_set_in_place, builder.func);
        let vec_remove_in_place = self
            .object
            .declare_func_in_func(self.vec_remove_in_place, builder.func);
        let vec_swap_in_place = self
            .object
            .declare_func_in_func(self.vec_swap_in_place, builder.func);
        let vec_contains = self
            .object
            .declare_func_in_func(self.vec_contains, builder.func);
        let vec_extend_in_place = self
            .object
            .declare_func_in_func(self.vec_extend_in_place, builder.func);
        let vec_insert_in_place = self
            .object
            .declare_func_in_func(self.vec_insert_in_place, builder.func);
        let vec_clear_in_place = self
            .object
            .declare_func_in_func(self.vec_clear_in_place, builder.func);
        let vec_reverse_in_place = self
            .object
            .declare_func_in_func(self.vec_reverse_in_place, builder.func);
        let collection_operation = self
            .object
            .declare_func_in_func(self.collection_operation, builder.func);
        let vec_index = self
            .object
            .declare_func_in_func(self.vec_index, builder.func);
        let vec_slice = self
            .object
            .declare_func_in_func(self.vec_slice, builder.func);
        let vec_index_option = self
            .object
            .declare_func_in_func(self.vec_index_option, builder.func);
        let vec_take_index_in_place = self
            .object
            .declare_func_in_func(self.vec_take_index_in_place, builder.func);
        let vec_set_index_in_place = self
            .object
            .declare_func_in_func(self.vec_set_index_in_place, builder.func);
        let array_zeros = self
            .object
            .declare_func_in_func(self.array_zeros, builder.func);
        let array_full = self
            .object
            .declare_func_in_func(self.array_full, builder.func);
        let array_from_vec = self
            .object
            .declare_func_in_func(self.array_from_vec, builder.func);
        let array_clone = self
            .object
            .declare_func_in_func(self.array_clone, builder.func);
        let array_shape = self
            .object
            .declare_func_in_func(self.array_shape, builder.func);
        let array_len = self
            .object
            .declare_func_in_func(self.array_len, builder.func);
        let array_get = self
            .object
            .declare_func_in_func(self.array_get, builder.func);
        let array_set_in_place = self
            .object
            .declare_func_in_func(self.array_set_in_place, builder.func);
        let array_fill_in_place = self
            .object
            .declare_func_in_func(self.array_fill_in_place, builder.func);
        let array_index = self
            .object
            .declare_func_in_func(self.array_index, builder.func);
        let array_set_index_in_place = self
            .object
            .declare_func_in_func(self.array_set_index_in_place, builder.func);
        let array_slice = self
            .object
            .declare_func_in_func(self.array_slice, builder.func);
        let array_binary = self
            .object
            .declare_func_in_func(self.array_binary, builder.func);
        let array_map = self
            .object
            .declare_func_in_func(self.array_map, builder.func);
        let array_reduce = self
            .object
            .declare_func_in_func(self.array_reduce, builder.func);
        let map_empty = self
            .object
            .declare_func_in_func(self.map_empty, builder.func);
        let map_len = self.object.declare_func_in_func(self.map_len, builder.func);
        let map_is_empty = self
            .object
            .declare_func_in_func(self.map_is_empty, builder.func);
        let map_get = self.object.declare_func_in_func(self.map_get, builder.func);
        let map_set_in_place = self
            .object
            .declare_func_in_func(self.map_set_in_place, builder.func);
        let map_remove_in_place = self
            .object
            .declare_func_in_func(self.map_remove_in_place, builder.func);
        let map_contains_key = self
            .object
            .declare_func_in_func(self.map_contains_key, builder.func);
        let map_keys = self
            .object
            .declare_func_in_func(self.map_keys, builder.func);
        let map_values = self
            .object
            .declare_func_in_func(self.map_values, builder.func);
        let map_items = self
            .object
            .declare_func_in_func(self.map_items, builder.func);
        let map_clear_in_place = self
            .object
            .declare_func_in_func(self.map_clear_in_place, builder.func);
        let map_extend_in_place = self
            .object
            .declare_func_in_func(self.map_extend_in_place, builder.func);
        let map_index = self
            .object
            .declare_func_in_func(self.map_index, builder.func);
        let map_set_index_in_place = self
            .object
            .declare_func_in_func(self.map_set_index_in_place, builder.func);
        let set_empty = self
            .object
            .declare_func_in_func(self.set_empty, builder.func);
        let set_len = self.object.declare_func_in_func(self.set_len, builder.func);
        let set_is_empty = self
            .object
            .declare_func_in_func(self.set_is_empty, builder.func);
        let set_contains = self
            .object
            .declare_func_in_func(self.set_contains, builder.func);
        let set_insert_in_place = self
            .object
            .declare_func_in_func(self.set_insert_in_place, builder.func);
        let set_remove_in_place = self
            .object
            .declare_func_in_func(self.set_remove_in_place, builder.func);
        let set_index_option = self
            .object
            .declare_func_in_func(self.set_index_option, builder.func);
        let set_take_index_in_place = self
            .object
            .declare_func_in_func(self.set_take_index_in_place, builder.func);
        let retain_value = self
            .object
            .declare_func_in_func(self.retain_value, builder.func);
        let release_value = self
            .object
            .declare_func_in_func(self.release_value, builder.func);
        let clone_value = self
            .object
            .declare_func_in_func(self.clone_value, builder.func);
        let unbox_i64 = self
            .object
            .declare_func_in_func(self.unbox_i64, builder.func);
        let unbox_int64 = self
            .object
            .declare_func_in_func(self.unbox_int64, builder.func);
        let integer_to_float = self
            .object
            .declare_func_in_func(self.integer_to_float, builder.func);
        let integer_width_binary = self
            .object
            .declare_func_in_func(self.integer_width_binary, builder.func);
        let unbox_u64 = self
            .object
            .declare_func_in_func(self.unbox_u64, builder.func);
        let unbox_f64 = self
            .object
            .declare_func_in_func(self.unbox_f64, builder.func);
        let unbox_bool = self
            .object
            .declare_func_in_func(self.unbox_bool, builder.func);
        let value_as_condition = self
            .object
            .declare_func_in_func(self.value_as_condition, builder.func);
        let unary_value = self
            .object
            .declare_func_in_func(self.unary_value, builder.func);
        let binary_value = self
            .object
            .declare_func_in_func(self.binary_value, builder.func);
        let cast_value = self
            .object
            .declare_func_in_func(self.cast_value, builder.func);
        let cast_integer_to_integer = self
            .object
            .declare_func_in_func(self.cast_integer_to_integer, builder.func);
        let cast_integer_to_float = self
            .object
            .declare_func_in_func(self.cast_integer_to_float, builder.func);
        let cast_float_to_integer = self
            .object
            .declare_func_in_func(self.cast_float_to_integer, builder.func);
        let value_type_matches = self
            .object
            .declare_func_in_func(self.value_type_matches, builder.func);
        let value_has_runtime_type = self
            .object
            .declare_func_in_func(self.value_has_runtime_type, builder.func);
        let tuple_new = self
            .object
            .declare_func_in_func(self.tuple_new, builder.func);
        let tuple_element = self
            .object
            .declare_func_in_func(self.tuple_element, builder.func);
        let tuple_take_element = self
            .object
            .declare_func_in_func(self.tuple_take_element, builder.func);
        let enum_variant = self
            .object
            .declare_func_in_func(self.enum_variant, builder.func);
        let variant_matches = self
            .object
            .declare_func_in_func(self.variant_matches, builder.func);
        let variant_payload = self
            .object
            .declare_func_in_func(self.variant_payload, builder.func);
        let variant_take_payload = self
            .object
            .declare_func_in_func(self.variant_take_payload, builder.func);
        let instance_empty = self
            .object
            .declare_func_in_func(self.instance_empty, builder.func);
        let instance_get_field = self
            .object
            .declare_func_in_func(self.instance_get_field, builder.func);
        let instance_take_field = self
            .object
            .declare_func_in_func(self.instance_take_field, builder.func);
        let instance_set_field_owned = self
            .object
            .declare_func_in_func(self.instance_set_field_owned, builder.func);
        let arg_buffer_new = self
            .object
            .declare_func_in_func(self.arg_buffer_new, builder.func);
        let arg_buffer_store = self
            .object
            .declare_func_in_func(self.arg_buffer_store, builder.func);
        let arg_buffer_store_owned = self
            .object
            .declare_func_in_func(self.arg_buffer_store_owned, builder.func);
        let task_arg_buffer_guard = self
            .object
            .declare_func_in_func(self.task_arg_buffer_guard, builder.func);
        let task_arg_buffer_disarm = self
            .object
            .declare_func_in_func(self.task_arg_buffer_disarm, builder.func);
        let host_builtin = self
            .object
            .declare_func_in_func(self.host_builtin, builder.func);
        let ffi_call = self
            .object
            .declare_func_in_func(self.ffi_call, builder.func);
        let monotonic_time_ms = self
            .object
            .declare_func_in_func(self.monotonic_time_ms, builder.func);
        let channel_new = self
            .object
            .declare_func_in_func(self.channel_new, builder.func);
        let channel_send = self
            .object
            .declare_func_in_func(self.channel_send, builder.func);
        let channel_send_timeout_value = self
            .object
            .declare_func_in_func(self.channel_send_timeout_value, builder.func);
        let channel_try_send = self
            .object
            .declare_func_in_func(self.channel_try_send, builder.func);
        let channel_recv = self
            .object
            .declare_func_in_func(self.channel_recv, builder.func);
        let channel_recv_in_task_group = self
            .object
            .declare_func_in_func(self.channel_recv_in_task_group, builder.func);
        let channel_recv_with_registered_producers = self
            .object
            .declare_func_in_func(self.channel_recv_with_registered_producers, builder.func);
        let channel_recv_timeout_value = self
            .object
            .declare_func_in_func(self.channel_recv_timeout_value, builder.func);
        let channel_recv_or_none = self
            .object
            .declare_func_in_func(self.channel_recv_or_none, builder.func);
        let channel_recv_or_none_timeout_value = self
            .object
            .declare_func_in_func(self.channel_recv_or_none_timeout_value, builder.func);
        let channel_recv_or_value = self
            .object
            .declare_func_in_func(self.channel_recv_or_value, builder.func);
        let channel_recv_or_value_timeout_value = self
            .object
            .declare_func_in_func(self.channel_recv_or_value_timeout_value, builder.func);
        let channel_close = self
            .object
            .declare_func_in_func(self.channel_close, builder.func);
        let task_group_new = self
            .object
            .declare_func_in_func(self.task_group_new, builder.func);
        let task_group_cancel = self
            .object
            .declare_func_in_func(self.task_group_cancel, builder.func);
        let task_group_close = self
            .object
            .declare_func_in_func(self.task_group_close, builder.func);
        let task_join = self
            .object
            .declare_func_in_func(self.task_join, builder.func);
        let task_join_timeout_value = self
            .object
            .declare_func_in_func(self.task_join_timeout_value, builder.func);
        let task_join_or_none = self
            .object
            .declare_func_in_func(self.task_join_or_none, builder.func);
        let task_join_or_none_timeout_value = self
            .object
            .declare_func_in_func(self.task_join_or_none_timeout_value, builder.func);
        let task_join_or_value = self
            .object
            .declare_func_in_func(self.task_join_or_value, builder.func);
        let task_join_or_value_timeout_value = self
            .object
            .declare_func_in_func(self.task_join_or_value_timeout_value, builder.func);
        let wait_any = self
            .object
            .declare_func_in_func(self.wait_any, builder.func);
        let wait_any_timeout_value = self
            .object
            .declare_func_in_func(self.wait_any_timeout_value, builder.func);
        let wait_all = self
            .object
            .declare_func_in_func(self.wait_all, builder.func);
        let wait_all_timeout_value = self
            .object
            .declare_func_in_func(self.wait_all_timeout_value, builder.func);
        let select = self.object.declare_func_in_func(self.select, builder.func);
        let io_write = self
            .object
            .declare_func_in_func(self.io_write, builder.func);
        let io_flush = self
            .object
            .declare_func_in_func(self.io_flush, builder.func);
        let io_read_line = self
            .object
            .declare_func_in_func(self.io_read_line, builder.func);
        let fs_exists = self
            .object
            .declare_func_in_func(self.fs_exists, builder.func);
        let fs_read_to_string = self
            .object
            .declare_func_in_func(self.fs_read_to_string, builder.func);
        let fs_read_bytes = self
            .object
            .declare_func_in_func(self.fs_read_bytes, builder.func);
        let fs_write_string = self
            .object
            .declare_func_in_func(self.fs_write_string, builder.func);
        let fs_write_bytes = self
            .object
            .declare_func_in_func(self.fs_write_bytes, builder.func);
        let fs_append_string = self
            .object
            .declare_func_in_func(self.fs_append_string, builder.func);
        let fs_append_bytes = self
            .object
            .declare_func_in_func(self.fs_append_bytes, builder.func);
        let fs_create_dir = self
            .object
            .declare_func_in_func(self.fs_create_dir, builder.func);
        let fs_read_dir = self
            .object
            .declare_func_in_func(self.fs_read_dir, builder.func);
        let fs_remove_file = self
            .object
            .declare_func_in_func(self.fs_remove_file, builder.func);
        let fs_open = self.object.declare_func_in_func(self.fs_open, builder.func);
        let fs_create = self
            .object
            .declare_func_in_func(self.fs_create, builder.func);
        let fs_append = self
            .object
            .declare_func_in_func(self.fs_append, builder.func);
        let file_read_all = self
            .object
            .declare_func_in_func(self.file_read_all, builder.func);
        let file_read_bytes = self
            .object
            .declare_func_in_func(self.file_read_bytes, builder.func);
        let file_write_all = self
            .object
            .declare_func_in_func(self.file_write_all, builder.func);
        let file_write_bytes = self
            .object
            .declare_func_in_func(self.file_write_bytes, builder.func);
        let file_flush = self
            .object
            .declare_func_in_func(self.file_flush, builder.func);
        let file_close = self
            .object
            .declare_func_in_func(self.file_close, builder.func);
        let process_inherit = self
            .object
            .declare_func_in_func(self.process_inherit, builder.func);
        let process_null = self
            .object
            .declare_func_in_func(self.process_null, builder.func);
        let process_pipe = self
            .object
            .declare_func_in_func(self.process_pipe, builder.func);
        let process_supervisor = self
            .object
            .declare_func_in_func(self.process_supervisor, builder.func);
        let process_start = self
            .object
            .declare_func_in_func(self.process_start, builder.func);
        let process_run = self
            .object
            .declare_func_in_func(self.process_run, builder.func);
        let process_child_stdin = self
            .object
            .declare_func_in_func(self.process_child_stdin, builder.func);
        let process_child_stdout = self
            .object
            .declare_func_in_func(self.process_child_stdout, builder.func);
        let process_child_stderr = self
            .object
            .declare_func_in_func(self.process_child_stderr, builder.func);
        let process_child_wait = self
            .object
            .declare_func_in_func(self.process_child_wait, builder.func);
        let process_child_wait_or_none = self
            .object
            .declare_func_in_func(self.process_child_wait_or_none, builder.func);
        let process_child_wait_ok = self
            .object
            .declare_func_in_func(self.process_child_wait_ok, builder.func);
        let process_child_kill = self
            .object
            .declare_func_in_func(self.process_child_kill, builder.func);
        let process_child_terminate = self
            .object
            .declare_func_in_func(self.process_child_terminate, builder.func);
        let process_child_close = self
            .object
            .declare_func_in_func(self.process_child_close, builder.func);
        let process_pipe_read_all = self
            .object
            .declare_func_in_func(self.process_pipe_read_all, builder.func);
        let process_pipe_read_line = self
            .object
            .declare_func_in_func(self.process_pipe_read_line, builder.func);
        let process_pipe_read_bytes = self
            .object
            .declare_func_in_func(self.process_pipe_read_bytes, builder.func);
        let process_pipe_write_all = self
            .object
            .declare_func_in_func(self.process_pipe_write_all, builder.func);
        let process_pipe_write_bytes = self
            .object
            .declare_func_in_func(self.process_pipe_write_bytes, builder.func);
        let process_pipe_flush = self
            .object
            .declare_func_in_func(self.process_pipe_flush, builder.func);
        let process_pipe_close = self
            .object
            .declare_func_in_func(self.process_pipe_close, builder.func);
        let process_completed_status = self
            .object
            .declare_func_in_func(self.process_completed_status, builder.func);
        let process_completed_success = self
            .object
            .declare_func_in_func(self.process_completed_success, builder.func);
        let process_completed_stdout = self
            .object
            .declare_func_in_func(self.process_completed_stdout, builder.func);
        let process_completed_stderr = self
            .object
            .declare_func_in_func(self.process_completed_stderr, builder.func);
        let process_completed_stdout_bytes = self
            .object
            .declare_func_in_func(self.process_completed_stdout_bytes, builder.func);
        let process_completed_stderr_bytes = self
            .object
            .declare_func_in_func(self.process_completed_stderr_bytes, builder.func);
        let process_completed_check = self
            .object
            .declare_func_in_func(self.process_completed_check, builder.func);
        let process_supervisor_start = self
            .object
            .declare_func_in_func(self.process_supervisor_start, builder.func);
        let process_supervisor_wait = self
            .object
            .declare_func_in_func(self.process_supervisor_wait, builder.func);
        let process_supervisor_wait_or_none = self
            .object
            .declare_func_in_func(self.process_supervisor_wait_or_none, builder.func);
        let process_supervisor_stop = self
            .object
            .declare_func_in_func(self.process_supervisor_stop, builder.func);
        let process_supervisor_is_empty = self
            .object
            .declare_func_in_func(self.process_supervisor_is_empty, builder.func);
        let process_supervisor_close = self
            .object
            .declare_func_in_func(self.process_supervisor_close, builder.func);
        let net_connect = self
            .object
            .declare_func_in_func(self.net_connect, builder.func);
        let net_connect_timeout = self
            .object
            .declare_func_in_func(self.net_connect_timeout, builder.func);
        let net_listen = self
            .object
            .declare_func_in_func(self.net_listen, builder.func);
        let net_udp_bind = self
            .object
            .declare_func_in_func(self.net_udp_bind, builder.func);
        let net_unix_listen = self
            .object
            .declare_func_in_func(self.net_unix_listen, builder.func);
        let net_unix_connect = self
            .object
            .declare_func_in_func(self.net_unix_connect, builder.func);
        let net_unix_connect_timeout = self
            .object
            .declare_func_in_func(self.net_unix_connect_timeout, builder.func);
        let net_tls_listen = self
            .object
            .declare_func_in_func(self.net_tls_listen, builder.func);
        let net_tls_connect = self
            .object
            .declare_func_in_func(self.net_tls_connect, builder.func);
        let net_tls_connect_timeout = self
            .object
            .declare_func_in_func(self.net_tls_connect_timeout, builder.func);
        let net_http_listen = self
            .object
            .declare_func_in_func(self.net_http_listen, builder.func);
        let net_http_request_text = self
            .object
            .declare_func_in_func(self.net_http_request_text, builder.func);
        let net_http_request_text_timeout = self
            .object
            .declare_func_in_func(self.net_http_request_text_timeout, builder.func);
        let net_http_request_bytes = self
            .object
            .declare_func_in_func(self.net_http_request_bytes, builder.func);
        let net_http_request_bytes_timeout = self
            .object
            .declare_func_in_func(self.net_http_request_bytes_timeout, builder.func);
        let net_websocket_listen = self
            .object
            .declare_func_in_func(self.net_websocket_listen, builder.func);
        let net_websocket_connect = self
            .object
            .declare_func_in_func(self.net_websocket_connect, builder.func);
        let net_websocket_connect_timeout = self
            .object
            .declare_func_in_func(self.net_websocket_connect_timeout, builder.func);
        let tcp_listener_accept = self
            .object
            .declare_func_in_func(self.tcp_listener_accept, builder.func);
        let tcp_listener_local_addr = self
            .object
            .declare_func_in_func(self.tcp_listener_local_addr, builder.func);
        let tcp_listener_close = self
            .object
            .declare_func_in_func(self.tcp_listener_close, builder.func);
        let tcp_stream_read_all = self
            .object
            .declare_func_in_func(self.tcp_stream_read_all, builder.func);
        let tcp_stream_read_line = self
            .object
            .declare_func_in_func(self.tcp_stream_read_line, builder.func);
        let tcp_stream_read_bytes = self
            .object
            .declare_func_in_func(self.tcp_stream_read_bytes, builder.func);
        let tcp_stream_read_exact = self
            .object
            .declare_func_in_func(self.tcp_stream_read_exact, builder.func);
        let tcp_stream_write_all = self
            .object
            .declare_func_in_func(self.tcp_stream_write_all, builder.func);
        let tcp_stream_write_bytes = self
            .object
            .declare_func_in_func(self.tcp_stream_write_bytes, builder.func);
        let tcp_stream_flush = self
            .object
            .declare_func_in_func(self.tcp_stream_flush, builder.func);
        let tcp_stream_local_addr = self
            .object
            .declare_func_in_func(self.tcp_stream_local_addr, builder.func);
        let tcp_stream_peer_addr = self
            .object
            .declare_func_in_func(self.tcp_stream_peer_addr, builder.func);
        let tcp_stream_shutdown_read = self
            .object
            .declare_func_in_func(self.tcp_stream_shutdown_read, builder.func);
        let tcp_stream_shutdown_write = self
            .object
            .declare_func_in_func(self.tcp_stream_shutdown_write, builder.func);
        let tcp_stream_shutdown_both = self
            .object
            .declare_func_in_func(self.tcp_stream_shutdown_both, builder.func);
        let tcp_stream_close = self
            .object
            .declare_func_in_func(self.tcp_stream_close, builder.func);
        let udp_socket_send_text = self
            .object
            .declare_func_in_func(self.udp_socket_send_text, builder.func);
        let udp_socket_send_bytes = self
            .object
            .declare_func_in_func(self.udp_socket_send_bytes, builder.func);
        let udp_socket_recv = self
            .object
            .declare_func_in_func(self.udp_socket_recv, builder.func);
        let udp_socket_recv_from = self
            .object
            .declare_func_in_func(self.udp_socket_recv_from, builder.func);
        let udp_socket_local_addr = self
            .object
            .declare_func_in_func(self.udp_socket_local_addr, builder.func);
        let udp_socket_peer_addr = self
            .object
            .declare_func_in_func(self.udp_socket_peer_addr, builder.func);
        let udp_socket_close = self
            .object
            .declare_func_in_func(self.udp_socket_close, builder.func);
        let udp_datagram_address = self
            .object
            .declare_func_in_func(self.udp_datagram_address, builder.func);
        let udp_datagram_bytes = self
            .object
            .declare_func_in_func(self.udp_datagram_bytes, builder.func);
        let udp_datagram_text = self
            .object
            .declare_func_in_func(self.udp_datagram_text, builder.func);
        let http_listener_accept = self
            .object
            .declare_func_in_func(self.http_listener_accept, builder.func);
        let http_listener_local_addr = self
            .object
            .declare_func_in_func(self.http_listener_local_addr, builder.func);
        let http_listener_close = self
            .object
            .declare_func_in_func(self.http_listener_close, builder.func);
        let http_exchange_method = self
            .object
            .declare_func_in_func(self.http_exchange_method, builder.func);
        let http_exchange_path = self
            .object
            .declare_func_in_func(self.http_exchange_path, builder.func);
        let http_exchange_headers = self
            .object
            .declare_func_in_func(self.http_exchange_headers, builder.func);
        let http_exchange_body_text = self
            .object
            .declare_func_in_func(self.http_exchange_body_text, builder.func);
        let http_exchange_body_bytes = self
            .object
            .declare_func_in_func(self.http_exchange_body_bytes, builder.func);
        let http_exchange_respond_text = self
            .object
            .declare_func_in_func(self.http_exchange_respond_text, builder.func);
        let http_exchange_respond_bytes = self
            .object
            .declare_func_in_func(self.http_exchange_respond_bytes, builder.func);
        let http_response_status = self
            .object
            .declare_func_in_func(self.http_response_status, builder.func);
        let http_response_reason = self
            .object
            .declare_func_in_func(self.http_response_reason, builder.func);
        let http_response_headers = self
            .object
            .declare_func_in_func(self.http_response_headers, builder.func);
        let http_response_text = self
            .object
            .declare_func_in_func(self.http_response_text, builder.func);
        let http_response_bytes = self
            .object
            .declare_func_in_func(self.http_response_bytes, builder.func);
        let websocket_listener_accept = self
            .object
            .declare_func_in_func(self.websocket_listener_accept, builder.func);
        let websocket_listener_local_addr = self
            .object
            .declare_func_in_func(self.websocket_listener_local_addr, builder.func);
        let websocket_send_text = self
            .object
            .declare_func_in_func(self.websocket_send_text, builder.func);
        let websocket_send_bytes = self
            .object
            .declare_func_in_func(self.websocket_send_bytes, builder.func);
        let websocket_recv_text = self
            .object
            .declare_func_in_func(self.websocket_recv_text, builder.func);
        let websocket_recv_bytes = self
            .object
            .declare_func_in_func(self.websocket_recv_bytes, builder.func);
        let websocket_close = self
            .object
            .declare_func_in_func(self.websocket_close, builder.func);
        let unix_listener_accept = self
            .object
            .declare_func_in_func(self.unix_listener_accept, builder.func);
        let unix_listener_close = self
            .object
            .declare_func_in_func(self.unix_listener_close, builder.func);
        let unix_stream_read_line = self
            .object
            .declare_func_in_func(self.unix_stream_read_line, builder.func);
        let unix_stream_read_exact = self
            .object
            .declare_func_in_func(self.unix_stream_read_exact, builder.func);
        let unix_stream_write_all = self
            .object
            .declare_func_in_func(self.unix_stream_write_all, builder.func);
        let unix_stream_close = self
            .object
            .declare_func_in_func(self.unix_stream_close, builder.func);
        let tls_listener_accept = self
            .object
            .declare_func_in_func(self.tls_listener_accept, builder.func);
        let tls_listener_local_addr = self
            .object
            .declare_func_in_func(self.tls_listener_local_addr, builder.func);
        let tls_listener_close = self
            .object
            .declare_func_in_func(self.tls_listener_close, builder.func);
        let tls_stream_read_line = self
            .object
            .declare_func_in_func(self.tls_stream_read_line, builder.func);
        let tls_stream_read_exact = self
            .object
            .declare_func_in_func(self.tls_stream_read_exact, builder.func);
        let tls_stream_write_all = self
            .object
            .declare_func_in_func(self.tls_stream_write_all, builder.func);
        let tls_stream_close = self
            .object
            .declare_func_in_func(self.tls_stream_close, builder.func);
        let cancelled = self
            .object
            .declare_func_in_func(self.cancelled, builder.func);
        let yield_now = self
            .object
            .declare_func_in_func(self.yield_now, builder.func);
        let sleep_value_void = self
            .object
            .declare_func_in_func(self.sleep_value_void, builder.func);
        let start_task_call = self
            .object
            .declare_func_in_func(self.start_task_call, builder.func);
        let function_frame_metadata = self
            .module
            .functions
            .iter()
            .chain(self.module.top_level.iter())
            .map(|candidate| {
                (
                    candidate.name.clone(),
                    (
                        candidate
                            .source_path
                            .clone()
                            .unwrap_or_else(|| self.program_path.clone()),
                        candidate.span,
                    ),
                )
            })
            .collect();
        let current_function_name = public_direct_function_name(&function.name);
        let current_function_path = function
            .source_path
            .clone()
            .unwrap_or_else(|| self.program_path.clone());

        let mut compiler = FunctionCompiler {
            builder,
            blocks,
            variables,
            variable_types,
            next_variable_index: variable_index,
            function_refs,
            function_thunk_refs,
            function_default_binder_refs,
            cleanup_thunk_refs,
            function_return_types: self.function_return_types.clone(),
            function_param_types: self.function_param_types.clone(),
            function_writeback_types: self.function_writeback_types.clone(),
            function_frame_metadata,
            current_function_name,
            current_function_path,
            writeback_locals,
            mutable_param_indices,
            classes: self.classes.clone(),
            trait_impls: self.trait_impls.clone(),
            return_type: function.return_type.clone(),
            owned_opaque_temporaries: HashSet::new(),
            view_places: HashMap::new(),
            view_selector_vars,
            view_selector_tags,
            closure_selector_vars,
            closure_selector_tags,
            closure_capture_writebacks: HashMap::new(),
            object: &mut self.object,
            string_data: &mut self.string_data,
            cleanup_places,
            cleanup_active_vars,
            cleanup_registration_vars,
            safepoint_fuel,
            exit_call,
            set_returned_view_projection,
            take_returned_view_projection,
            print_i64,
            print_u64,
            print_f32,
            print_f64,
            print_bool,
            print_value,
            sqrt_f64,
            assert_fail,
            assert_fail_detailed,
            fail_division_by_zero,
            fail_int32_overflow,
            fail_integer_overflow,
            register_cleanup,
            unregister_cleanup,
            refresh_cleanup,
            set_next_mutable_sinks,
            set_next_indirect_mutable_sinks,
            current_mutable_sink,
            mutable_sink_new,
            mutable_sink_project,
            mutable_sink_store_owned,
            mutable_sink_release,
            tag_value_type,
            box_i32,
            box_i64,
            box_u64,
            box_uint_literal,
            box_f64,
            box_bool,
            function_value,
            module_constant,
            closure_value,
            closure_capture,
            function_call,
            function_bind_defaults,
            box_unit,
            string_literal,
            string_len,
            string_byte_len,
            string_slice,
            string_contains,
            string_starts_with,
            string_ends_with,
            string_split,
            string_replace,
            string_to_lower,
            string_to_upper,
            string_strip_prefix,
            string_strip_suffix,
            string_trim,
            string_join,
            stringify_value,
            format_value,
            abs_value,
            min_value,
            max_value,
            sqrt_value,
            round_value,
            divmod_value,
            parse_int32,
            parse_int64,
            parse_float64,
            duration_literal,
            duration_from_i64,
            duration_to_float,
            rng_new,
            rng_next_int,
            rng_next_float,
            rng_shuffle,
            random_secure_int,
            random_secure_bytes,
            range_new,
            range_current,
            range_end,
            range_advance,
            vec_empty,
            vec_len,
            vec_is_empty,
            vec_push_in_place,
            vec_pop_in_place,
            vec_get,
            vec_set_in_place,
            vec_remove_in_place,
            vec_swap_in_place,
            vec_contains,
            vec_extend_in_place,
            vec_insert_in_place,
            vec_clear_in_place,
            vec_reverse_in_place,
            collection_operation,
            vec_index,
            vec_slice,
            vec_index_option,
            vec_take_index_in_place,
            vec_set_index_in_place,
            array_zeros,
            array_full,
            array_from_vec,
            array_clone,
            array_shape,
            array_len,
            array_get,
            array_set_in_place,
            array_fill_in_place,
            array_index,
            array_set_index_in_place,
            array_slice,
            array_binary,
            array_map,
            array_reduce,
            map_empty,
            map_len,
            map_is_empty,
            map_get,
            map_set_in_place,
            map_remove_in_place,
            map_contains_key,
            map_keys,
            map_values,
            map_items,
            map_clear_in_place,
            map_extend_in_place,
            map_index,
            map_set_index_in_place,
            set_empty,
            set_len,
            set_is_empty,
            set_contains,
            set_insert_in_place,
            set_remove_in_place,
            set_index_option,
            set_take_index_in_place,
            retain_value,
            release_value,
            clone_value,
            unbox_i64,
            unbox_int64,
            integer_to_float,
            integer_width_binary,
            unbox_u64,
            unbox_f64,
            unbox_bool,
            value_as_condition,
            unary_value,
            binary_value,
            cast_value,
            cast_integer_to_integer,
            cast_integer_to_float,
            cast_float_to_integer,
            value_type_matches,
            value_has_runtime_type,
            tuple_new,
            tuple_element,
            tuple_take_element,
            enum_variant,
            variant_matches,
            variant_payload,
            variant_take_payload,
            instance_empty,
            instance_get_field,
            instance_take_field,
            instance_set_field_owned,
            arg_buffer_new,
            arg_buffer_store,
            arg_buffer_store_owned,
            task_arg_buffer_guard,
            task_arg_buffer_disarm,
            host_builtin,
            ffi_call,
            monotonic_time_ms,
            channel_new,
            channel_send,
            channel_send_timeout_value,
            channel_try_send,
            channel_recv,
            channel_recv_in_task_group,
            channel_recv_with_registered_producers,
            channel_recv_timeout_value,
            channel_recv_or_none,
            channel_recv_or_none_timeout_value,
            channel_recv_or_value,
            channel_recv_or_value_timeout_value,
            channel_close,
            task_group_new,
            task_group_cancel,
            task_group_close,
            task_join,
            task_join_timeout_value,
            task_join_or_none,
            task_join_or_none_timeout_value,
            task_join_or_value,
            task_join_or_value_timeout_value,
            wait_any,
            wait_any_timeout_value,
            wait_all,
            wait_all_timeout_value,
            select,
            io_write,
            io_flush,
            io_read_line,
            fs_exists,
            fs_read_to_string,
            fs_read_bytes,
            fs_write_string,
            fs_write_bytes,
            fs_append_string,
            fs_append_bytes,
            fs_create_dir,
            fs_read_dir,
            fs_remove_file,
            fs_open,
            fs_create,
            fs_append,
            file_read_all,
            file_read_bytes,
            file_write_all,
            file_write_bytes,
            file_flush,
            file_close,
            process_inherit,
            process_null,
            process_pipe,
            process_supervisor,
            process_start,
            process_run,
            process_child_stdin,
            process_child_stdout,
            process_child_stderr,
            process_child_wait,
            process_child_wait_or_none,
            process_child_wait_ok,
            process_child_kill,
            process_child_terminate,
            process_child_close,
            process_pipe_read_all,
            process_pipe_read_line,
            process_pipe_read_bytes,
            process_pipe_write_all,
            process_pipe_write_bytes,
            process_pipe_flush,
            process_pipe_close,
            process_completed_status,
            process_completed_success,
            process_completed_stdout,
            process_completed_stderr,
            process_completed_stdout_bytes,
            process_completed_stderr_bytes,
            process_completed_check,
            process_supervisor_start,
            process_supervisor_wait,
            process_supervisor_wait_or_none,
            process_supervisor_stop,
            process_supervisor_is_empty,
            process_supervisor_close,
            net_connect,
            net_connect_timeout,
            net_listen,
            net_udp_bind,
            net_unix_listen,
            net_unix_connect,
            net_unix_connect_timeout,
            net_tls_listen,
            net_tls_connect,
            net_tls_connect_timeout,
            net_http_listen,
            net_http_request_text,
            net_http_request_text_timeout,
            net_http_request_bytes,
            net_http_request_bytes_timeout,
            net_websocket_listen,
            net_websocket_connect,
            net_websocket_connect_timeout,
            tcp_listener_accept,
            tcp_listener_local_addr,
            tcp_listener_close,
            tcp_stream_read_all,
            tcp_stream_read_line,
            tcp_stream_read_bytes,
            tcp_stream_read_exact,
            tcp_stream_write_all,
            tcp_stream_write_bytes,
            tcp_stream_flush,
            tcp_stream_local_addr,
            tcp_stream_peer_addr,
            tcp_stream_shutdown_read,
            tcp_stream_shutdown_write,
            tcp_stream_shutdown_both,
            tcp_stream_close,
            udp_socket_send_text,
            udp_socket_send_bytes,
            udp_socket_recv,
            udp_socket_recv_from,
            udp_socket_local_addr,
            udp_socket_peer_addr,
            udp_socket_close,
            udp_datagram_address,
            udp_datagram_bytes,
            udp_datagram_text,
            http_listener_accept,
            http_listener_local_addr,
            http_listener_close,
            http_exchange_method,
            http_exchange_path,
            http_exchange_headers,
            http_exchange_body_text,
            http_exchange_body_bytes,
            http_exchange_respond_text,
            http_exchange_respond_bytes,
            http_response_status,
            http_response_reason,
            http_response_headers,
            http_response_text,
            http_response_bytes,
            websocket_listener_accept,
            websocket_listener_local_addr,
            websocket_send_text,
            websocket_send_bytes,
            websocket_recv_text,
            websocket_recv_bytes,
            websocket_close,
            unix_listener_accept,
            unix_listener_close,
            unix_stream_read_line,
            unix_stream_read_exact,
            unix_stream_write_all,
            unix_stream_close,
            tls_listener_accept,
            tls_listener_local_addr,
            tls_listener_close,
            tls_stream_read_line,
            tls_stream_read_exact,
            tls_stream_write_all,
            tls_stream_close,
            cancelled,
            yield_now,
            sleep_value_void,
            start_task_call,
        };

        let return_ty = ensure_direct_type(
            &function.return_type,
            &self.classes,
            &format!("return type of `{}`", function.name),
        )?;
        compiler.compile_reachable_blocks(function, &return_ty)?;

        compiler.builder.seal_all_blocks();
        compiler.builder.finalize();
        try_or_string_error!(
            ctx.verify(self.object.isa()),
            "failed to define direct function `{}`: {}\n{}",
            function.name,
            ctx.func.display()
        );
        try_or_string_error!(
            self.object.define_function(func_id, &mut ctx),
            "failed to define direct function `{}`: {}",
            function.name
        );
        Ok(())
    }

    fn define_function_thunk(&mut self, function: &MirFunction) -> std::result::Result<(), String> {
        if function.receiver.is_some() {
            return Err(format!(
                "direct backend does not yet support task-start thunks for methods like `{}`",
                function.name
            ));
        }

        let thunk_id = self.function_thunks[&function.name];
        let target_id = self.functions[&function.name];
        let mut ctx = self.object.make_context();
        ctx.func.signature = thunk_signature(self.call_conv);
        ctx.func.name = UserFuncName::user(0, thunk_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let args_ptr = builder.block_params(entry)[0];
        let target_ref = self.object.declare_func_in_func(target_id, builder.func);
        let unbox_i64 = self
            .object
            .declare_func_in_func(self.unbox_i64, builder.func);
        let unbox_int64 = self
            .object
            .declare_func_in_func(self.unbox_int64, builder.func);
        let unbox_u64 = self
            .object
            .declare_func_in_func(self.unbox_u64, builder.func);
        let unbox_f64 = self
            .object
            .declare_func_in_func(self.unbox_f64, builder.func);
        let unbox_bool = self
            .object
            .declare_func_in_func(self.unbox_bool, builder.func);
        let release_value = self
            .object
            .declare_func_in_func(self.release_value, builder.func);

        if matches!(function.name.as_str(), "main" | "__script") {
            let module_constant = self
                .object
                .declare_func_in_func(self.module_constant, builder.func);
            for constant in &self.module.constants {
                let initializer =
                    self.function_thunks
                        .get(&constant.initializer)
                        .ok_or_else(|| {
                            format!(
                                "direct backend cannot find module constant initializer `{}`",
                                constant.initializer
                            )
                        })?;
                let initializer = self.object.declare_func_in_func(*initializer, builder.func);
                let initializer = builder.ins().func_addr(types::I64, initializer);
                let (key_ptr, key_len) = declare_string_constant(
                    &mut self.object,
                    &mut self.string_data,
                    &mut builder,
                    constant.key.as_bytes(),
                )?;
                let call = builder
                    .ins()
                    .call(module_constant, &[key_ptr, key_len, initializer]);
                let value = builder.inst_results(call)[0];
                builder.ins().call(release_value, &[value]);
            }
        }

        let mut lowered_args = Vec::new();
        let param_types = self.function_param_types[&function.name].clone();
        for (index, param_ty) in param_types.iter().enumerate() {
            let raw = builder
                .ins()
                .load(types::I64, MemFlags::new(), args_ptr, (index as i32) * 8);
            // The uniform function-value buffer is an in/out ownership
            // channel. Clear each incoming slot before transferring its
            // retained handle into the concrete function ABI; mutable
            // parameter writebacks are installed into their original slots
            // after the call.
            let zero = builder.ins().iconst(types::I64, 0);
            builder
                .ins()
                .store(MemFlags::new(), zero, args_ptr, (index as i32) * 8);
            match param_ty {
                DirectType::Opaque(_) => lowered_args.push(raw),
                DirectType::Scalar(ScalarKind::Int32) => {
                    let inst = builder.ins().call(unbox_i64, &[raw]);
                    lowered_args.push(builder.inst_results(inst)[0]);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::Scalar(ScalarKind::Int64) => {
                    let inst = builder.ins().call(unbox_int64, &[raw]);
                    lowered_args.push(builder.inst_results(inst)[0]);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::Scalar(ScalarKind::Uint64) => {
                    let inst = builder.ins().call(unbox_u64, &[raw]);
                    lowered_args.push(builder.inst_results(inst)[0]);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::Scalar(ScalarKind::Float32)
                | DirectType::Scalar(ScalarKind::Float64) => {
                    let inst = builder.ins().call(unbox_f64, &[raw]);
                    lowered_args.push(builder.inst_results(inst)[0]);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::Scalar(ScalarKind::Bool) => {
                    let inst = builder.ins().call(unbox_bool, &[raw]);
                    lowered_args.push(builder.inst_results(inst)[0]);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::Scalar(ScalarKind::Unit) => {
                    lowered_args.push(builder.ins().iconst(types::I64, 0));
                    let _ = builder.ins().call(release_value, &[raw]);
                }
                DirectType::PlainClass(_) => {
                    lowered_args.extend(unbox_thunk_value(self, &mut builder, raw, param_ty)?);
                    let _ = builder.ins().call(release_value, &[raw]);
                }
            }
        }

        let inst = builder.ins().call(target_ref, &lowered_args);
        let results = builder.inst_results(inst).to_vec();
        let return_ty = match self.function_return_types.get(&function.name).cloned() {
            Some(return_ty) => return_ty,
            None => {
                return Err(format!(
                    "direct backend does not know return type for `{}`",
                    function.name
                ));
            }
        };
        // `target_ref` uses `signature_for(function)`: its results are the
        // declared return ABI followed by mutable-parameter writebacks. The
        // cached direct types above are built from that same `MirFunction`.
        let return_count = return_ty.value_count();
        let mut cursor = return_count;
        for (index, param) in function.params.iter().enumerate() {
            if param.passing != MirReceiverKind::BorrowMut {
                continue;
            }
            let writeback_ty = &param_types[index];
            let writeback_count = writeback_ty.value_count();
            let boxed_writeback = box_thunk_value(
                self,
                &mut builder,
                &results[cursor..cursor + writeback_count],
                writeback_ty,
            )?;
            builder.ins().store(
                MemFlags::new(),
                boxed_writeback,
                args_ptr,
                (index as i32) * 8,
            );
            cursor += writeback_count;
        }
        let boxed = box_thunk_value(self, &mut builder, &results[..return_count], &return_ty)?;
        builder.ins().return_(&[boxed]);
        builder.finalize();

        try_or_string_error!(
            self.object.define_function(thunk_id, &mut ctx),
            "failed to define direct function thunk `{}`: {}",
            function.name
        );
        Ok(())
    }

    fn define_function_default_binder(
        &mut self,
        function: &MirFunction,
    ) -> std::result::Result<(), String> {
        if function.receiver.is_some() {
            return Err(format!(
                "direct backend cannot build a function-value default binder for method `{}`",
                function.name
            ));
        }
        let binder_id = self.function_default_binders[&function.name];
        let mut ctx = self.object.make_context();
        ctx.func.signature = default_binder_signature(self.call_conv);
        ctx.func.name = UserFuncName::user(0, binder_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let args_ptr = builder.block_params(entry)[0];
        let transfer_defaults = builder.block_params(entry)[2];
        let store_owned = self
            .object
            .declare_func_in_func(self.arg_buffer_store_owned, builder.func);
        let param_types = self
            .function_param_types
            .get(&function.name)
            .cloned()
            .unwrap_or_default();

        for (index, param) in function.params.iter().enumerate() {
            let Some(default_name) = param.default_function.as_ref() else {
                continue;
            };
            let raw = builder
                .ins()
                .load(types::I64, MemFlags::new(), args_ptr, (index as i32) * 8);
            let missing = builder.ins().icmp_imm(IntCC::Equal, raw, 0);
            let default_block = builder.create_block();
            let next_block = builder.create_block();
            builder
                .ins()
                .brif(missing, default_block, &[], next_block, &[]);
            builder.switch_to_block(default_block);
            let default_id = *self.functions.get(default_name).ok_or_else(|| {
                format!(
                    "direct backend is missing default function `{default_name}` for `{}`",
                    function.name
                )
            })?;
            let default_ref = self.object.declare_func_in_func(default_id, builder.func);
            let call = builder.ins().call(default_ref, &[]);
            let results = builder.inst_results(call).to_vec();
            let param_ty = param_types.get(index).ok_or_else(|| {
                format!(
                    "direct backend is missing parameter {} metadata for `{}`",
                    index + 1,
                    function.name
                )
            })?;
            let boxed = box_thunk_value(self, &mut builder, &results, param_ty)?;
            let transfer = builder
                .ins()
                .icmp_imm(IntCC::NotEqual, transfer_defaults, 0);
            let transfer_block = builder.create_block();
            let retain_block = builder.create_block();
            builder
                .ins()
                .brif(transfer, transfer_block, &[], retain_block, &[]);
            builder.switch_to_block(transfer_block);
            let index_value = builder.ins().iconst(types::I64, index as i64);
            builder
                .ins()
                .call(store_owned, &[args_ptr, index_value, boxed]);
            builder.ins().jump(next_block, &[]);
            builder.seal_block(transfer_block);
            builder.switch_to_block(retain_block);
            builder
                .ins()
                .store(MemFlags::new(), boxed, args_ptr, (index as i32) * 8);
            builder.ins().jump(next_block, &[]);
            builder.seal_block(retain_block);
            builder.seal_block(default_block);
            builder.switch_to_block(next_block);
            builder.seal_block(next_block);
        }
        builder.ins().return_(&[]);
        builder.finalize();
        try_or_string_error!(
            self.object.define_function(binder_id, &mut ctx),
            "failed to define direct default binder `{}`: {}",
            function.name
        );
        Ok(())
    }

    fn define_cleanup_thunks(&mut self, function: &MirFunction) -> std::result::Result<(), String> {
        let reachable = self
            .reachable_blocks
            .get(&function.name)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "direct backend is missing reachable-block metadata for `{}`",
                    function.name
                )
            })?;
        for place in collect_cleanup_places(function, &reachable) {
            self.define_cleanup_thunk(function, &place)?;
        }
        Ok(())
    }

    fn define_cleanup_thunk(
        &mut self,
        function: &MirFunction,
        place: &str,
    ) -> std::result::Result<(), String> {
        let thunk_id = *self
            .cleanup_thunks
            .get(&(function.name.clone(), place.to_string()))
            .ok_or({
                format!(
                    "direct backend could not find cleanup thunk for `{}` in `{}`",
                    place, function.name
                )
            })?;
        let place_ty = cleanup_place_type_in_reachable(
            function,
            &self.classes,
            place,
            &self.function_return_types,
            self.reachable_blocks.get(&function.name).ok_or_else(|| {
                format!(
                    "direct backend is missing reachable-block metadata for `{}`",
                    function.name
                )
            })?,
        )?;

        let mut ctx = self.object.make_context();
        ctx.func.signature = thunk_signature(self.call_conv);
        ctx.func.name = UserFuncName::user(0, thunk_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let args_ptr = builder.block_params(entry)[0];
        let raw = builder.ins().load(types::I64, MemFlags::new(), args_ptr, 0);
        match &place_ty {
            DirectType::PlainClass(class_ty) => {
                let close_method = self
                    .classes
                    .get(&class_ty.class_name)
                    .and_then(|class| class.methods.iter().find(|method| method.name == "close"))
                    .cloned();
                if let Some(method) = close_method {
                    let target_id = *self.functions.get(&method.function_name).ok_or({
                        format!(
                            "direct backend could not find cleanup close method `{}`",
                            method.function_name
                        )
                    })?;
                    let target_ref = self.object.declare_func_in_func(target_id, builder.func);
                    let lowered = unbox_thunk_value(self, &mut builder, raw, &place_ty)?;
                    let inst = builder.ins().call(target_ref, &lowered);
                    let results = builder.inst_results(inst).to_vec();
                    release_direct_call_results(
                        self,
                        &mut builder,
                        &method.function_name,
                        &results,
                    )?;
                }
                let zero = builder.ins().iconst(types::I64, 0);
                let unit = box_thunk_value(
                    self,
                    &mut builder,
                    &[zero],
                    &DirectType::Scalar(ScalarKind::Unit),
                )?;
                builder.ins().return_(&[unit]);
            }
            DirectType::Opaque(ty) => {
                let close_method = match ty {
                    Type::Named(class_name, _) => self
                        .classes
                        .get(class_name)
                        .and_then(|class| {
                            class.methods.iter().find(|method| method.name == "close")
                        })
                        .cloned(),
                    _ => None,
                };
                if let Some(method) = close_method {
                    let target_id = *self.functions.get(&method.function_name).ok_or({
                        format!(
                            "direct backend could not find cleanup close method `{}`",
                            method.function_name
                        )
                    })?;
                    let target_ref = self.object.declare_func_in_func(target_id, builder.func);
                    let retain_value = self
                        .object
                        .declare_func_in_func(self.retain_value, builder.func);
                    let retained = builder.ins().call(retain_value, &[raw]);
                    let retained = builder.inst_results(retained)[0];
                    let inst = builder.ins().call(target_ref, &[retained]);
                    let results = builder.inst_results(inst).to_vec();
                    release_direct_call_results(
                        self,
                        &mut builder,
                        &method.function_name,
                        &results,
                    )?;
                    let zero = builder.ins().iconst(types::I64, 0);
                    let unit = box_thunk_value(
                        self,
                        &mut builder,
                        &[zero],
                        &DirectType::Scalar(ScalarKind::Unit),
                    )?;
                    builder.ins().return_(&[unit]);
                } else {
                    let close_value = self
                        .object
                        .declare_func_in_func(self.close_value, builder.func);
                    let cancel_before = builder.ins().iconst(types::I64, 1);
                    let inst = builder.ins().call(close_value, &[raw, cancel_before]);
                    let result = builder.inst_results(inst)[0];
                    builder.ins().return_(&[result]);
                }
            }
            DirectType::Scalar(_) => {
                let zero = builder.ins().iconst(types::I64, 0);
                let unit = box_thunk_value(
                    self,
                    &mut builder,
                    &[zero],
                    &DirectType::Scalar(ScalarKind::Unit),
                )?;
                builder.ins().return_(&[unit]);
            }
        }
        builder.finalize();

        try_or_string_error!(
            self.object.define_function(thunk_id, &mut ctx),
            "failed to define cleanup thunk for `{}` in `{}`: {}",
            place,
            function.name
        );
        Ok(())
    }

    fn define_main_wrapper(&mut self) -> std::result::Result<(), String> {
        let entry_name = if self.functions.contains_key("main") {
            "main".to_string()
        } else if self.functions.contains_key("__script") {
            "__script".to_string()
        } else {
            return Err(
                "direct backend requires a `main` function or top-level script".to_string(),
            );
        };
        let entry_thunk_id = self.function_thunks[&entry_name];

        let mut ctx = self.object.make_context();
        ctx.func.signature = main_signature(self.call_conv);
        let wrapper_id = try_or_string_error!(
            self.object
                .declare_function("main", Linkage::Export, &ctx.func.signature),
            "failed to declare main wrapper: {}"
        );
        ctx.func.name = UserFuncName::user(0, wrapper_id.as_u32());

        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        let runtime_init = self
            .object
            .declare_func_in_func(self.runtime_init, builder.func);
        let run_root = self
            .object
            .declare_func_in_func(self.run_root, builder.func);
        let entry_thunk_ref = self
            .object
            .declare_func_in_func(entry_thunk_id, builder.func);
        let (path_ptr, path_len) = declare_string_constant(
            &mut self.object,
            &mut self.string_data,
            &mut builder,
            self.program_path.as_bytes(),
        )?;
        let (source_ptr, source_len) = declare_string_constant(
            &mut self.object,
            &mut self.string_data,
            &mut builder,
            self.program_source.as_bytes(),
        )?;
        builder
            .ins()
            .call(runtime_init, &[path_ptr, path_len, source_ptr, source_len]);
        let thunk_ptr = builder.ins().func_addr(types::I64, entry_thunk_ref);
        let result = builder.ins().call(run_root, &[thunk_ptr]);
        let return_code = builder.inst_results(result)[0];
        builder.ins().return_(&[return_code]);
        builder.finalize();

        try_or_string_error!(
            self.object.define_function(wrapper_id, &mut ctx),
            "failed to define main wrapper: {}"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectViewPlace {
    alternatives: Vec<DirectViewAlternative>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectViewAlternative {
    place: String,
    conditions: Vec<(Variable, i64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectClosureCaptureWriteback {
    index: usize,
    place: DirectViewPlace,
    ty: DirectType,
}

impl DirectViewPlace {
    fn static_place(place: String) -> Self {
        Self {
            alternatives: vec![DirectViewAlternative {
                place,
                conditions: Vec::new(),
            }],
        }
    }

    fn project(mut self, projection: &str) -> Self {
        if projection.is_empty() {
            return self;
        }
        for alternative in &mut self.alternatives {
            alternative.place = format!("{}.{}", alternative.place, projection);
        }
        self
    }

    fn conditioned(mut self, selector: Variable, expected: i64) -> Self {
        for alternative in &mut self.alternatives {
            let condition = (selector, expected);
            if !alternative.conditions.contains(&condition) {
                alternative.conditions.push(condition);
            }
        }
        self
    }
}

fn direct_terminator_successors(terminator: &Terminator) -> Vec<&str> {
    match terminator {
        Terminator::Goto(label) => vec![label],
        Terminator::Branch {
            then_label,
            else_label,
            ..
        } => vec![then_label, else_label],
        Terminator::ForRange {
            body_label,
            exit_label,
            ..
        } => vec![body_label, exit_label],
        Terminator::Match {
            arms, otherwise, ..
        } => arms
            .iter()
            .map(|arm| arm.label.as_str())
            .chain(std::iter::once(otherwise.as_str()))
            .collect(),
        Terminator::Return(_) | Terminator::AssertFail { .. } | Terminator::Unreachable => {
            Vec::new()
        }
    }
}

fn reachable_direct_block_labels(
    function: &MirFunction,
) -> std::result::Result<HashSet<String>, String> {
    let mut blocks = HashMap::new();
    for block in &function.blocks {
        if blocks.insert(block.label.as_str(), block).is_some() {
            return Err(format!(
                "direct backend found duplicate MIR block `{}` in `{}`",
                block.label, function.name
            ));
        }
    }
    if !blocks.contains_key(function.entry.as_str()) {
        return Err(format!(
            "direct backend could not find entry block `{}` in `{}`",
            function.entry, function.name
        ));
    }
    for block in &function.blocks {
        for successor in direct_terminator_successors(&block.terminator) {
            if !blocks.contains_key(successor) {
                return Err(format!(
                    "direct backend MIR block `{}` in `{}` targets unknown block `{successor}`",
                    block.label, function.name
                ));
            }
        }
    }

    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry.as_str()];
    while let Some(label) = pending.pop() {
        if !reachable.insert(label.to_string()) {
            continue;
        }
        let block = blocks.get(label).ok_or_else(|| {
            format!(
                "direct backend could not resolve MIR block `{label}` in `{}`",
                function.name
            )
        })?;
        pending.extend(direct_terminator_successors(&block.terminator));
    }
    Ok(reachable)
}

fn direct_view_selector_tags(
    function: &MirFunction,
    reachable: &HashSet<String>,
) -> HashMap<String, HashMap<String, i64>> {
    let mut projections = HashMap::<String, BTreeSet<String>>::new();
    for block in &function.blocks {
        if !reachable.contains(&block.label) {
            continue;
        }
        for instruction in &block.instructions {
            let Instruction::BeginReturnedLoan {
                loan,
                projections: loan_projections,
                ..
            } = instruction
            else {
                continue;
            };
            projections
                .entry(loan.clone())
                .or_default()
                .extend(loan_projections.iter().cloned());
        }
    }
    projections
        .into_iter()
        .map(|(loan, projections)| {
            let tags = projections
                .into_iter()
                .enumerate()
                .map(|(index, projection)| (projection, index as i64))
                .collect();
            (loan, tags)
        })
        .collect()
}

fn direct_view_maps_equivalent(
    left: &HashMap<String, DirectViewPlace>,
    right: &HashMap<String, DirectViewPlace>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(loan, left_place)| {
            right.get(loan).is_some_and(|right_place| {
                left_place.alternatives.len() == right_place.alternatives.len()
                    && left_place.alternatives.iter().all(|left_alternative| {
                        right_place.alternatives.iter().any(|right_alternative| {
                            left_alternative.place == right_alternative.place
                                && left_alternative.conditions.len()
                                    == right_alternative.conditions.len()
                                && left_alternative.conditions.iter().all(|condition| {
                                    right_alternative.conditions.contains(condition)
                                })
                        })
                    })
            })
        })
}

fn merge_direct_closure_writebacks(
    left: &HashMap<String, Vec<DirectClosureCaptureWriteback>>,
    right: &HashMap<String, Vec<DirectClosureCaptureWriteback>>,
) -> HashMap<String, Vec<DirectClosureCaptureWriteback>> {
    let mut merged = left.clone();
    for (closure, writebacks) in right {
        let merged_writebacks = merged.entry(closure.clone()).or_default();
        for writeback in writebacks {
            if let Some(existing) = merged_writebacks
                .iter_mut()
                .find(|existing| existing.index == writeback.index && existing.ty == writeback.ty)
            {
                for alternative in &writeback.place.alternatives {
                    if !existing.place.alternatives.contains(alternative) {
                        existing.place.alternatives.push(alternative.clone());
                    }
                }
            } else {
                merged_writebacks.push(writeback.clone());
            }
        }
    }
    merged
}

fn direct_closure_metadata_uses(value: &Rvalue) -> impl Iterator<Item = &str> {
    let transferred = match value {
        Rvalue::Use(Operand::Place(place) | Operand::MovePlace(place)) => Some(place.as_str()),
        _ => None,
    };
    let called = match value {
        Rvalue::Call {
            callee: CallTarget::Value(Operand::Place(place) | Operand::MovePlace(place)),
            ..
        } => Some(place.as_str()),
        _ => None,
    };
    transferred
        .into_iter()
        .chain(called)
        .map(|place| place.split('.').next().unwrap_or(place))
}

/// Computes which closure values still need mutable-capture writeback
/// descriptors on entry to each reachable block. The descriptors are compiler
/// metadata, not part of the runtime closure value, and are only observed when
/// an indirect call is compiled or when a closure value is transferred to
/// another local. Keeping dead descriptors across a loop backedge makes a
/// loop-local closure look like a new loop-carried identity after the header
/// has already been compiled.
fn direct_closure_writeback_live_ins(
    function: &MirFunction,
    reachable: &HashSet<String>,
) -> HashMap<String, HashSet<String>> {
    let closure_roots = function
        .local_types
        .iter()
        .filter(|local| matches!(&local.ty, Type::Closure { .. }))
        .map(|local| {
            local
                .name
                .split('.')
                .next()
                .unwrap_or(&local.name)
                .to_string()
        })
        .collect::<HashSet<_>>();
    let blocks = function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.label))
        .map(|block| (block.label.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut block_uses = HashMap::<String, HashSet<String>>::new();
    let mut block_defs = HashMap::<String, HashSet<String>>::new();

    for block in blocks.values() {
        let mut uses = HashSet::new();
        let mut defs = HashSet::new();
        for instruction in &block.instructions {
            let value = match instruction {
                Instruction::Assign { value, .. } | Instruction::WriteLoan { value, .. } => {
                    Some(value)
                }
                _ => None,
            };
            if let Some(value) = value {
                for root in direct_closure_metadata_uses(value) {
                    if closure_roots.contains(root) && !defs.contains(root) {
                        uses.insert(root.to_string());
                    }
                }
            }
            if let Instruction::Assign { target, .. } = instruction {
                let root = target.split('.').next().unwrap_or(target);
                if closure_roots.contains(root) {
                    defs.insert(root.to_string());
                }
            }
        }
        block_uses.insert(block.label.clone(), uses);
        block_defs.insert(block.label.clone(), defs);
    }

    let mut live_ins = reachable
        .iter()
        .map(|label| (label.clone(), HashSet::new()))
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for block in blocks.values() {
            let mut live_out = HashSet::new();
            for successor in direct_terminator_successors(&block.terminator) {
                if let Some(successor_live_in) = live_ins.get(successor) {
                    live_out.extend(successor_live_in.iter().cloned());
                }
            }
            let defs = &block_defs[&block.label];
            live_out.retain(|root| !defs.contains(root));
            live_out.extend(block_uses[&block.label].iter().cloned());
            let entry = live_ins
                .get_mut(&block.label)
                .expect("reachable block should have closure liveness state");
            if *entry != live_out {
                *entry = live_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    live_ins
}

fn direct_reverse_postorder(
    function: &MirFunction,
    reachable: &HashSet<String>,
) -> std::result::Result<Vec<String>, String> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.label.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut discovered = HashSet::new();
    let mut postorder = Vec::with_capacity(reachable.len());
    let mut pending = vec![(function.entry.clone(), false)];
    while let Some((label, expanded)) = pending.pop() {
        if expanded {
            postorder.push(label);
            continue;
        }
        if !discovered.insert(label.clone()) {
            continue;
        }
        let block = blocks.get(label.as_str()).ok_or_else(|| {
            format!(
                "direct backend could not resolve reachable block `{label}` in `{}`",
                function.name
            )
        })?;
        pending.push((label, true));
        for successor in direct_terminator_successors(&block.terminator)
            .into_iter()
            .rev()
        {
            if reachable.contains(successor) && !discovered.contains(successor) {
                pending.push((successor.to_string(), false));
            }
        }
    }
    postorder.reverse();
    Ok(postorder)
}

struct FunctionCompiler<'a> {
    builder: FunctionBuilder<'a>,
    blocks: HashMap<String, cranelift_codegen::ir::Block>,
    variables: HashMap<String, Vec<Variable>>,
    variable_types: HashMap<String, DirectType>,
    next_variable_index: usize,
    function_refs: HashMap<String, cranelift_codegen::ir::FuncRef>,
    function_thunk_refs: HashMap<String, cranelift_codegen::ir::FuncRef>,
    function_default_binder_refs: HashMap<String, cranelift_codegen::ir::FuncRef>,
    cleanup_thunk_refs: HashMap<String, cranelift_codegen::ir::FuncRef>,
    function_return_types: HashMap<String, DirectType>,
    function_param_types: HashMap<String, Vec<DirectType>>,
    function_writeback_types: HashMap<String, Vec<DirectType>>,
    function_frame_metadata: HashMap<String, (String, Span)>,
    current_function_name: String,
    current_function_path: String,
    writeback_locals: Vec<(String, DirectType)>,
    mutable_param_indices: HashMap<String, usize>,
    classes: HashMap<String, MirClass>,
    trait_impls: Vec<MirTraitImpl>,
    return_type: Type,
    owned_opaque_temporaries: HashSet<Value>,
    view_places: HashMap<String, DirectViewPlace>,
    view_selector_vars: HashMap<String, Variable>,
    view_selector_tags: HashMap<String, HashMap<String, i64>>,
    closure_selector_vars: HashMap<String, Variable>,
    closure_selector_tags: HashMap<(String, usize), i64>,
    closure_capture_writebacks: HashMap<String, Vec<DirectClosureCaptureWriteback>>,
    object: &'a mut ObjectModule,
    string_data: &'a mut HashMap<Vec<u8>, DataId>,
    cleanup_places: Vec<String>,
    cleanup_active_vars: HashMap<String, Variable>,
    cleanup_registration_vars: HashMap<String, Variable>,
    safepoint_fuel: Option<Variable>,
    exit_call: cranelift_codegen::ir::FuncRef,
    set_returned_view_projection: cranelift_codegen::ir::FuncRef,
    take_returned_view_projection: cranelift_codegen::ir::FuncRef,
    print_i64: cranelift_codegen::ir::FuncRef,
    print_u64: cranelift_codegen::ir::FuncRef,
    print_f32: cranelift_codegen::ir::FuncRef,
    print_f64: cranelift_codegen::ir::FuncRef,
    print_bool: cranelift_codegen::ir::FuncRef,
    print_value: cranelift_codegen::ir::FuncRef,
    sqrt_f64: cranelift_codegen::ir::FuncRef,
    assert_fail: cranelift_codegen::ir::FuncRef,
    assert_fail_detailed: cranelift_codegen::ir::FuncRef,
    fail_division_by_zero: cranelift_codegen::ir::FuncRef,
    fail_int32_overflow: cranelift_codegen::ir::FuncRef,
    fail_integer_overflow: cranelift_codegen::ir::FuncRef,
    register_cleanup: cranelift_codegen::ir::FuncRef,
    unregister_cleanup: cranelift_codegen::ir::FuncRef,
    refresh_cleanup: cranelift_codegen::ir::FuncRef,
    set_next_mutable_sinks: cranelift_codegen::ir::FuncRef,
    set_next_indirect_mutable_sinks: cranelift_codegen::ir::FuncRef,
    current_mutable_sink: cranelift_codegen::ir::FuncRef,
    mutable_sink_new: cranelift_codegen::ir::FuncRef,
    mutable_sink_project: cranelift_codegen::ir::FuncRef,
    mutable_sink_store_owned: cranelift_codegen::ir::FuncRef,
    mutable_sink_release: cranelift_codegen::ir::FuncRef,
    tag_value_type: cranelift_codegen::ir::FuncRef,
    box_i32: cranelift_codegen::ir::FuncRef,
    box_i64: cranelift_codegen::ir::FuncRef,
    box_u64: cranelift_codegen::ir::FuncRef,
    box_uint_literal: cranelift_codegen::ir::FuncRef,
    box_f64: cranelift_codegen::ir::FuncRef,
    box_bool: cranelift_codegen::ir::FuncRef,
    function_value: cranelift_codegen::ir::FuncRef,
    module_constant: cranelift_codegen::ir::FuncRef,
    closure_value: cranelift_codegen::ir::FuncRef,
    closure_capture: cranelift_codegen::ir::FuncRef,
    function_call: cranelift_codegen::ir::FuncRef,
    function_bind_defaults: cranelift_codegen::ir::FuncRef,
    box_unit: cranelift_codegen::ir::FuncRef,
    string_literal: cranelift_codegen::ir::FuncRef,
    string_len: cranelift_codegen::ir::FuncRef,
    string_byte_len: cranelift_codegen::ir::FuncRef,
    string_slice: cranelift_codegen::ir::FuncRef,
    string_contains: cranelift_codegen::ir::FuncRef,
    string_starts_with: cranelift_codegen::ir::FuncRef,
    string_ends_with: cranelift_codegen::ir::FuncRef,
    string_split: cranelift_codegen::ir::FuncRef,
    string_replace: cranelift_codegen::ir::FuncRef,
    string_to_lower: cranelift_codegen::ir::FuncRef,
    string_to_upper: cranelift_codegen::ir::FuncRef,
    string_strip_prefix: cranelift_codegen::ir::FuncRef,
    string_strip_suffix: cranelift_codegen::ir::FuncRef,
    string_trim: cranelift_codegen::ir::FuncRef,
    string_join: cranelift_codegen::ir::FuncRef,
    stringify_value: cranelift_codegen::ir::FuncRef,
    format_value: cranelift_codegen::ir::FuncRef,
    abs_value: cranelift_codegen::ir::FuncRef,
    min_value: cranelift_codegen::ir::FuncRef,
    max_value: cranelift_codegen::ir::FuncRef,
    sqrt_value: cranelift_codegen::ir::FuncRef,
    round_value: cranelift_codegen::ir::FuncRef,
    divmod_value: cranelift_codegen::ir::FuncRef,
    parse_int32: cranelift_codegen::ir::FuncRef,
    parse_int64: cranelift_codegen::ir::FuncRef,
    parse_float64: cranelift_codegen::ir::FuncRef,
    duration_literal: cranelift_codegen::ir::FuncRef,
    duration_from_i64: cranelift_codegen::ir::FuncRef,
    duration_to_float: cranelift_codegen::ir::FuncRef,
    rng_new: cranelift_codegen::ir::FuncRef,
    rng_next_int: cranelift_codegen::ir::FuncRef,
    rng_next_float: cranelift_codegen::ir::FuncRef,
    rng_shuffle: cranelift_codegen::ir::FuncRef,
    random_secure_int: cranelift_codegen::ir::FuncRef,
    random_secure_bytes: cranelift_codegen::ir::FuncRef,
    range_new: cranelift_codegen::ir::FuncRef,
    range_current: cranelift_codegen::ir::FuncRef,
    range_end: cranelift_codegen::ir::FuncRef,
    range_advance: cranelift_codegen::ir::FuncRef,
    vec_empty: cranelift_codegen::ir::FuncRef,
    vec_len: cranelift_codegen::ir::FuncRef,
    vec_is_empty: cranelift_codegen::ir::FuncRef,
    vec_push_in_place: cranelift_codegen::ir::FuncRef,
    #[allow(dead_code)]
    vec_pop_in_place: cranelift_codegen::ir::FuncRef,
    vec_get: cranelift_codegen::ir::FuncRef,
    vec_set_in_place: cranelift_codegen::ir::FuncRef,
    #[allow(dead_code)]
    vec_remove_in_place: cranelift_codegen::ir::FuncRef,
    vec_swap_in_place: cranelift_codegen::ir::FuncRef,
    vec_contains: cranelift_codegen::ir::FuncRef,
    vec_extend_in_place: cranelift_codegen::ir::FuncRef,
    vec_insert_in_place: cranelift_codegen::ir::FuncRef,
    vec_clear_in_place: cranelift_codegen::ir::FuncRef,
    vec_reverse_in_place: cranelift_codegen::ir::FuncRef,
    collection_operation: cranelift_codegen::ir::FuncRef,
    vec_index: cranelift_codegen::ir::FuncRef,
    vec_slice: cranelift_codegen::ir::FuncRef,
    vec_index_option: cranelift_codegen::ir::FuncRef,
    vec_take_index_in_place: cranelift_codegen::ir::FuncRef,
    vec_set_index_in_place: cranelift_codegen::ir::FuncRef,
    array_zeros: cranelift_codegen::ir::FuncRef,
    array_full: cranelift_codegen::ir::FuncRef,
    array_from_vec: cranelift_codegen::ir::FuncRef,
    array_clone: cranelift_codegen::ir::FuncRef,
    array_shape: cranelift_codegen::ir::FuncRef,
    array_len: cranelift_codegen::ir::FuncRef,
    array_get: cranelift_codegen::ir::FuncRef,
    array_set_in_place: cranelift_codegen::ir::FuncRef,
    array_fill_in_place: cranelift_codegen::ir::FuncRef,
    array_index: cranelift_codegen::ir::FuncRef,
    array_set_index_in_place: cranelift_codegen::ir::FuncRef,
    array_slice: cranelift_codegen::ir::FuncRef,
    array_binary: cranelift_codegen::ir::FuncRef,
    array_map: cranelift_codegen::ir::FuncRef,
    array_reduce: cranelift_codegen::ir::FuncRef,
    map_empty: cranelift_codegen::ir::FuncRef,
    map_len: cranelift_codegen::ir::FuncRef,
    map_is_empty: cranelift_codegen::ir::FuncRef,
    map_get: cranelift_codegen::ir::FuncRef,
    map_set_in_place: cranelift_codegen::ir::FuncRef,
    map_remove_in_place: cranelift_codegen::ir::FuncRef,
    map_contains_key: cranelift_codegen::ir::FuncRef,
    map_keys: cranelift_codegen::ir::FuncRef,
    map_values: cranelift_codegen::ir::FuncRef,
    map_items: cranelift_codegen::ir::FuncRef,
    map_clear_in_place: cranelift_codegen::ir::FuncRef,
    map_extend_in_place: cranelift_codegen::ir::FuncRef,
    map_index: cranelift_codegen::ir::FuncRef,
    map_set_index_in_place: cranelift_codegen::ir::FuncRef,
    set_empty: cranelift_codegen::ir::FuncRef,
    set_len: cranelift_codegen::ir::FuncRef,
    set_is_empty: cranelift_codegen::ir::FuncRef,
    set_contains: cranelift_codegen::ir::FuncRef,
    set_insert_in_place: cranelift_codegen::ir::FuncRef,
    #[allow(dead_code)]
    set_remove_in_place: cranelift_codegen::ir::FuncRef,
    set_index_option: cranelift_codegen::ir::FuncRef,
    set_take_index_in_place: cranelift_codegen::ir::FuncRef,
    retain_value: cranelift_codegen::ir::FuncRef,
    release_value: cranelift_codegen::ir::FuncRef,
    clone_value: cranelift_codegen::ir::FuncRef,
    unbox_i64: cranelift_codegen::ir::FuncRef,
    unbox_int64: cranelift_codegen::ir::FuncRef,
    integer_to_float: cranelift_codegen::ir::FuncRef,
    integer_width_binary: cranelift_codegen::ir::FuncRef,
    unbox_u64: cranelift_codegen::ir::FuncRef,
    unbox_f64: cranelift_codegen::ir::FuncRef,
    unbox_bool: cranelift_codegen::ir::FuncRef,
    value_as_condition: cranelift_codegen::ir::FuncRef,
    unary_value: cranelift_codegen::ir::FuncRef,
    binary_value: cranelift_codegen::ir::FuncRef,
    cast_value: cranelift_codegen::ir::FuncRef,
    cast_integer_to_integer: cranelift_codegen::ir::FuncRef,
    cast_integer_to_float: cranelift_codegen::ir::FuncRef,
    cast_float_to_integer: cranelift_codegen::ir::FuncRef,
    value_type_matches: cranelift_codegen::ir::FuncRef,
    value_has_runtime_type: cranelift_codegen::ir::FuncRef,
    tuple_new: cranelift_codegen::ir::FuncRef,
    tuple_element: cranelift_codegen::ir::FuncRef,
    tuple_take_element: cranelift_codegen::ir::FuncRef,
    enum_variant: cranelift_codegen::ir::FuncRef,
    variant_matches: cranelift_codegen::ir::FuncRef,
    variant_payload: cranelift_codegen::ir::FuncRef,
    variant_take_payload: cranelift_codegen::ir::FuncRef,
    instance_empty: cranelift_codegen::ir::FuncRef,
    instance_get_field: cranelift_codegen::ir::FuncRef,
    instance_take_field: cranelift_codegen::ir::FuncRef,
    instance_set_field_owned: cranelift_codegen::ir::FuncRef,
    arg_buffer_new: cranelift_codegen::ir::FuncRef,
    arg_buffer_store: cranelift_codegen::ir::FuncRef,
    arg_buffer_store_owned: cranelift_codegen::ir::FuncRef,
    task_arg_buffer_guard: cranelift_codegen::ir::FuncRef,
    task_arg_buffer_disarm: cranelift_codegen::ir::FuncRef,
    host_builtin: cranelift_codegen::ir::FuncRef,
    ffi_call: cranelift_codegen::ir::FuncRef,
    monotonic_time_ms: cranelift_codegen::ir::FuncRef,
    channel_new: cranelift_codegen::ir::FuncRef,
    channel_send: cranelift_codegen::ir::FuncRef,
    channel_send_timeout_value: cranelift_codegen::ir::FuncRef,
    channel_try_send: cranelift_codegen::ir::FuncRef,
    channel_recv: cranelift_codegen::ir::FuncRef,
    channel_recv_in_task_group: cranelift_codegen::ir::FuncRef,
    channel_recv_with_registered_producers: cranelift_codegen::ir::FuncRef,
    channel_recv_timeout_value: cranelift_codegen::ir::FuncRef,
    channel_recv_or_none: cranelift_codegen::ir::FuncRef,
    channel_recv_or_none_timeout_value: cranelift_codegen::ir::FuncRef,
    channel_recv_or_value: cranelift_codegen::ir::FuncRef,
    channel_recv_or_value_timeout_value: cranelift_codegen::ir::FuncRef,
    channel_close: cranelift_codegen::ir::FuncRef,
    task_group_new: cranelift_codegen::ir::FuncRef,
    task_group_cancel: cranelift_codegen::ir::FuncRef,
    task_group_close: cranelift_codegen::ir::FuncRef,
    task_join: cranelift_codegen::ir::FuncRef,
    task_join_timeout_value: cranelift_codegen::ir::FuncRef,
    task_join_or_none: cranelift_codegen::ir::FuncRef,
    task_join_or_none_timeout_value: cranelift_codegen::ir::FuncRef,
    task_join_or_value: cranelift_codegen::ir::FuncRef,
    task_join_or_value_timeout_value: cranelift_codegen::ir::FuncRef,
    wait_any: cranelift_codegen::ir::FuncRef,
    wait_any_timeout_value: cranelift_codegen::ir::FuncRef,
    wait_all: cranelift_codegen::ir::FuncRef,
    wait_all_timeout_value: cranelift_codegen::ir::FuncRef,
    select: cranelift_codegen::ir::FuncRef,
    io_write: cranelift_codegen::ir::FuncRef,
    io_flush: cranelift_codegen::ir::FuncRef,
    io_read_line: cranelift_codegen::ir::FuncRef,
    fs_exists: cranelift_codegen::ir::FuncRef,
    fs_read_to_string: cranelift_codegen::ir::FuncRef,
    fs_read_bytes: cranelift_codegen::ir::FuncRef,
    fs_write_string: cranelift_codegen::ir::FuncRef,
    fs_write_bytes: cranelift_codegen::ir::FuncRef,
    fs_append_string: cranelift_codegen::ir::FuncRef,
    fs_append_bytes: cranelift_codegen::ir::FuncRef,
    fs_create_dir: cranelift_codegen::ir::FuncRef,
    fs_read_dir: cranelift_codegen::ir::FuncRef,
    fs_remove_file: cranelift_codegen::ir::FuncRef,
    fs_open: cranelift_codegen::ir::FuncRef,
    fs_create: cranelift_codegen::ir::FuncRef,
    fs_append: cranelift_codegen::ir::FuncRef,
    file_read_all: cranelift_codegen::ir::FuncRef,
    file_read_bytes: cranelift_codegen::ir::FuncRef,
    file_write_all: cranelift_codegen::ir::FuncRef,
    file_write_bytes: cranelift_codegen::ir::FuncRef,
    file_flush: cranelift_codegen::ir::FuncRef,
    file_close: cranelift_codegen::ir::FuncRef,
    process_inherit: cranelift_codegen::ir::FuncRef,
    process_null: cranelift_codegen::ir::FuncRef,
    process_pipe: cranelift_codegen::ir::FuncRef,
    process_supervisor: cranelift_codegen::ir::FuncRef,
    process_start: cranelift_codegen::ir::FuncRef,
    process_run: cranelift_codegen::ir::FuncRef,
    process_child_stdin: cranelift_codegen::ir::FuncRef,
    process_child_stdout: cranelift_codegen::ir::FuncRef,
    process_child_stderr: cranelift_codegen::ir::FuncRef,
    process_child_wait: cranelift_codegen::ir::FuncRef,
    process_child_wait_or_none: cranelift_codegen::ir::FuncRef,
    process_child_wait_ok: cranelift_codegen::ir::FuncRef,
    process_child_kill: cranelift_codegen::ir::FuncRef,
    process_child_terminate: cranelift_codegen::ir::FuncRef,
    process_child_close: cranelift_codegen::ir::FuncRef,
    process_pipe_read_all: cranelift_codegen::ir::FuncRef,
    process_pipe_read_line: cranelift_codegen::ir::FuncRef,
    process_pipe_read_bytes: cranelift_codegen::ir::FuncRef,
    process_pipe_write_all: cranelift_codegen::ir::FuncRef,
    process_pipe_write_bytes: cranelift_codegen::ir::FuncRef,
    process_pipe_flush: cranelift_codegen::ir::FuncRef,
    process_pipe_close: cranelift_codegen::ir::FuncRef,
    process_completed_status: cranelift_codegen::ir::FuncRef,
    process_completed_success: cranelift_codegen::ir::FuncRef,
    process_completed_stdout: cranelift_codegen::ir::FuncRef,
    process_completed_stderr: cranelift_codegen::ir::FuncRef,
    process_completed_stdout_bytes: cranelift_codegen::ir::FuncRef,
    process_completed_stderr_bytes: cranelift_codegen::ir::FuncRef,
    process_completed_check: cranelift_codegen::ir::FuncRef,
    process_supervisor_start: cranelift_codegen::ir::FuncRef,
    process_supervisor_wait: cranelift_codegen::ir::FuncRef,
    process_supervisor_wait_or_none: cranelift_codegen::ir::FuncRef,
    process_supervisor_stop: cranelift_codegen::ir::FuncRef,
    process_supervisor_is_empty: cranelift_codegen::ir::FuncRef,
    process_supervisor_close: cranelift_codegen::ir::FuncRef,
    net_connect: cranelift_codegen::ir::FuncRef,
    net_connect_timeout: cranelift_codegen::ir::FuncRef,
    net_listen: cranelift_codegen::ir::FuncRef,
    net_udp_bind: cranelift_codegen::ir::FuncRef,
    net_unix_listen: cranelift_codegen::ir::FuncRef,
    net_unix_connect: cranelift_codegen::ir::FuncRef,
    net_unix_connect_timeout: cranelift_codegen::ir::FuncRef,
    net_tls_listen: cranelift_codegen::ir::FuncRef,
    net_tls_connect: cranelift_codegen::ir::FuncRef,
    net_tls_connect_timeout: cranelift_codegen::ir::FuncRef,
    net_http_listen: cranelift_codegen::ir::FuncRef,
    net_http_request_text: cranelift_codegen::ir::FuncRef,
    net_http_request_text_timeout: cranelift_codegen::ir::FuncRef,
    net_http_request_bytes: cranelift_codegen::ir::FuncRef,
    net_http_request_bytes_timeout: cranelift_codegen::ir::FuncRef,
    net_websocket_listen: cranelift_codegen::ir::FuncRef,
    net_websocket_connect: cranelift_codegen::ir::FuncRef,
    net_websocket_connect_timeout: cranelift_codegen::ir::FuncRef,
    tcp_listener_accept: cranelift_codegen::ir::FuncRef,
    tcp_listener_local_addr: cranelift_codegen::ir::FuncRef,
    tcp_listener_close: cranelift_codegen::ir::FuncRef,
    tcp_stream_read_all: cranelift_codegen::ir::FuncRef,
    tcp_stream_read_line: cranelift_codegen::ir::FuncRef,
    tcp_stream_read_bytes: cranelift_codegen::ir::FuncRef,
    tcp_stream_read_exact: cranelift_codegen::ir::FuncRef,
    tcp_stream_write_all: cranelift_codegen::ir::FuncRef,
    tcp_stream_write_bytes: cranelift_codegen::ir::FuncRef,
    tcp_stream_flush: cranelift_codegen::ir::FuncRef,
    tcp_stream_local_addr: cranelift_codegen::ir::FuncRef,
    tcp_stream_peer_addr: cranelift_codegen::ir::FuncRef,
    tcp_stream_shutdown_read: cranelift_codegen::ir::FuncRef,
    tcp_stream_shutdown_write: cranelift_codegen::ir::FuncRef,
    tcp_stream_shutdown_both: cranelift_codegen::ir::FuncRef,
    tcp_stream_close: cranelift_codegen::ir::FuncRef,
    udp_socket_send_text: cranelift_codegen::ir::FuncRef,
    udp_socket_send_bytes: cranelift_codegen::ir::FuncRef,
    udp_socket_recv: cranelift_codegen::ir::FuncRef,
    udp_socket_recv_from: cranelift_codegen::ir::FuncRef,
    udp_socket_local_addr: cranelift_codegen::ir::FuncRef,
    udp_socket_peer_addr: cranelift_codegen::ir::FuncRef,
    udp_socket_close: cranelift_codegen::ir::FuncRef,
    udp_datagram_address: cranelift_codegen::ir::FuncRef,
    udp_datagram_bytes: cranelift_codegen::ir::FuncRef,
    udp_datagram_text: cranelift_codegen::ir::FuncRef,
    http_listener_accept: cranelift_codegen::ir::FuncRef,
    http_listener_local_addr: cranelift_codegen::ir::FuncRef,
    http_listener_close: cranelift_codegen::ir::FuncRef,
    http_exchange_method: cranelift_codegen::ir::FuncRef,
    http_exchange_path: cranelift_codegen::ir::FuncRef,
    http_exchange_headers: cranelift_codegen::ir::FuncRef,
    http_exchange_body_text: cranelift_codegen::ir::FuncRef,
    http_exchange_body_bytes: cranelift_codegen::ir::FuncRef,
    http_exchange_respond_text: cranelift_codegen::ir::FuncRef,
    http_exchange_respond_bytes: cranelift_codegen::ir::FuncRef,
    http_response_status: cranelift_codegen::ir::FuncRef,
    http_response_reason: cranelift_codegen::ir::FuncRef,
    http_response_headers: cranelift_codegen::ir::FuncRef,
    http_response_text: cranelift_codegen::ir::FuncRef,
    http_response_bytes: cranelift_codegen::ir::FuncRef,
    websocket_listener_accept: cranelift_codegen::ir::FuncRef,
    websocket_listener_local_addr: cranelift_codegen::ir::FuncRef,
    websocket_send_text: cranelift_codegen::ir::FuncRef,
    websocket_send_bytes: cranelift_codegen::ir::FuncRef,
    websocket_recv_text: cranelift_codegen::ir::FuncRef,
    websocket_recv_bytes: cranelift_codegen::ir::FuncRef,
    websocket_close: cranelift_codegen::ir::FuncRef,
    unix_listener_accept: cranelift_codegen::ir::FuncRef,
    unix_listener_close: cranelift_codegen::ir::FuncRef,
    unix_stream_read_line: cranelift_codegen::ir::FuncRef,
    unix_stream_read_exact: cranelift_codegen::ir::FuncRef,
    unix_stream_write_all: cranelift_codegen::ir::FuncRef,
    unix_stream_close: cranelift_codegen::ir::FuncRef,
    tls_listener_accept: cranelift_codegen::ir::FuncRef,
    tls_listener_local_addr: cranelift_codegen::ir::FuncRef,
    tls_listener_close: cranelift_codegen::ir::FuncRef,
    tls_stream_read_line: cranelift_codegen::ir::FuncRef,
    tls_stream_read_exact: cranelift_codegen::ir::FuncRef,
    tls_stream_write_all: cranelift_codegen::ir::FuncRef,
    tls_stream_close: cranelift_codegen::ir::FuncRef,
    cancelled: cranelift_codegen::ir::FuncRef,
    yield_now: cranelift_codegen::ir::FuncRef,
    sleep_value_void: cranelift_codegen::ir::FuncRef,
    start_task_call: cranelift_codegen::ir::FuncRef,
}

impl<'a> FunctionCompiler<'a> {
    fn compiled_block(
        &self,
        label: &str,
    ) -> std::result::Result<cranelift_codegen::ir::Block, String> {
        self.blocks.get(label).copied().ok_or_else(|| {
            format!(
                "direct backend could not find MIR block `{label}` in `{}`",
                self.current_function_name
            )
        })
    }

    fn snapshot_view_place_selectors(
        &mut self,
        mut place: DirectViewPlace,
    ) -> std::result::Result<DirectViewPlace, String> {
        let mut snapshots = Vec::<(Variable, Variable)>::new();
        for alternative in &mut place.alternatives {
            for (selector, _) in &mut alternative.conditions {
                let snapshot = if let Some((_, snapshot)) =
                    snapshots.iter().find(|(original, _)| original == selector)
                {
                    *snapshot
                } else {
                    let snapshot = Variable::from_u32(self.next_variable_index as u32);
                    self.next_variable_index =
                        self.next_variable_index.checked_add(1).ok_or_else(|| {
                            "direct backend exhausted selector snapshot variables".to_string()
                        })?;
                    self.builder.declare_var(snapshot, types::I64);
                    let current = self.builder.use_var(*selector);
                    self.builder.def_var(snapshot, current);
                    snapshots.push((*selector, snapshot));
                    snapshot
                };
                *selector = snapshot;
            }
        }
        Ok(place)
    }

    fn local_type(&self, name: &str) -> std::result::Result<DirectType, String> {
        self.variable_types.get(name).cloned().ok_or(format!(
            "direct backend does not know local type for `{}`",
            name
        ))
    }

    fn local_vars(&self, name: &str) -> std::result::Result<Vec<Variable>, String> {
        self.variables
            .get(name)
            .cloned()
            .ok_or(format!("direct backend does not know local `{}`", name))
    }

    fn is_opaque_value(&self, value: &ValueRef) -> bool {
        matches!(value.ty, DirectType::Opaque(_))
    }

    fn temporary_owns_opaque(&self, value: &ValueRef) -> bool {
        self.is_opaque_value(value) && self.owned_opaque_temporaries.contains(&value.values[0])
    }

    fn mark_temporary_opaque_owned(&mut self, value: &ValueRef) {
        if self.is_opaque_value(value) {
            self.owned_opaque_temporaries.insert(value.values[0]);
        }
    }

    fn clear_temporary_opaque_owned(&mut self, value: &ValueRef) {
        if self.is_opaque_value(value) {
            self.owned_opaque_temporaries.remove(&value.values[0]);
        }
    }

    fn retain_opaque_handle(&mut self, value: Value) -> Value {
        let inst = self.builder.ins().call(self.retain_value, &[value]);
        self.builder.inst_results(inst)[0]
    }

    fn release_opaque_handle(&mut self, value: Value) {
        let _ = self.builder.ins().call(self.release_value, &[value]);
    }

    fn release_all_temporary_owned(&mut self) {
        let owned = self
            .owned_opaque_temporaries
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for value in owned {
            self.release_opaque_handle(value);
        }
        self.owned_opaque_temporaries.clear();
    }

    fn release_temporary_owned_since(&mut self, baseline: &HashSet<Value>) {
        let created = self
            .owned_opaque_temporaries
            .difference(baseline)
            .copied()
            .collect::<Vec<_>>();
        for value in created {
            self.release_opaque_handle(value);
            self.owned_opaque_temporaries.remove(&value);
        }
    }

    fn release_root_if_opaque(&mut self, name: &str) -> std::result::Result<(), String> {
        let ty = self.local_type(name)?;
        if !matches!(ty, DirectType::Opaque(_)) {
            return Ok(());
        }
        let vars = self.local_vars(name)?;
        let current = self.builder.use_var(vars[0]);
        self.release_opaque_handle(current);
        Ok(())
    }

    fn release_all_opaque_roots(&mut self) -> std::result::Result<(), String> {
        let names = self.variable_types.keys().cloned().collect::<Vec<_>>();
        for name in names {
            self.release_root_if_opaque(&name)?;
        }
        Ok(())
    }

    fn transfer_opaque_arg(&mut self, value: &ValueRef) -> Value {
        if self.temporary_owns_opaque(value) {
            self.clear_temporary_opaque_owned(value);
            value.values[0]
        } else {
            self.retain_opaque_handle(value.values[0])
        }
    }

    fn transfer_owned_opaque_value(&mut self, value: &ValueRef) -> Value {
        if self.temporary_owns_opaque(value) {
            self.clear_temporary_opaque_owned(value);
            value.values[0]
        } else {
            let inst = self
                .builder
                .ins()
                .call(self.clone_value, &[value.values[0]]);
            self.builder.inst_results(inst)[0]
        }
    }

    fn export_return_value(&mut self, value: ValueRef) -> Vec<Value> {
        if !self.is_opaque_value(&value) {
            return value.values;
        }
        if self.temporary_owns_opaque(&value) {
            self.clear_temporary_opaque_owned(&value);
            value.values
        } else {
            vec![self.retain_opaque_handle(value.values[0])]
        }
    }

    fn owned_opaque_result(&mut self, values: Vec<Value>, ty: Type) -> ValueRef {
        let value = ValueRef {
            values,
            ty: DirectType::Opaque(ty),
        };
        self.mark_temporary_opaque_owned(&value);
        value
    }

    fn runtime_call_results(&mut self, callee: FuncRef, args: &[Value]) -> Vec<Value> {
        let inst = self.builder.ins().call(callee, args);
        self.builder.inst_results(inst).to_vec()
    }

    fn compile_reachable_blocks(
        &mut self,
        function: &MirFunction,
        return_ty: &DirectType,
    ) -> std::result::Result<(), String> {
        let reachable = self.blocks.keys().cloned().collect::<HashSet<_>>();
        let order = direct_reverse_postorder(function, &reachable)?;
        let closure_writeback_live_ins = direct_closure_writeback_live_ins(function, &reachable);
        let block_by_label = function
            .blocks
            .iter()
            .filter(|block| self.blocks.contains_key(&block.label))
            .map(|block| (block.label.as_str(), block))
            .collect::<HashMap<_, _>>();
        let mut incoming_views = HashMap::<String, HashMap<String, DirectViewPlace>>::new();
        incoming_views.insert(function.entry.clone(), HashMap::new());
        let mut incoming_writebacks =
            HashMap::<String, HashMap<String, Vec<DirectClosureCaptureWriteback>>>::new();
        incoming_writebacks.insert(function.entry.clone(), HashMap::new());
        let mut compiled = HashSet::new();

        for label in order {
            let block = block_by_label.get(label.as_str()).ok_or_else(|| {
                format!(
                    "direct backend could not resolve reachable block `{label}` in `{}`",
                    function.name
                )
            })?;
            self.view_places = incoming_views.get(&label).cloned().ok_or_else(|| {
                format!(
                    "direct backend has no incoming view state for block `{label}` in `{}`",
                    function.name
                )
            })?;
            self.closure_capture_writebacks =
                incoming_writebacks.get(&label).cloned().ok_or_else(|| {
                    format!(
                        "direct backend has no incoming closure-writeback state for block `{label}` in `{}`",
                        function.name
                    )
                })?;
            self.compile_block(block, return_ty)?;
            compiled.insert(label.clone());
            let outgoing_views = self.view_places.clone();
            let outgoing_writebacks = self.closure_capture_writebacks.clone();

            for successor in direct_terminator_successors(&block.terminator) {
                match incoming_views.get(successor) {
                    Some(existing) if !direct_view_maps_equivalent(existing, &outgoing_views) => {
                        return Err(format!(
                            "direct backend reaches MIR block `{successor}` in `{}` with inconsistent view identities",
                            function.name
                        ));
                    }
                    Some(_) => {}
                    None => {
                        incoming_views.insert(successor.to_string(), outgoing_views.clone());
                    }
                }
                let live_writebacks = closure_writeback_live_ins.get(successor).ok_or_else(|| {
                    format!(
                        "direct backend has no closure-writeback liveness for block `{successor}` in `{}`",
                        function.name
                    )
                })?;
                let successor_writebacks = outgoing_writebacks
                    .iter()
                    .filter(|(closure, _)| live_writebacks.contains(*closure))
                    .map(|(closure, writebacks)| (closure.clone(), writebacks.clone()))
                    .collect::<HashMap<_, _>>();
                match incoming_writebacks.get(successor).cloned() {
                    Some(existing) => {
                        let merged =
                            merge_direct_closure_writebacks(&existing, &successor_writebacks);
                        if compiled.contains(successor) && merged != existing {
                            return Err(format!(
                                "direct backend reaches already-compiled MIR block `{successor}` in `{}` with new closure writeback identities",
                                function.name
                            ));
                        }
                        incoming_writebacks.insert(successor.to_string(), merged);
                    }
                    None => {
                        incoming_writebacks.insert(successor.to_string(), successor_writebacks);
                    }
                }
            }
        }

        if compiled.len() != self.blocks.len() {
            return Err(format!(
                "direct backend did not compile every reachable MIR block in `{}`",
                function.name
            ));
        }
        Ok(())
    }

    fn compile_block(
        &mut self,
        block: &BasicBlock,
        return_ty: &DirectType,
    ) -> std::result::Result<(), String> {
        let block_id = *self.blocks.get(&block.label).ok_or_else(|| {
            format!(
                "direct backend could not find compiled block `{}`",
                block.label
            )
        })?;
        if self.builder.current_block() != Some(block_id) {
            self.builder.switch_to_block(block_id);
        }
        self.owned_opaque_temporaries.clear();

        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            self.compile_instruction(&block.label, instruction_index, instruction)?;
        }
        self.compile_terminator(&block.terminator, return_ty)?;
        Ok(())
    }

    fn compile_instruction(
        &mut self,
        block_label: &str,
        instruction_index: usize,
        instruction: &Instruction,
    ) -> std::result::Result<(), String> {
        match instruction {
            Instruction::Safepoint => {
                let Some(fuel_variable) = self.safepoint_fuel else {
                    return Ok(());
                };
                let fuel = self.builder.use_var(fuel_variable);
                let remaining = self.builder.ins().iadd_imm(fuel, -1);
                self.builder.def_var(fuel_variable, remaining);
                let exhausted = self.builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
                let slow_block = self.builder.create_block();
                let continue_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(exhausted, slow_block, &[], continue_block, &[]);

                self.builder.switch_to_block(slow_block);
                let reset = self
                    .builder
                    .ins()
                    .iconst(types::I64, NATIVE_LOOP_SAFEPOINT_INTERVAL as i64);
                self.builder.def_var(fuel_variable, reset);
                self.builder.ins().call(self.yield_now, &[]);
                self.builder.ins().jump(continue_block, &[]);

                self.builder.switch_to_block(continue_block);
            }
            Instruction::BeginLoan { loan, source, .. } => {
                let source = self.resolve_view_place(source)?;
                self.view_places.insert(loan.clone(), source);
            }
            Instruction::BeginReturnedLoan {
                loan,
                origin,
                projections,
                ..
            } => {
                let mut encoded = Vec::new();
                for (index, projection) in projections.iter().enumerate() {
                    if index != 0 {
                        encoded.push(0);
                    }
                    encoded.extend_from_slice(projection.as_bytes());
                }
                let (projections_ptr, projections_len) = self.string_constant(&encoded)?;
                let selected = self.builder.ins().call(
                    self.take_returned_view_projection,
                    &[projections_ptr, projections_len],
                );
                let selected = self.builder.inst_results(selected)[0];
                let selector_var = *self.view_selector_vars.get(loan).ok_or_else(|| {
                    format!(
                        "direct backend has no returned-view selector storage for loan `{loan}`"
                    )
                })?;
                let selector_tags =
                    self.view_selector_tags.get(loan).cloned().ok_or_else(|| {
                        format!(
                            "direct backend has no returned-view selector tags for loan `{loan}`"
                        )
                    })?;
                let mut canonical_selector = self.builder.ins().iconst(types::I64, -1);
                for (index, projection) in projections.iter().enumerate() {
                    let tag = *selector_tags.get(projection).ok_or_else(|| {
                        format!(
                            "direct backend has no selector tag for projection `{projection}` on loan `{loan}`"
                        )
                    })?;
                    let matches = self.builder.ins().icmp_imm(
                        IntCC::Equal,
                        selected,
                        i64::try_from(index).map_err(|_| {
                            format!(
                                "direct backend returned-view projection index overflows for loan `{loan}`"
                            )
                        })?,
                    );
                    let tag = self.builder.ins().iconst(types::I64, tag);
                    canonical_selector =
                        self.builder.ins().select(matches, tag, canonical_selector);
                }
                self.builder.def_var(selector_var, canonical_selector);
                let origin = self.resolve_view_place(origin)?;
                let mut alternatives = Vec::new();
                for origin in origin.alternatives {
                    for projection in projections {
                        let mut conditions = origin.conditions.clone();
                        let tag = *selector_tags.get(projection).ok_or_else(|| {
                            format!(
                                "direct backend has no selector tag for projection `{projection}` on loan `{loan}`"
                            )
                        })?;
                        conditions.push((selector_var, tag));
                        let alternative = DirectViewAlternative {
                            place: if projection.is_empty() {
                                origin.place.clone()
                            } else {
                                format!("{}.{}", origin.place, projection)
                            },
                            conditions,
                        };
                        if !alternatives.contains(&alternative) {
                            alternatives.push(alternative);
                        }
                    }
                }
                self.view_places
                    .insert(loan.clone(), DirectViewPlace { alternatives });
            }
            Instruction::Reborrow {
                loan,
                parent,
                projection,
                ..
            } => {
                let source = self.resolve_view_place(parent)?.project(projection);
                self.view_places.insert(loan.clone(), source);
            }
            Instruction::ReadLoan { target, loan } => {
                let loaded = self.load_place(loan)?;
                let target_ty = self.type_of_place(target)?;
                let loaded = self.coerce_value(loaded, &target_ty)?;
                self.store_place(target, loaded)?;
            }
            Instruction::WriteLoan { loan, value } => {
                let target_ty = self.type_of_place(loan)?;
                let compiled = self.compile_rvalue_for_target(value, &target_ty)?;
                let coerced = self.coerce_value(compiled, &target_ty)?;
                self.store_place(loan, coerced)?;
            }
            Instruction::EndLoan { loan } => {
                self.view_places.remove(loan);
            }
            Instruction::ReturnLoan { loan, origin } => {
                self.emit_returned_view_projection(loan, origin)?;
                let loan_root = loan.split('.').next().unwrap_or(loan);
                self.view_places.remove(loan_root);
            }
            Instruction::Assign { target, value } => {
                if let Rvalue::Closure { captures, .. } = value {
                    let root = target.split('.').next().unwrap_or(target).to_string();
                    let selector = if captures
                        .iter()
                        .any(|capture| capture.passing == MirReceiverKind::BorrowMut)
                    {
                        let selector = *self.closure_selector_vars.get(&root).ok_or_else(|| {
                            format!("direct backend has no closure writeback selector for `{root}`")
                        })?;
                        let tag = *self
                            .closure_selector_tags
                            .get(&(block_label.to_string(), instruction_index))
                            .ok_or_else(|| {
                                format!(
                                    "direct backend has no closure writeback tag for instruction {instruction_index} in `{block_label}`"
                                )
                            })?;
                        let selected = self.builder.ins().iconst(types::I64, tag);
                        self.builder.def_var(selector, selected);
                        Some((selector, tag))
                    } else {
                        None
                    };
                    let writebacks = captures
                        .iter()
                        .enumerate()
                        .filter_map(|(index, capture)| {
                            (capture.passing == MirReceiverKind::BorrowMut)
                                .then_some((index, capture))
                        })
                        .map(|(index, capture)| {
                            let source = capture.source_place.clone().ok_or_else(|| {
                                format!(
                                    "direct backend mutable closure capture `{}` has no source place",
                                    capture.name
                                )
                            })?;
                            let ty = ensure_direct_type(
                                &capture.ty,
                                &self.classes,
                                &format!("mutable closure capture `{}`", capture.name),
                            )?;
                            let place = if capture.resolve_source_at_capture {
                                let resolved = self.resolve_view_place(&source)?;
                                self.snapshot_view_place_selectors(resolved)?
                            } else {
                                DirectViewPlace::static_place(source)
                            };
                            let place = if let Some((selector, tag)) = selector {
                                place.conditioned(selector, tag)
                            } else {
                                place
                            };
                            Ok(DirectClosureCaptureWriteback { index, place, ty })
                        })
                        .collect::<std::result::Result<Vec<_>, String>>()?;
                    if writebacks.is_empty() {
                        self.closure_capture_writebacks.remove(&root);
                    } else {
                        self.closure_capture_writebacks.insert(root, writebacks);
                    }
                } else if let Rvalue::Use(Operand::Place(source) | Operand::MovePlace(source)) =
                    value
                {
                    let source_root = source.split('.').next().unwrap_or(source);
                    let target_root = target.split('.').next().unwrap_or(target).to_string();
                    match self.closure_capture_writebacks.get(source_root).cloned() {
                        Some(writebacks) => {
                            self.closure_capture_writebacks
                                .insert(target_root, writebacks);
                        }
                        None => {
                            self.closure_capture_writebacks.remove(&target_root);
                        }
                    }
                }
                if let Rvalue::Try { value: try_value } = value {
                    let target_ty = self.type_of_place(target)?;
                    self.compile_try_assign(target, target_ty, try_value)?;
                } else {
                    let target_ty = self.type_of_place(target)?;
                    let compiled = self.compile_rvalue_for_target(value, &target_ty)?;
                    let assignment_span = match value {
                        Rvalue::Unary { span, .. }
                        | Rvalue::Cast { span, .. }
                        | Rvalue::Binary { span, .. } => Some(*span),
                        _ => None,
                    };
                    let coerced = self.coerce_value_at(compiled, &target_ty, assignment_span)?;
                    self.store_place(target, coerced)?;
                }
            }
            Instruction::Eval { value } => {
                let _ = self.load_operand(value)?;
            }
            Instruction::PushCleanup { place } => {
                self.set_cleanup_active(place, true)?;
                self.register_cleanup_for_place(place)?;
            }
            Instruction::PopCleanup {
                place,
                cancel_before_cleanup,
            } => {
                self.unregister_cleanup_for_place(place)?;
                self.set_cleanup_active(place, false)?;
                self.emit_cleanup_for_place(place, *cancel_before_cleanup)?;
            }
        }
        self.release_all_temporary_owned();
        Ok(())
    }

    fn compile_terminator(
        &mut self,
        terminator: &Terminator,
        return_ty: &DirectType,
    ) -> std::result::Result<(), String> {
        match terminator {
            Terminator::Return(operand) => {
                let value = self.load_operand_for_target(operand, return_ty)?;
                let coerced = self.coerce_value(value, return_ty)?;
                self.emit_return_value(coerced)?;
            }
            Terminator::Goto(label) => {
                self.release_all_temporary_owned();
                let block = self.compiled_block(label)?;
                self.builder.ins().jump(block, &[]);
            }
            Terminator::Branch {
                condition,
                then_label,
                else_label,
            } => {
                let condition = self.load_operand(condition)?;
                let condition = self.as_bool_value(condition)?;
                let then_block = self.compiled_block(then_label)?;
                let else_block = self.compiled_block(else_label)?;
                self.release_all_temporary_owned();
                self.builder
                    .ins()
                    .brif(condition, then_block, &[], else_block, &[]);
            }
            Terminator::Match {
                scrutinee,
                arms,
                otherwise,
            } => {
                let scrutinee = self.load_operand(scrutinee)?;
                let DirectType::Opaque(scrutinee_ty) = &scrutinee.ty else {
                    return Err(
                        "direct backend expected enum matches to use opaque scrutinees".to_string(),
                    );
                };
                let scrutinee_enum_name = match scrutinee_ty {
                    Type::Named(name, _) => name.as_str(),
                    other => {
                        return Err(format!(
                            "direct backend expected match scrutinee to carry an enum type name, found `{}`",
                            other
                        ))
                    }
                };
                for arm in arms {
                    if arm.wildcard {
                        self.release_all_temporary_owned();
                        let arm_block = self.compiled_block(&arm.label)?;
                        self.builder.ins().jump(arm_block, &[]);
                        return Ok(());
                    }
                    let next_block = self.builder.create_block();
                    let matched = self.variant_matches_value(
                        scrutinee.values[0],
                        arm.enum_name.as_deref().unwrap_or(scrutinee_enum_name),
                        arm.variant_name.as_deref().unwrap_or_default(),
                    )?;
                    let arm_block = self.compiled_block(&arm.label)?;
                    let matched_cleanup = self.builder.create_block();
                    let pending_owned = self.owned_opaque_temporaries.clone();
                    self.builder
                        .ins()
                        .brif(matched, matched_cleanup, &[], next_block, &[]);
                    self.builder.switch_to_block(matched_cleanup);
                    self.release_all_temporary_owned();
                    self.builder.ins().jump(arm_block, &[]);
                    self.builder.seal_block(matched_cleanup);
                    self.builder.switch_to_block(next_block);
                    self.owned_opaque_temporaries = pending_owned;
                }
                self.release_all_temporary_owned();
                let otherwise = self.compiled_block(otherwise)?;
                self.builder.ins().jump(otherwise, &[]);
            }
            Terminator::ForRange {
                binding,
                iterable,
                body_label,
                exit_label,
            } => {
                self.compile_for_range(binding, iterable, body_label, exit_label)?;
                self.release_all_temporary_owned();
            }
            Terminator::AssertFail {
                message,
                captures,
                span,
            } => {
                let message = match message {
                    Some(message) => {
                        let message = self.load_operand(message)?;
                        match &message.ty {
                            DirectType::Opaque(Type::Named(name, args))
                                if name == "str" && args.is_empty() => {}
                            other => {
                                return Err(format!(
                                    "direct backend expected an assertion message to be `str`, found `{}`",
                                    render_direct_type(other)
                                ))
                            }
                        }
                        message.values[0]
                    }
                    None => self.builder.ins().iconst(types::I64, 0),
                };
                let (line, column) = self.span_values(Some(*span));
                match captures.as_slice() {
                    [] => {
                        self.builder
                            .ins()
                            .call(self.assert_fail, &[message, line, column]);
                    }
                    [left, right] => {
                        let left_label = self.string_value(&left.label)?.values[0];
                        let left_type = self.string_value(&left.ty.to_string())?.values[0];
                        let left_value = self.load_assertion_capture_value(&left.value)?;
                        let right_label = self.string_value(&right.label)?.values[0];
                        let right_type = self.string_value(&right.ty.to_string())?.values[0];
                        let right_value = self.load_assertion_capture_value(&right.value)?;
                        self.builder.ins().call(
                            self.assert_fail_detailed,
                            &[
                                message,
                                line,
                                column,
                                left_label,
                                left_type,
                                left_value,
                                right_label,
                                right_type,
                                right_value,
                            ],
                        );
                    }
                    _ => {
                        return Err(
                            "direct backend requires exactly two assertion captures when captures are present"
                                .to_string(),
                        )
                    }
                }
                self.builder.ins().trap(TrapCode::unwrap_user(1));
            }
            other => {
                return Err(format!(
                    "direct backend does not support MIR terminator `{:?}`",
                    other
                ))
            }
        }
        Ok(())
    }

    fn load_assertion_capture_value(
        &mut self,
        operand: &Operand,
    ) -> std::result::Result<Value, String> {
        let value = self.load_operand(operand)?;
        match &value.ty {
            DirectType::Opaque(Type::Named(name, args)) if name == "str" && args.is_empty() => {
                Ok(value.values[0])
            }
            other => Err(format!(
                "direct backend expected a rendered assertion capture to be `str`, found `{}`",
                render_direct_type(other)
            )),
        }
    }

    fn emit_return_value(&mut self, value: ValueRef) -> std::result::Result<(), String> {
        let mut return_values = self.export_return_value(value);
        self.emit_pending_cleanups(true)?;
        self.append_writeback_return_values(&mut return_values)?;
        self.release_all_temporary_owned();
        self.release_all_opaque_roots()?;
        self.builder.ins().call(self.exit_call, &[]);
        self.builder.ins().return_(&return_values);
        Ok(())
    }

    fn compile_rvalue_for_target(
        &mut self,
        rvalue: &Rvalue,
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        match rvalue {
            Rvalue::Use(operand) => {
                let integer_hint = target.scalar_kind().filter(|kind| kind.is_integer());
                self.load_operand_with_integer_hint(operand, integer_hint)
            }
            Rvalue::ModuleConstant { key, initializer } => {
                let thunk = *self.function_thunk_refs.get(initializer).ok_or_else(|| {
                    format!(
                        "direct backend cannot find module constant initializer `{initializer}`"
                    )
                })?;
                let thunk_ptr = self.builder.ins().func_addr(types::I64, thunk);
                let (key_ptr, key_len) = self.string_constant(key.as_bytes())?;
                let call = self
                    .builder
                    .ins()
                    .call(self.module_constant, &[key_ptr, key_len, thunk_ptr]);
                let value = self.owned_opaque_result(
                    self.builder.inst_results(call).to_vec(),
                    Type::named("Unknown"),
                );
                self.coerce_value(value, target)
            }
            Rvalue::Closure {
                function,
                signature,
                captures,
                consuming,
            } => self.compile_closure(function, signature, captures, *consuming),
            Rvalue::FormatString { parts } => self.compile_format_string(parts),
            Rvalue::Unary { op, value, span } => {
                let integer_hint = target.scalar_kind().filter(|kind| kind.is_integer());
                if matches!(op, UnaryOp::Neg)
                    && matches!(integer_hint, Some(ScalarKind::Int64))
                    && matches!(value, Operand::Int(magnitude) if *magnitude == (i64::MAX as u128) + 1)
                {
                    return Ok(ValueRef {
                        values: vec![self.builder.ins().iconst(types::I64, i64::MIN)],
                        ty: DirectType::Scalar(ScalarKind::Int64),
                    });
                }
                let value = self.load_operand_with_integer_hint(value, integer_hint)?;
                self.compile_unary(*op, value, Some(*span))
            }
            Rvalue::Cast { value, ty, span } => {
                let cast_target = ensure_direct_type(ty, &self.classes, "cast target")?;
                let value = self.load_operand_for_target(value, &cast_target)?;
                self.compile_cast(value, ty, &cast_target, Some(*span))
            }
            Rvalue::Binary {
                op,
                left,
                right,
                span,
            } => {
                let target_integer_hint = target.scalar_kind().filter(|kind| kind.is_integer());
                let left_integer_hint = target_integer_hint
                    .or_else(|| self.operand_integer_kind(left))
                    .or_else(|| {
                        matches!(left, Operand::Int(_))
                            .then(|| self.operand_integer_kind(right))
                            .flatten()
                    });
                let right_integer_hint = target_integer_hint
                    .or_else(|| self.operand_integer_kind(right))
                    .or_else(|| {
                        matches!(right, Operand::Int(_))
                            .then(|| self.operand_integer_kind(left))
                            .flatten()
                    });
                let left = self.load_operand_with_integer_hint(left, left_integer_hint)?;
                let right = self.load_operand_with_integer_hint(right, right_integer_hint)?;
                self.compile_binary(*op, left, right, Some(*span))
            }
            Rvalue::Call { callee, args } => self.compile_call(callee, args, target),
            Rvalue::VecLiteral {
                elements,
                element_type,
            } => self.compile_vec_literal_for_target(elements, element_type, Some(target)),
            Rvalue::TupleLiteral {
                elements,
                element_types,
            } => {
                let tuple_type = match target {
                    DirectType::Opaque(Type::Tuple(target_elements)) => {
                        Type::Tuple(target_elements.clone())
                    }
                    _ => Type::Tuple(element_types.clone()),
                };
                self.compile_tuple_literal(elements, element_types, tuple_type)
            }
            Rvalue::TupleElement {
                tuple,
                index,
                element_type,
            } => self.compile_tuple_element(tuple, *index, element_type),
            Rvalue::TupleTakeElement {
                place,
                index,
                element_type,
            } => self.compile_take_tuple_element(place, *index, element_type),
            Rvalue::MapLiteral {
                entries,
                key_type,
                value_type,
            } => self.compile_map_literal_for_target(entries, key_type, value_type, Some(target)),
            Rvalue::SetLiteral {
                elements,
                element_type,
            } => self.compile_set_literal_for_target(elements, element_type, Some(target)),
            Rvalue::Construct { class_name, fields } => {
                self.compile_construct(class_name, fields, target)
            }
            Rvalue::Member { object, field } => {
                let object = self.load_operand(object)?;
                self.extract_field(object, field)
            }
            Rvalue::EnumVariant {
                enum_name,
                variant_name,
                payloads,
            } => self.compile_enum_variant_for_target(
                enum_name,
                variant_name,
                payloads,
                Some(target),
            ),
            Rvalue::VariantPayload {
                scrutinee,
                variant_name: _,
                index,
            } => {
                if let Operand::MovePlace(place) = scrutinee {
                    return self.compile_take_variant_payload(place, *index);
                }
                let scrutinee = self.load_operand(scrutinee)?;
                self.compile_variant_payload(scrutinee, *index)
            }
            Rvalue::StartTask {
                returns_handle,
                result_is_copy,
                stack_size,
                task_group,
                function,
                args,
                span,
            } => self.compile_start_task(TaskStart {
                mode: TaskStartMode {
                    returns_handle: *returns_handle,
                    result_is_copy: *result_is_copy,
                },
                stack_size: stack_size.as_ref(),
                task_group,
                function,
                args,
                spawn_span: *span,
                target,
            }),
            Rvalue::Try { .. } => unreachable!("try rvalues are handled before target lowering"),
        }
    }

    fn compile_closure(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[crate::mir::MirClosureCapture],
        consuming: bool,
    ) -> std::result::Result<ValueRef, String> {
        let function_operand = Operand::Function {
            name: function.to_string(),
            signature: Box::new(signature.clone()),
        };
        let loaded_function = self.load_operand(&function_operand)?;
        let function_value = self.ensure_opaque(loaded_function)?;
        let function_value = self.transfer_owned_opaque_value(&function_value);
        let count = self.builder.ins().iconst(types::I64, captures.len() as i64);
        let buffer = if captures.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let call = self.builder.ins().call(self.arg_buffer_new, &[count]);
            self.builder.inst_results(call)[0]
        };
        for (index, capture) in captures.iter().enumerate() {
            let capture_ty = ensure_direct_type(
                &capture.ty,
                &self.classes,
                &format!("closure capture `{}`", capture.name),
            )?;
            let value = self.load_operand_for_target(&capture.value, &capture_ty)?;
            let value = self.coerce_value(value, &capture_ty)?;
            let value = self.ensure_opaque(value)?;
            let value = self.transfer_owned_opaque_value(&value);
            let index = self.builder.ins().iconst(types::I64, index as i64);
            self.builder
                .ins()
                .call(self.arg_buffer_store_owned, &[buffer, index, value]);
        }
        let mode_buffer_size = u32::try_from(captures.len().max(1).saturating_mul(8))
            .ok()
            .ok_or("direct backend closure capture-mode buffer is too large".to_string())?;
        let mode_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            mode_buffer_size,
            3,
        ));
        let modes = self.builder.ins().stack_addr(types::I64, mode_slot, 0);
        for (index, capture) in captures.iter().enumerate() {
            let mutable = self.builder.ins().iconst(
                types::I64,
                i64::from(capture.passing == MirReceiverKind::BorrowMut),
            );
            self.builder
                .ins()
                .store(MemFlags::new(), mutable, modes, (index as i32) * 8);
        }
        let consuming = self.builder.ins().iconst(types::I64, i64::from(consuming));
        let call = self.builder.ins().call(
            self.closure_value,
            &[function_value, buffer, count, modes, consuming],
        );
        Ok(self.owned_opaque_result(self.builder.inst_results(call).to_vec(), signature.clone()))
    }

    fn compile_vec_literal_for_target(
        &mut self,
        elements: &[Operand],
        element_type: &Type,
        target: Option<&DirectType>,
    ) -> std::result::Result<ValueRef, String> {
        let resolved_element_type = match target {
            Some(DirectType::Opaque(Type::Named(name, args)))
                if name == "list" && args.len() == 1 =>
            {
                args[0].clone()
            }
            _ => element_type.clone(),
        };
        let element_direct_ty =
            ensure_direct_type(&resolved_element_type, &self.classes, "Vec element")?;
        let init = self.builder.ins().call(self.vec_empty, &[]);
        let vector = self.owned_opaque_result(
            self.builder.inst_results(init).to_vec(),
            Type::Named("list".to_string(), vec![resolved_element_type]),
        );
        for element in elements {
            let value = self.load_operand_as_opaque_direct(element, &element_direct_ty)?;
            let value = self.transfer_owned_opaque_value(&value);
            let result = self
                .builder
                .ins()
                .call(self.vec_push_in_place, &[vector.values[0], value]);
            self.release_opaque_handle(self.builder.inst_results(result)[0]);
        }
        Ok(vector)
    }

    fn compile_map_literal_for_target(
        &mut self,
        entries: &[MirMapEntry],
        key_type: &Type,
        value_type: &Type,
        target: Option<&DirectType>,
    ) -> std::result::Result<ValueRef, String> {
        let (resolved_key_type, resolved_value_type) = match target {
            Some(DirectType::Opaque(Type::Named(name, args)))
                if name == "dict" && args.len() == 2 =>
            {
                (args[0].clone(), args[1].clone())
            }
            _ => (key_type.clone(), value_type.clone()),
        };
        let key_direct_ty = ensure_direct_type(&resolved_key_type, &self.classes, "dict key")?;
        let value_direct_ty = ensure_direct_type(&resolved_value_type, &self.classes, "Map value")?;
        let init = self.builder.ins().call(self.map_empty, &[]);
        let map = self.owned_opaque_result(
            self.builder.inst_results(init).to_vec(),
            Type::Named(
                "dict".to_string(),
                vec![resolved_key_type, resolved_value_type],
            ),
        );
        for entry in entries {
            let key = self.load_operand_as_opaque_direct(&entry.key, &key_direct_ty)?;
            let value = self.load_operand_as_opaque_direct(&entry.value, &value_direct_ty)?;
            let key = self.transfer_owned_opaque_value(&key);
            let value = self.transfer_owned_opaque_value(&value);
            let result = self
                .builder
                .ins()
                .call(self.map_set_in_place, &[map.values[0], key, value]);
            self.release_opaque_handle(self.builder.inst_results(result)[0]);
        }
        Ok(map)
    }

    fn compile_set_literal_for_target(
        &mut self,
        elements: &[Operand],
        element_type: &Type,
        target: Option<&DirectType>,
    ) -> std::result::Result<ValueRef, String> {
        let resolved_element_type = match target {
            Some(DirectType::Opaque(Type::Named(name, args)))
                if name == "set" && args.len() == 1 =>
            {
                args[0].clone()
            }
            _ => element_type.clone(),
        };
        let element_direct_ty =
            ensure_direct_type(&resolved_element_type, &self.classes, "set element")?;
        let init = self.builder.ins().call(self.set_empty, &[]);
        let set = self.owned_opaque_result(
            self.builder.inst_results(init).to_vec(),
            Type::Named("set".to_string(), vec![resolved_element_type]),
        );
        for element in elements {
            let value = self.load_operand_as_opaque_direct(element, &element_direct_ty)?;
            let value = self.transfer_owned_opaque_value(&value);
            let _ = self
                .builder
                .ins()
                .call(self.set_insert_in_place, &[set.values[0], value]);
        }
        Ok(set)
    }

    fn compile_unary(
        &mut self,
        op: UnaryOp,
        value: ValueRef,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        if matches!(value.ty, DirectType::Opaque(_)) || op == UnaryOp::BitNot {
            let target_ty = value.ty.clone();
            let value = self.ensure_opaque(value)?;
            let opcode = match op {
                UnaryOp::Neg => 0,
                UnaryOp::Not => 1,
                UnaryOp::BitNot => 2,
            };
            let opcode = self.builder.ins().iconst(types::I64, opcode);
            let (line, column) = self.span_values(span);
            let inst = self
                .builder
                .ins()
                .call(self.unary_value, &[opcode, value.values[0], line, column]);
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return if matches!(target_ty, DirectType::Opaque(_)) {
                Ok(result)
            } else {
                self.coerce_value(result, &target_ty)
            };
        }
        match (op, value.ty.scalar_kind()) {
            (UnaryOp::Neg, Some(ScalarKind::Int32)) => Ok(ValueRef {
                values: vec![self.builder.ins().ineg(value.values[0])],
                ty: DirectType::Scalar(ScalarKind::Int32),
            }),
            (UnaryOp::Neg, Some(ScalarKind::Int64)) => self.compile_wide_integer_negation(
                WideIntegerKind::Int64,
                value.values[0],
                span,
            ),
            (UnaryOp::Neg, Some(ScalarKind::Uint64)) => self.compile_wide_integer_negation(
                WideIntegerKind::Uint64,
                value.values[0],
                span,
            ),
            (UnaryOp::Neg, Some(kind)) if kind.is_float() => Ok(ValueRef {
                values: vec![self.builder.ins().fneg(value.values[0])],
                ty: DirectType::Scalar(kind),
            }),
            (UnaryOp::Not, Some(ScalarKind::Bool)) => {
                let zero = self.builder.ins().iconst(types::I64, 0);
                let cmp = self.builder.ins().icmp(IntCC::Equal, value.values[0], zero);
                Ok(ValueRef {
                    values: vec![self.builder.ins().uextend(types::I64, cmp)],
                    ty: DirectType::Scalar(ScalarKind::Bool),
                })
            }
            _ => Err(format!(
                "direct backend does not support unary operation `{:?}` for the current operand type",
                op
            )),
        }
    }

    fn compile_wide_integer_negation(
        &mut self,
        kind: WideIntegerKind,
        value: Value,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        let zero = self.builder.ins().iconst(types::I64, 0);
        let (result, overflow) = if kind.is_signed() {
            self.builder.ins().ssub_overflow(zero, value)
        } else {
            self.builder.ins().usub_overflow(zero, value)
        };
        self.emit_integer_overflow_failure_branch(
            overflow,
            kind,
            WideOverflowOp::Sub,
            zero,
            value,
            span,
        )?;
        Ok(ValueRef {
            values: vec![result],
            ty: DirectType::Scalar(kind.scalar_kind()),
        })
    }

    fn compile_cast(
        &mut self,
        value: ValueRef,
        target: &Type,
        target_ty: &DirectType,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        if matches!(value.ty, DirectType::Opaque(_)) || matches!(target_ty, DirectType::Opaque(_)) {
            let boxed = self.ensure_opaque(value)?;
            let (target_ptr, target_len) = self.string_constant(target.to_string().as_bytes())?;
            let (line, column) = self.span_values(span);
            let inst = self.builder.ins().call(
                self.cast_value,
                &[boxed.values[0], target_ptr, target_len, line, column],
            );
            let boxed =
                self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), target.clone());
            return self.coerce_value(boxed, target_ty);
        }
        let Some(target_kind) = target_ty.scalar_kind() else {
            return Err(format!(
                "direct backend only supports numeric casts, found target `{}`",
                target
            ));
        };
        let Some(source_kind) = value.ty.scalar_kind() else {
            return Err(format!(
                "direct backend only supports numeric casts from scalar values, found `{}`",
                render_direct_type(&value.ty)
            ));
        };

        let source = value.values[0];
        let result = match (source_kind, target_kind) {
            (_, _) if source_kind == target_kind => source,
            (source_kind, target_kind) if source_kind.is_integer() && target_kind.is_integer() => {
                let source_code = self.builder.ins().iconst(
                    types::I64,
                    if matches!(source_kind, ScalarKind::Uint64) {
                        1
                    } else {
                        0
                    },
                );
                let target_code = self.builder.ins().iconst(
                    types::I64,
                    match target_kind {
                        ScalarKind::Int32 => 0,
                        ScalarKind::Int64 => 1,
                        ScalarKind::Uint64 => 2,
                        _ => unreachable!("integer cast target should be exhaustive"),
                    },
                );
                let (line, column) = self.span_values(span);
                let inst = self.builder.ins().call(
                    self.cast_integer_to_integer,
                    &[source, source_code, target_code, line, column],
                );
                self.builder.inst_results(inst)[0]
            }
            (source_kind, target_kind) if source_kind.is_integer() && target_kind.is_float() => {
                let source_code = self.builder.ins().iconst(
                    types::I64,
                    if matches!(source_kind, ScalarKind::Uint64) {
                        1
                    } else {
                        0
                    },
                );
                let target_code = self.builder.ins().iconst(
                    types::I64,
                    if matches!(target_kind, ScalarKind::Float32) {
                        0
                    } else {
                        1
                    },
                );
                let (line, column) = self.span_values(span);
                let inst = self.builder.ins().call(
                    self.cast_integer_to_float,
                    &[source, source_code, target_code, line, column],
                );
                self.builder.inst_results(inst)[0]
            }
            (source_kind, target_kind) if source_kind.is_float() && target_kind.is_integer() => {
                let target_code = self.builder.ins().iconst(
                    types::I64,
                    match target_kind {
                        ScalarKind::Int32 => 0,
                        ScalarKind::Int64 => 1,
                        ScalarKind::Uint64 => 2,
                        _ => unreachable!("integer cast target should be exhaustive"),
                    },
                );
                let (line, column) = self.span_values(span);
                let inst = self.builder.ins().call(
                    self.cast_float_to_integer,
                    &[source, target_code, line, column],
                );
                self.builder.inst_results(inst)[0]
            }
            (lhs, rhs) if lhs.is_float() && rhs.is_float() => source,
            _ => {
                return Err(format!(
                    "direct backend only supports numeric casts, found `{}` to `{}`",
                    render_direct_type(&value.ty),
                    target
                ));
            }
        };

        Ok(ValueRef {
            values: vec![result],
            ty: DirectType::Scalar(target_kind),
        })
    }

    fn compile_binary(
        &mut self,
        op: BinaryOp,
        left: ValueRef,
        right: ValueRef,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        if matches!(
            op,
            BinaryOp::Pow
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::Shl
                | BinaryOp::Shr
        ) {
            let target_ty = left.ty.clone();
            let float_width = match &target_ty {
                DirectType::Scalar(ScalarKind::Float32) => 32,
                DirectType::Scalar(ScalarKind::Float64) => 64,
                _ => 0,
            };
            let left = self.ensure_opaque(left)?;
            let right = self.ensure_opaque(right)?;
            let opcode_value = Self::binary_opcode(op);
            let opcode = self.builder.ins().iconst(types::I64, opcode_value);
            let float_width = self.builder.ins().iconst(types::I64, float_width);
            let (line, column) = self.span_values(span);
            let inst = self.builder.ins().call(
                self.binary_value,
                &[
                    opcode,
                    left.values[0],
                    right.values[0],
                    float_width,
                    line,
                    column,
                ],
            );
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return if matches!(target_ty, DirectType::Opaque(_)) {
                Ok(result)
            } else {
                self.coerce_value(result, &target_ty)
            };
        }
        let left_array_element = direct_array_element_type(&left.ty).cloned();
        let right_array_element = direct_array_element_type(&right.ty).cloned();
        if left_array_element.is_some() || right_array_element.is_some() {
            let array_ty = if left_array_element.is_some() {
                direct_type_to_type(&left.ty)
            } else {
                direct_type_to_type(&right.ty)
            };
            let element_type = left_array_element
                .as_ref()
                .or(right_array_element.as_ref())
                .expect("Array operation has an Array dtype");
            let element_direct =
                ensure_direct_type(element_type, &self.classes, "Array scalar operand")?;
            let scalar_left = i64::from(left_array_element.is_none());
            let left = if left_array_element.is_some() {
                left
            } else {
                self.coerce_value_at(left, &element_direct, span)?
            };
            let right = if right_array_element.is_some() {
                right
            } else {
                self.coerce_value_at(right, &element_direct, span)?
            };
            let left = self.ensure_opaque(left)?;
            let right = self.ensure_opaque(right)?;
            let scalar_left = self.builder.ins().iconst(types::I64, scalar_left);
            let operation = self
                .builder
                .ins()
                .iconst(types::I64, direct_array_binary_opcode(op)?);
            let checked_mode = self.builder.ins().iconst(types::I64, 0);
            let (line, column) = self.span_values(span);
            let inst = self.builder.ins().call(
                self.array_binary,
                &[
                    left.values[0],
                    right.values[0],
                    scalar_left,
                    operation,
                    checked_mode,
                    line,
                    column,
                ],
            );
            return Ok(self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), array_ty));
        }
        if matches!(left.ty, DirectType::Opaque(_)) || matches!(right.ty, DirectType::Opaque(_)) {
            let left = self.ensure_opaque(left)?;
            let right = self.ensure_opaque(right)?;
            let binary_opcode = Self::binary_opcode(op);
            let opcode = self.builder.ins().iconst(types::I64, binary_opcode);
            let float_width = self.builder.ins().iconst(types::I64, 0);
            let (line, column) = self.span_values(span);
            let inst = self.builder.ins().call(
                self.binary_value,
                &[
                    opcode,
                    left.values[0],
                    right.values[0],
                    float_width,
                    line,
                    column,
                ],
            );
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            ));
        }
        match (left.ty.scalar_kind(), right.ty.scalar_kind()) {
            (Some(ScalarKind::Int32), Some(ScalarKind::Int32)) => {
                self.compile_int32_binary(op, left.values[0], right.values[0], span)
            }
            (Some(ScalarKind::Int64), Some(ScalarKind::Int64)) => self
                .compile_wide_integer_binary(
                    WideIntegerKind::Int64,
                    op,
                    left.values[0],
                    right.values[0],
                    span,
                ),
            (Some(ScalarKind::Uint64), Some(ScalarKind::Uint64)) => self
                .compile_wide_integer_binary(
                    WideIntegerKind::Uint64,
                    op,
                    left.values[0],
                    right.values[0],
                    span,
                ),
            (Some(lhs), Some(rhs)) if lhs.is_float() && rhs.is_float() => {
                self.compile_float_binary(op, left.values[0], right.values[0], lhs, span)
            }
            (Some(ScalarKind::Bool), Some(ScalarKind::Bool)) => {
                self.compile_bool_binary(op, left.values[0], right.values[0])
            }
            (Some(ScalarKind::Unit), Some(ScalarKind::Unit))
                if matches!(op, BinaryOp::Eq | BinaryOp::NotEq) =>
            {
                self.compile_bool_binary(op, left.values[0], right.values[0])
            }
            _ => Err(format!(
                "direct backend does not support binary operation `{:?}` for the current operand types",
                op
            )),
        }
    }

    fn compile_int32_binary(
        &mut self,
        op: BinaryOp,
        left: Value,
        right: Value,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        let ty = DirectType::Scalar(ScalarKind::Int32);
        let value = match op {
            BinaryOp::Add => ValueRef {
                values: vec![self.builder.ins().iadd(left, right)],
                ty,
            },
            BinaryOp::Sub => ValueRef {
                values: vec![self.builder.ins().isub(left, right)],
                ty,
            },
            BinaryOp::Mul => ValueRef {
                values: vec![self.builder.ins().imul(left, right)],
                ty,
            },
            BinaryOp::FloorDiv => {
                self.emit_int_division_guard(right, span)?;
                ValueRef {
                    values: vec![self.compile_signed_floor_divmod(left, right, true)],
                    ty,
                }
            }
            BinaryOp::Mod => {
                self.emit_int_division_guard(right, span)?;
                ValueRef {
                    values: vec![self.compile_signed_floor_divmod(left, right, false)],
                    ty,
                }
            }
            BinaryOp::Eq => self.boolean_from_icmp(IntCC::Equal, left, right),
            BinaryOp::NotEq => self.boolean_from_icmp(IntCC::NotEqual, left, right),
            BinaryOp::Less => self.boolean_from_icmp(IntCC::SignedLessThan, left, right),
            BinaryOp::LessEq => self.boolean_from_icmp(IntCC::SignedLessThanOrEqual, left, right),
            BinaryOp::Greater => self.boolean_from_icmp(IntCC::SignedGreaterThan, left, right),
            BinaryOp::GreaterEq => {
                self.boolean_from_icmp(IntCC::SignedGreaterThanOrEqual, left, right)
            }
            other => {
                return Err(format!(
                    "direct backend does not support integer binary operation `{:?}`",
                    other
                ))
            }
        };

        if matches!(value.ty.scalar_kind(), Some(ScalarKind::Int32)) {
            self.emit_int32_bounds_check(value.values[0], span)?;
        }
        Ok(value)
    }

    fn compile_signed_floor_divmod(&mut self, left: Value, right: Value, quotient: bool) -> Value {
        let truncating_quotient = self.builder.ins().sdiv(left, right);
        let truncating_remainder = self.builder.ins().srem(left, right);
        let zero = self.builder.ins().iconst(types::I64, 0);
        let remainder_nonzero =
            self.builder
                .ins()
                .icmp(IntCC::NotEqual, truncating_remainder, zero);
        let left_negative = self.builder.ins().icmp(IntCC::SignedLessThan, left, zero);
        let right_negative = self.builder.ins().icmp(IntCC::SignedLessThan, right, zero);
        let signs_differ = self.builder.ins().bxor(left_negative, right_negative);
        let adjust = self.builder.ins().band(remainder_nonzero, signs_differ);

        if quotient {
            let one = self.builder.ins().iconst(types::I64, 1);
            let adjusted = self.builder.ins().isub(truncating_quotient, one);
            self.builder
                .ins()
                .select(adjust, adjusted, truncating_quotient)
        } else {
            let adjusted = self.builder.ins().iadd(truncating_remainder, right);
            self.builder
                .ins()
                .select(adjust, adjusted, truncating_remainder)
        }
    }

    fn compile_wide_integer_binary(
        &mut self,
        kind: WideIntegerKind,
        op: BinaryOp,
        left: Value,
        right: Value,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        let ty = DirectType::Scalar(kind.scalar_kind());
        let arithmetic = |value| ValueRef {
            values: vec![value],
            ty: ty.clone(),
        };
        let value = match op {
            BinaryOp::Add => {
                let (result, overflow) = if kind.is_signed() {
                    self.builder.ins().sadd_overflow(left, right)
                } else {
                    self.builder.ins().uadd_overflow(left, right)
                };
                self.emit_integer_overflow_failure_branch(
                    overflow,
                    kind,
                    WideOverflowOp::Add,
                    left,
                    right,
                    span,
                )?;
                arithmetic(result)
            }
            BinaryOp::Sub => {
                let (result, overflow) = if kind.is_signed() {
                    self.builder.ins().ssub_overflow(left, right)
                } else {
                    self.builder.ins().usub_overflow(left, right)
                };
                self.emit_integer_overflow_failure_branch(
                    overflow,
                    kind,
                    WideOverflowOp::Sub,
                    left,
                    right,
                    span,
                )?;
                arithmetic(result)
            }
            BinaryOp::Mul => {
                let (result, overflow) = if kind.is_signed() {
                    self.builder.ins().smul_overflow(left, right)
                } else {
                    self.builder.ins().umul_overflow(left, right)
                };
                self.emit_integer_overflow_failure_branch(
                    overflow,
                    kind,
                    WideOverflowOp::Mul,
                    left,
                    right,
                    span,
                )?;
                arithmetic(result)
            }
            BinaryOp::FloorDiv => {
                self.emit_int_division_guard(right, span)?;
                if kind.is_signed() {
                    let min = self.builder.ins().iconst(types::I64, i64::MIN);
                    let negative_one = self.builder.ins().iconst(types::I64, -1);
                    let is_min = self.builder.ins().icmp(IntCC::Equal, left, min);
                    let is_negative_one =
                        self.builder.ins().icmp(IntCC::Equal, right, negative_one);
                    let overflow = self.builder.ins().band(is_min, is_negative_one);
                    self.emit_integer_overflow_failure_branch(
                        overflow,
                        kind,
                        WideOverflowOp::Div,
                        left,
                        right,
                        span,
                    )?;
                    arithmetic(self.compile_signed_floor_divmod(left, right, true))
                } else {
                    arithmetic(self.builder.ins().udiv(left, right))
                }
            }
            BinaryOp::Mod => {
                self.emit_int_division_guard(right, span)?;
                if kind.is_signed() {
                    let min = self.builder.ins().iconst(types::I64, i64::MIN);
                    let negative_one = self.builder.ins().iconst(types::I64, -1);
                    let is_min = self.builder.ins().icmp(IntCC::Equal, left, min);
                    let is_negative_one =
                        self.builder.ins().icmp(IntCC::Equal, right, negative_one);
                    let exceptional = self.builder.ins().band(is_min, is_negative_one);
                    let normal_block = self.builder.create_block();
                    let continue_block = self.builder.create_block();
                    self.builder.append_block_param(continue_block, types::I64);
                    let zero_remainder = self.builder.ins().iconst(types::I64, 0);
                    self.builder.ins().brif(
                        exceptional,
                        continue_block,
                        &[zero_remainder],
                        normal_block,
                        &[],
                    );
                    self.builder.switch_to_block(normal_block);
                    let remainder = self.compile_signed_floor_divmod(left, right, false);
                    self.builder.ins().jump(continue_block, &[remainder]);
                    self.builder.seal_block(normal_block);
                    self.builder.switch_to_block(continue_block);
                    self.builder.seal_block(continue_block);
                    arithmetic(self.builder.block_params(continue_block)[0])
                } else {
                    arithmetic(self.builder.ins().urem(left, right))
                }
            }
            BinaryOp::Eq => self.boolean_from_icmp(IntCC::Equal, left, right),
            BinaryOp::NotEq => self.boolean_from_icmp(IntCC::NotEqual, left, right),
            BinaryOp::Less => self.boolean_from_icmp(
                if kind.is_signed() {
                    IntCC::SignedLessThan
                } else {
                    IntCC::UnsignedLessThan
                },
                left,
                right,
            ),
            BinaryOp::LessEq => self.boolean_from_icmp(
                if kind.is_signed() {
                    IntCC::SignedLessThanOrEqual
                } else {
                    IntCC::UnsignedLessThanOrEqual
                },
                left,
                right,
            ),
            BinaryOp::Greater => self.boolean_from_icmp(
                if kind.is_signed() {
                    IntCC::SignedGreaterThan
                } else {
                    IntCC::UnsignedGreaterThan
                },
                left,
                right,
            ),
            BinaryOp::GreaterEq => self.boolean_from_icmp(
                if kind.is_signed() {
                    IntCC::SignedGreaterThanOrEqual
                } else {
                    IntCC::UnsignedGreaterThanOrEqual
                },
                left,
                right,
            ),
            other => return Err(format!("{WIDE_INTEGER_BINARY_ERROR} `{other:?}`")),
        };
        Ok(value)
    }

    fn compile_float_binary(
        &mut self,
        op: BinaryOp,
        left: Value,
        right: Value,
        kind: ScalarKind,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        let ty = DirectType::Scalar(kind);
        match op {
            BinaryOp::Add => Ok(ValueRef {
                values: vec![self.builder.ins().fadd(left, right)],
                ty,
            }),
            BinaryOp::Sub => Ok(ValueRef {
                values: vec![self.builder.ins().fsub(left, right)],
                ty,
            }),
            BinaryOp::Mul => Ok(ValueRef {
                values: vec![self.builder.ins().fmul(left, right)],
                ty,
            }),
            BinaryOp::Div => {
                self.emit_float_division_guard(right, span)?;
                Ok(ValueRef {
                    values: vec![self.builder.ins().fdiv(left, right)],
                    ty,
                })
            }
            BinaryOp::FloorDiv | BinaryOp::Mod => {
                let opcode_value = Self::binary_opcode(op);
                let left_box = self.builder.ins().call(self.box_f64, &[left]);
                let right_box = self.builder.ins().call(self.box_f64, &[right]);
                let left_boxed = self.builder.inst_results(left_box)[0];
                let right_boxed = self.builder.inst_results(right_box)[0];
                let opcode = self.builder.ins().iconst(types::I64, opcode_value);
                let float_width = self.builder.ins().iconst(types::I64, 0);
                let (line, column) = self.span_values(span);
                let result = self.builder.ins().call(
                    self.binary_value,
                    &[opcode, left_boxed, right_boxed, float_width, line, column],
                );
                let result_boxed = self.builder.inst_results(result)[0];
                self.release_opaque_handle(left_boxed);
                self.release_opaque_handle(right_boxed);
                let unboxed = self.builder.ins().call(self.unbox_f64, &[result_boxed]);
                let value = self.builder.inst_results(unboxed)[0];
                self.release_opaque_handle(result_boxed);
                Ok(ValueRef {
                    values: vec![value],
                    ty,
                })
            }
            BinaryOp::Eq => Ok(self.boolean_from_fcmp(FloatCC::Equal, left, right)),
            BinaryOp::NotEq => Ok(self.boolean_from_fcmp(FloatCC::NotEqual, left, right)),
            BinaryOp::Less => Ok(self.boolean_from_fcmp(FloatCC::LessThan, left, right)),
            BinaryOp::LessEq => Ok(self.boolean_from_fcmp(FloatCC::LessThanOrEqual, left, right)),
            BinaryOp::Greater => Ok(self.boolean_from_fcmp(FloatCC::GreaterThan, left, right)),
            BinaryOp::GreaterEq => {
                Ok(self.boolean_from_fcmp(FloatCC::GreaterThanOrEqual, left, right))
            }
            other => Err(format!(
                "direct backend does not support float binary operation `{:?}`",
                other
            )),
        }
    }

    fn compile_bool_binary(
        &mut self,
        op: BinaryOp,
        left: Value,
        right: Value,
    ) -> std::result::Result<ValueRef, String> {
        match op {
            BinaryOp::Eq => Ok(self.boolean_from_icmp(IntCC::Equal, left, right)),
            BinaryOp::NotEq => Ok(self.boolean_from_icmp(IntCC::NotEqual, left, right)),
            BinaryOp::And => Ok(ValueRef {
                values: vec![self.builder.ins().band(left, right)],
                ty: DirectType::Scalar(ScalarKind::Bool),
            }),
            BinaryOp::Or => Ok(ValueRef {
                values: vec![self.builder.ins().bor(left, right)],
                ty: DirectType::Scalar(ScalarKind::Bool),
            }),
            other => Err(format!(
                "direct backend does not support boolean binary operation `{:?}`",
                other
            )),
        }
    }

    fn boolean_from_icmp(&mut self, cc: IntCC, left: Value, right: Value) -> ValueRef {
        let cmp = self.builder.ins().icmp(cc, left, right);
        ValueRef {
            values: vec![self.builder.ins().uextend(types::I64, cmp)],
            ty: DirectType::Scalar(ScalarKind::Bool),
        }
    }

    fn boolean_from_fcmp(&mut self, cc: FloatCC, left: Value, right: Value) -> ValueRef {
        let cmp = self.builder.ins().fcmp(cc, left, right);
        ValueRef {
            values: vec![self.builder.ins().uextend(types::I64, cmp)],
            ty: DirectType::Scalar(ScalarKind::Bool),
        }
    }

    fn emit_int_division_guard(
        &mut self,
        divisor: Value,
        span: Option<Span>,
    ) -> std::result::Result<(), String> {
        let zero = self.builder.ins().iconst(types::I64, 0);
        let is_zero = self.builder.ins().icmp(IntCC::Equal, divisor, zero);
        self.emit_division_failure_branch(is_zero, span)
    }

    fn emit_integer_overflow_failure_branch(
        &mut self,
        overflow: Value,
        kind: WideIntegerKind,
        op: WideOverflowOp,
        left: Value,
        right: Value,
        span: Option<Span>,
    ) -> std::result::Result<(), String> {
        let fail_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(overflow, fail_block, &[], continue_block, &[]);
        self.builder.switch_to_block(fail_block);
        self.emit_pending_cleanups(true)?;
        let kind_code = self.builder.ins().iconst(types::I64, kind as i64);
        let op_code = self.builder.ins().iconst(types::I64, op as i64);
        let (line, column) = self.span_values(span);
        self.builder.ins().call(
            self.fail_integer_overflow,
            &[kind_code, op_code, left, right, line, column],
        );
        self.builder.ins().trap(TrapCode::INTEGER_OVERFLOW);
        self.builder.seal_block(fail_block);
        self.builder.switch_to_block(continue_block);
        self.builder.seal_block(continue_block);
        Ok(())
    }

    fn emit_float_division_guard(
        &mut self,
        divisor: Value,
        span: Option<Span>,
    ) -> std::result::Result<(), String> {
        let zero = self.builder.ins().f64const(Ieee64::with_float(0.0));
        let is_zero = self.builder.ins().fcmp(FloatCC::Equal, divisor, zero);
        self.emit_division_failure_branch(is_zero, span)
    }

    fn emit_division_failure_branch(
        &mut self,
        is_zero: Value,
        span: Option<Span>,
    ) -> std::result::Result<(), String> {
        let fail_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(is_zero, fail_block, &[], continue_block, &[]);
        self.builder.switch_to_block(fail_block);
        self.emit_pending_cleanups(true)?;
        let (line, column) = self.span_values(span);
        self.builder
            .ins()
            .call(self.fail_division_by_zero, &[line, column]);
        self.builder.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
        self.builder.seal_block(fail_block);
        self.builder.switch_to_block(continue_block);
        self.builder.seal_block(continue_block);
        Ok(())
    }

    fn compile_call(
        &mut self,
        callee: &CallTarget,
        args: &[MirArg],
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        match callee {
            CallTarget::Name(name) if name == "print" => self.compile_print(args),
            CallTarget::Name(name) => self.compile_named_call(name, args, target),
            CallTarget::Value(function) => self.compile_function_value_call(function, args, target),
            CallTarget::Extern(call) => self.compile_extern_call(call, args, target),
            CallTarget::Member {
                object,
                field,
                receiver_place,
            } => self.compile_member_call(object, field, receiver_place.as_deref(), args),
            CallTarget::TraitMember {
                object,
                trait_name,
                field,
                receiver_place,
            } => self.compile_trait_member_call(
                object,
                trait_name,
                field,
                receiver_place.as_deref(),
                args,
            ),
        }
    }

    fn compile_extern_call(
        &mut self,
        call: &MirExternCall,
        args: &[MirArg],
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        if call.abi != "C" {
            return Err(format!(
                "direct backend received unsupported FFI ABI `{}`",
                call.abi
            ));
        }
        if args.len() != call.params.len() {
            return Err(format!(
                "direct backend expected {} argument(s) for extern `{}`, found {}",
                call.params.len(),
                call.symbol,
                args.len()
            ));
        }

        let mut spec_params = Vec::with_capacity(call.params.len());
        let mut expected_types = Vec::with_capacity(call.params.len());
        for param in &call.params {
            spec_params.push(DirectFfiParam {
                passing: match param.passing {
                    MirReceiverKind::Value => ReceiverKind::Value,
                    MirReceiverKind::Borrow => ReceiverKind::Borrow,
                    MirReceiverKind::BorrowMut => ReceiverKind::BorrowMut,
                },
                ty: direct_ffi_type_for_source(&param.ty, Some(param.passing))?,
            });
            expected_types.push(ensure_direct_type(
                &param.ty,
                &self.classes,
                &format!("extern parameter `{}`", param.name),
            )?);
        }
        let result_spec = direct_ffi_type_for_source(&call.return_type, None)?;
        let encoded_spec = encode_direct_ffi_call_spec(&DirectFfiCallSpec {
            symbol: call.symbol.clone(),
            params: spec_params,
            result: result_spec,
        });
        let (spec_ptr, spec_len) = self.string_constant(&encoded_spec)?;
        let buffer_size = match u32::try_from(args.len().max(1).saturating_mul(8)) {
            Ok(buffer_size) => buffer_size,
            Err(_) => return Err("direct backend FFI argument buffer is too large".to_string()),
        };
        let buffer_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            buffer_size,
            3,
        ));
        let buffer = self.builder.ins().stack_addr(types::I64, buffer_slot, 0);
        let mut writebacks = Vec::new();
        for (index, ((argument, param), expected)) in args
            .iter()
            .zip(&call.params)
            .zip(&expected_types)
            .enumerate()
        {
            let loaded = self.load_operand_for_target(&argument.value, expected)?;
            let coerced = self.coerce_value(loaded, expected)?;
            let boxed = self.ensure_opaque(coerced)?;
            self.builder
                .ins()
                .store(MemFlags::new(), boxed.values[0], buffer, (index as i32) * 8);
            match (param.passing, argument.writeback_place.as_ref()) {
                (MirReceiverKind::BorrowMut, Some(place)) => {
                    writebacks.push((place.clone(), boxed, expected.clone()));
                }
                (MirReceiverKind::BorrowMut, None) => {
                    return Err(format!(
                        "direct backend extern mutable argument {} has no writeback place",
                        index + 1
                    ));
                }
                (_, Some(_)) => {
                    return Err(format!(
                        "direct backend extern argument {} unexpectedly requests writeback",
                        index + 1
                    ));
                }
                (_, None) => {}
            }
        }
        let count = self
            .builder
            .ins()
            .iconst(types::I64, call.params.len() as i64);
        let invocation = self
            .builder
            .ins()
            .call(self.ffi_call, &[spec_ptr, spec_len, buffer, count]);
        let raw_result = self.builder.inst_results(invocation)[0];

        for (place, boxed, expected) in writebacks {
            let writeback = self.coerce_value(boxed, &expected)?;
            self.store_place(&place, writeback)?;
        }

        let boxed_result = self.owned_opaque_result(vec![raw_result], call.return_type.clone());
        let return_direct = ensure_direct_type(
            &call.return_type,
            &self.classes,
            &format!("extern return from `{}`", call.symbol),
        )?;
        let result = self.coerce_value(boxed_result, &return_direct)?;
        self.coerce_value(result, target)
    }

    fn compile_function_value_call(
        &mut self,
        function: &Operand,
        args: &[MirArg],
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        let function_type = infer_operand_type(function, &self.variable_types, &self.classes)
            .ok_or("direct backend could not infer the indirect callee type".to_string())?;
        let (params, return_type) = match function_type {
            DirectType::Opaque(Type::Function {
                params,
                return_type,
            }) => (params, return_type),
            DirectType::Opaque(Type::Closure {
                params,
                return_type,
                ..
            }) => (*params, return_type),
            _ => {
                return Err("direct backend expected an indirect function value".to_string());
            }
        };
        if args.len() > params.len() {
            return Err(format!(
                "direct backend expected at most {} indirect-call arguments, found {}",
                params.len(),
                args.len()
            ));
        }
        let binding = bind_function_value_args(
            &params,
            args,
            "direct backend function value has no parameter named",
            "direct backend received duplicate indirect-call arguments",
        )?;
        let param_types =
            function_value_param_types(&params, &self.classes, "indirect-call parameter")?;
        let return_direct =
            ensure_direct_type(&return_type, &self.classes, "indirect-call return type")?;

        let closure_writebacks = match function {
            Operand::Place(place) | Operand::MovePlace(place) => self
                .closure_capture_writebacks
                .get(place.split('.').next().unwrap_or(place))
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let loaded_function = self.load_operand(function)?;
        let function = self.ensure_opaque(loaded_function)?;
        let count = self.builder.ins().iconst(types::I64, params.len() as i64);
        let buffer_size = u32::try_from(params.len().max(1).saturating_mul(8))
            .ok()
            .ok_or("direct backend indirect-call buffer is too large".to_string())?;
        let buffer_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            buffer_size,
            3,
        ));
        let buffer = self.builder.ins().stack_addr(types::I64, buffer_slot, 0);
        let zero = self.builder.ins().iconst(types::I64, 0);
        for index in 0..params.len() {
            self.builder
                .ins()
                .store(MemFlags::new(), zero, buffer, (index as i32) * 8);
        }
        let mut writebacks = Vec::new();
        // Evaluate supplied expressions in source order while writing each
        // captured value to its declaration slot.
        for (argument, index) in args.iter().zip(&binding.source_slots) {
            let index = *index;
            let expected = &param_types[index];
            let passing = params[index].passing;
            let loaded_value = self.load_operand_for_target(&argument.value, expected)?;
            let value = self.coerce_value(loaded_value, expected)?;
            let value = self.ensure_opaque(value)?;
            let transferred = self.transfer_opaque_arg(&value);
            self.builder
                .ins()
                .store(MemFlags::new(), transferred, buffer, (index as i32) * 8);
            match (passing, argument.writeback_place.as_ref()) {
                (ReceiverKind::BorrowMut, Some(place)) => {
                    writebacks.push((index, place.clone(), expected.clone()));
                }
                (ReceiverKind::BorrowMut, None) => {
                    return Err(format!(
                        "direct backend indirect mutable argument {} has no writeback place",
                        index + 1
                    ));
                }
                (_, Some(_)) => {
                    return Err(format!(
                        "direct backend indirect argument {} unexpectedly requests writeback",
                        index + 1
                    ));
                }
                (_, None) => {}
            }
        }
        let keep_defaults_owned = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().call(
            self.function_bind_defaults,
            &[function.values[0], buffer, count, keep_defaults_owned],
        );
        for (index, supplied) in binding.slots.iter().enumerate() {
            if supplied.is_some() {
                continue;
            }
            let raw =
                self.builder
                    .ins()
                    .load(types::I64, MemFlags::new(), buffer, (index as i32) * 8);
            self.tag_raw_opaque_runtime_type(raw, &param_types[index])?;
        }
        let mut public_sinks = Vec::new();
        let mut capture_sinks = Vec::new();
        if !writebacks.is_empty() || !closure_writebacks.is_empty() {
            public_sinks = (0..params.len())
                .map(|_| self.builder.ins().iconst(types::I64, 0))
                .collect();
            for (index, place, _) in &writebacks {
                public_sinks[*index] = self.mutable_sink_for_place(place)?;
            }
            for writeback in &closure_writebacks {
                capture_sinks.push((
                    writeback.index,
                    self.mutable_sink_for_resolved_place(writeback.place.clone())?,
                ));
            }
            self.install_indirect_mutable_sinks(&public_sinks, &capture_sinks)?;
        }
        let call = self
            .builder
            .ins()
            .call(self.function_call, &[function.values[0], buffer, count]);
        let raw_result = self.builder.inst_results(call)[0];
        self.release_mutable_sinks(public_sinks.iter().copied());
        self.release_mutable_sinks(capture_sinks.iter().map(|(_, sink)| *sink));

        for (index, place, writeback_ty) in writebacks {
            let raw =
                self.builder
                    .ins()
                    .load(types::I64, MemFlags::new(), buffer, (index as i32) * 8);
            let zero = self.builder.ins().iconst(types::I64, 0);
            self.builder
                .ins()
                .store(MemFlags::new(), zero, buffer, (index as i32) * 8);
            let boxed = ValueRef {
                values: vec![raw],
                ty: DirectType::Opaque(direct_type_to_type(&writeback_ty)),
            };
            self.mark_temporary_opaque_owned(&boxed);
            let writeback = self.coerce_value(boxed, &writeback_ty)?;
            self.store_place(&place, writeback)?;
        }
        for writeback in closure_writebacks {
            let index = self
                .builder
                .ins()
                .iconst(types::I64, writeback.index as i64);
            let call = self
                .builder
                .ins()
                .call(self.closure_capture, &[function.values[0], index]);
            let raw = self.builder.inst_results(call)[0];
            let boxed = self.owned_opaque_result(vec![raw], direct_type_to_type(&writeback.ty));
            let value = self.coerce_value(boxed, &writeback.ty)?;
            self.store_resolved_view_place(writeback.place, value)?;
        }
        let boxed_result =
            self.owned_opaque_result(vec![raw_result], direct_type_to_type(&return_direct));
        let result = self.coerce_value(boxed_result, &return_direct)?;
        self.coerce_value(result, target)
    }

    fn compile_print(&mut self, args: &[MirArg]) -> std::result::Result<ValueRef, String> {
        let Some(argument) = args.first() else {
            return Err("direct backend expected `print` to receive one argument".to_string());
        };
        let argument = self.load_operand(&argument.value)?;
        match argument.ty.scalar_kind() {
            Some(ScalarKind::Int32) | Some(ScalarKind::Int64) => {
                self.builder
                    .ins()
                    .call(self.print_i64, &[argument.values[0]]);
            }
            Some(ScalarKind::Uint64) => {
                self.builder
                    .ins()
                    .call(self.print_u64, &[argument.values[0]]);
            }
            Some(ScalarKind::Float32) => {
                self.builder
                    .ins()
                    .call(self.print_f32, &[argument.values[0]]);
            }
            Some(ScalarKind::Float64) => {
                self.builder
                    .ins()
                    .call(self.print_f64, &[argument.values[0]]);
            }
            Some(ScalarKind::Bool) => {
                self.builder
                    .ins()
                    .call(self.print_bool, &[argument.values[0]]);
            }
            Some(ScalarKind::Unit) => {
                let argument = self.ensure_opaque(argument)?;
                self.builder
                    .ins()
                    .call(self.print_value, &[argument.values[0]]);
            }
            None => {
                let argument = self.ensure_opaque(argument)?;
                self.builder
                    .ins()
                    .call(self.print_value, &[argument.values[0]]);
            }
        }
        Ok(unit_value(&mut self.builder))
    }

    fn compile_format_string(
        &mut self,
        parts: &[MirFormatPart],
    ) -> std::result::Result<ValueRef, String> {
        let mut current = self.string_value("")?;
        for part in parts {
            let next = match part {
                MirFormatPart::Literal(text) => self.string_value(text)?,
                MirFormatPart::Value(value) => {
                    let value = self.load_operand(value)?;
                    let value = self.ensure_opaque(value)?;
                    let call = self
                        .builder
                        .ins()
                        .call(self.stringify_value, &[value.values[0]]);
                    self.owned_opaque_result(
                        self.builder.inst_results(call).to_vec(),
                        Type::named("str"),
                    )
                }
                MirFormatPart::Formatted {
                    value,
                    spec,
                    value_type,
                } => {
                    let value = self.load_operand(value)?;
                    let value = self.ensure_opaque(value)?;
                    let (spec_ptr, spec_len) = self.string_constant(spec.as_bytes())?;
                    let type_name = value_type.to_string();
                    let (type_ptr, type_len) = self.string_constant(type_name.as_bytes())?;
                    let call = self.builder.ins().call(
                        self.format_value,
                        &[value.values[0], spec_ptr, spec_len, type_ptr, type_len],
                    );
                    self.owned_opaque_result(
                        self.builder.inst_results(call).to_vec(),
                        Type::named("str"),
                    )
                }
            };
            current = self.compile_binary(BinaryOp::Add, current, next, None)?;
        }
        Ok(current)
    }

    fn string_value(&mut self, text: &str) -> std::result::Result<ValueRef, String> {
        let (ptr, len) = self.string_constant(text.as_bytes())?;
        let call = self.builder.ins().call(self.string_literal, &[ptr, len]);
        Ok(self.owned_opaque_result(self.builder.inst_results(call).to_vec(), Type::named("str")))
    }

    fn compile_named_call(
        &mut self,
        name: &str,
        args: &[MirArg],
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        if name == "random::Rng" {
            let ordered = ordered_named_args(&["seed"], args)?;
            let argument = ordered[0];
            let seed =
                self.load_operand_with_integer_hint(&argument.value, Some(ScalarKind::Int64))?;
            let seed = self.coerce_value(seed, &DirectType::Scalar(ScalarKind::Int64))?;
            let inst = self.builder.ins().call(self.rng_new, &[seed.values[0]]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("random.Rng"),
            ));
        }
        if name == "random::secure_int" {
            let ordered = ordered_named_args(&["lo", "hi"], args)?;
            let lo =
                self.load_operand_with_integer_hint(&ordered[0].value, Some(ScalarKind::Int64))?;
            let lo = self.coerce_value(lo, &DirectType::Scalar(ScalarKind::Int64))?;
            let hi =
                self.load_operand_with_integer_hint(&ordered[1].value, Some(ScalarKind::Int64))?;
            let hi = self.coerce_value(hi, &DirectType::Scalar(ScalarKind::Int64))?;
            let inst = self
                .builder
                .ins()
                .call(self.random_secure_int, &[lo.values[0], hi.values[0]]);
            return Ok(ValueRef {
                values: self.builder.inst_results(inst).to_vec(),
                ty: DirectType::Scalar(ScalarKind::Int64),
            });
        }
        if name == "random::secure_bytes" {
            let ordered = ordered_named_args(&["n"], args)?;
            let argument = ordered[0];
            let count =
                self.load_operand_with_integer_hint(&argument.value, Some(ScalarKind::Int64))?;
            let count = self.coerce_value(count, &DirectType::Scalar(ScalarKind::Int64))?;
            let inst = self
                .builder
                .ins()
                .call(self.random_secure_bytes, &[count.values[0]]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
            ));
        }
        if let Some(associated) = name
            .strip_prefix("Array.")
            .and_then(|field| BuiltinAssociatedFunction::resolve("Array", field))
        {
            let (array_ty, element_type) = match target {
                DirectType::Opaque(ty @ Type::Named(owner, arguments))
                    if owner == "Array" && arguments.len() == 1 =>
                {
                    (ty.clone(), arguments[0].clone())
                }
                other => {
                    return Err(format!(
                        "direct backend requires an Array result type for `{name}`, found {}",
                        render_direct_type(other)
                    ));
                }
            };
            let dtype = self
                .builder
                .ins()
                .iconst(types::I64, direct_array_dtype_code(&element_type)?);
            let zero = self.builder.ins().iconst(types::I64, 0);
            let shape_ty =
                DirectType::Opaque(Type::Named("list".to_string(), vec![Type::named("int64")]));
            let inst = if associated == BuiltinAssociatedFunction::ArrayZeros {
                let ordered = ordered_named_args(&["shape"], args)?;
                let shape = self.load_operand_for_target(&ordered[0].value, &shape_ty)?;
                let shape = self.ensure_opaque(shape)?;
                self.builder
                    .ins()
                    .call(self.array_zeros, &[dtype, shape.values[0], zero, zero])
            } else if associated == BuiltinAssociatedFunction::ArrayFull {
                let ordered = ordered_named_args(&["shape", "value"], args)?;
                let shape = self.load_operand_for_target(&ordered[0].value, &shape_ty)?;
                let shape = self.ensure_opaque(shape)?;
                let element_direct =
                    ensure_direct_type(&element_type, &self.classes, "Array dtype")?;
                let value =
                    self.load_operand_as_opaque_direct(&ordered[1].value, &element_direct)?;
                self.builder.ins().call(
                    self.array_full,
                    &[dtype, shape.values[0], value.values[0], zero, zero],
                )
            } else {
                debug_assert_eq!(associated, BuiltinAssociatedFunction::ArrayFromVec);
                let ordered = ordered_named_args(&["values", "shape"], args)?;
                let values_ty =
                    DirectType::Opaque(Type::Named("list".to_string(), vec![element_type]));
                let values = self.load_operand_for_target(&ordered[0].value, &values_ty)?;
                let values = self.ensure_opaque(values)?;
                let shape = self.load_operand_for_target(&ordered[1].value, &shape_ty)?;
                let shape = self.ensure_opaque(shape)?;
                self.builder.ins().call(
                    self.array_from_vec,
                    &[dtype, values.values[0], shape.values[0], zero, zero],
                )
            };
            return Ok(self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), array_ty));
        }
        if let Some(associated) = name
            .strip_prefix("Duration.")
            .and_then(|field| BuiltinAssociatedFunction::resolve("Duration", field))
        {
            let [argument] = args else {
                return Err(format!(
                    "direct backend expected `Duration.{}` to receive one argument",
                    associated.name()
                ));
            };
            let value =
                self.load_operand_with_integer_hint(&argument.value, Some(ScalarKind::Int64))?;
            let value = self.coerce_value(value, &DirectType::Scalar(ScalarKind::Int64))?;
            let unit_nanoseconds = match associated {
                BuiltinAssociatedFunction::DurationMilliseconds => {
                    crate::runtime_value::NANOS_PER_MILLISECOND
                }
                BuiltinAssociatedFunction::DurationSeconds => {
                    crate::runtime_value::NANOS_PER_SECOND
                }
                _ => {
                    debug_assert_eq!(associated, BuiltinAssociatedFunction::DurationMinutes);
                    crate::runtime_value::NANOS_PER_MINUTE
                }
            };
            let unit_nanoseconds = self
                .builder
                .ins()
                .iconst(types::I64, unit_nanoseconds as i64);
            let inst = self
                .builder
                .ins()
                .call(self.duration_from_i64, &[value.values[0], unit_nanoseconds]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Duration"),
            ));
        }
        if name == "range" {
            return self.compile_range(args);
        }
        if name == "Queue" {
            if args.len() > 1 {
                return Err(format!(
                    "direct backend expected `{}()` to take at most one capacity argument",
                    name
                ));
            }
            let capacity = match args {
                [] => self.builder.ins().iconst(types::I64, 0),
                [argument] => {
                    if argument.name.as_deref() != Some("capacity") && argument.name.is_some() {
                        return Err(
                            "direct backend expected `Queue()` to receive only `capacity=`"
                                .to_string(),
                        );
                    }
                    let value = self.load_operand(&argument.value)?;
                    let value = self.ensure_opaque(value)?;
                    value.values[0]
                }
                _ => unreachable!(),
            };
            let inst = self.builder.ins().call(self.channel_new, &[capacity]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::Named("Queue".to_string(), vec![Type::named("Unknown")]),
            ));
        }
        if name == "TaskGroup" {
            if !args.is_empty() {
                return Err(format!(
                    "direct backend expected `{}()` to take no arguments",
                    name
                ));
            }
            let inst = self.builder.ins().call(self.task_group_new, &[]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("TaskGroup"),
            ));
        }
        if name == "cancelled" {
            if !args.is_empty() {
                return Err(
                    "direct backend expected `cancelled()` to take no arguments".to_string()
                );
            }
            let inst = self.builder.ins().call(self.cancelled, &[]);
            return Ok(ValueRef {
                values: self.builder.inst_results(inst).to_vec(),
                ty: DirectType::Scalar(ScalarKind::Bool),
            });
        }
        if name == "yield_now" {
            if !args.is_empty() {
                return Err(
                    "direct backend expected `yield_now()` to take no arguments".to_string()
                );
            }
            self.builder.ins().call(self.yield_now, &[]);
            return Ok(unit_value(&mut self.builder));
        }
        if name == "sleep" {
            let [argument] = args else {
                return Err(
                    "direct backend expected `sleep()` to receive one duration argument"
                        .to_string(),
                );
            };
            let duration = self.load_operand(&argument.value)?;
            let duration = self.ensure_opaque(duration)?;
            self.builder
                .ins()
                .call(self.sleep_value_void, &[duration.values[0]]);
            return Ok(unit_value(&mut self.builder));
        }
        if name == "select" {
            if args.is_empty() || args.iter().any(|argument| argument.name.is_some()) {
                return Err(
                    "direct backend expected `select(source, ...)` with one or more positional \
                     Queue, Task, or Duration sources"
                        .to_string(),
                );
            }

            let mut operands = Vec::with_capacity(args.len());
            let mut element_types = Vec::with_capacity(args.len());
            let mut queue_payload: Option<Type> = None;
            let mut task_payload: Option<Type> = None;
            for argument in args {
                let direct =
                    infer_operand_type(&argument.value, &self.variable_types, &self.classes)
                        .ok_or(
                            "direct backend could not infer a `select` source type".to_string(),
                        )?;
                let ty = direct_type_to_type(&direct);
                match &ty {
                    Type::Named(name, type_args) if name == "Queue" => {
                        let [payload] = type_args.as_slice() else {
                            return Err("direct backend expected `Queue[Q]` as a `select` source"
                                .to_string());
                        };
                        if let Some(previous) = &queue_payload {
                            if previous != payload {
                                return Err(
                                    "direct backend received inconsistent Queue payload types in \
                                     `select`"
                                        .to_string(),
                                );
                            }
                        } else {
                            queue_payload = Some(payload.clone());
                        }
                    }
                    Type::Named(name, type_args) if name == "Task" => {
                        let [payload] = type_args.as_slice() else {
                            return Err("direct backend expected `Task[T]` as a `select` source"
                                .to_string());
                        };
                        if let Some(previous) = &task_payload {
                            if previous != payload {
                                return Err(
                                    "direct backend received inconsistent Task result types in \
                                     `select`"
                                        .to_string(),
                                );
                            }
                        } else {
                            task_payload = Some(payload.clone());
                        }
                    }
                    Type::Named(name, type_args) if name == "Duration" && type_args.is_empty() => {}
                    _ => {
                        return Err(format!(
                            "direct backend expected a Queue, Task, or Duration `select` source, \
                             found `{}`",
                            render_direct_type(&direct)
                        ));
                    }
                }
                operands.push(argument.value.clone());
                element_types.push(ty);
            }

            let tuple_type = Type::Tuple(element_types.clone());
            let tuple = self.compile_tuple_literal(&operands, &element_types, tuple_type)?;
            let tuple = self.transfer_opaque_arg(&tuple);
            let inst = self.builder.ins().call(self.select, &[tuple]);
            let inferred_result_type = Type::Named(
                "SelectOutcome".to_string(),
                vec![
                    queue_payload.unwrap_or(Type::Unit),
                    task_payload.unwrap_or(Type::Unit),
                ],
            );
            let result_type = match target {
                DirectType::Opaque(Type::Named(name, type_args))
                    if name == "SelectOutcome" && type_args.len() == 2 =>
                {
                    Type::Named(name.clone(), type_args.clone())
                }
                _ => inferred_result_type,
            };
            return Ok(
                self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), result_type)
            );
        }
        if matches!(name, "wait_any" | "wait_all") {
            let mut tasks_arg: Option<&MirArg> = None;
            let mut timeout_arg: Option<&MirArg> = None;
            for (index, argument) in args.iter().enumerate() {
                match argument.name.as_deref() {
                    Some("tasks") => {
                        if tasks_arg.replace(argument).is_some() {
                            return Err(format!(
                                "direct backend expected `{name}(tasks, timeout=...)`"
                            ));
                        }
                    }
                    Some("timeout") => {
                        if timeout_arg.replace(argument).is_some() {
                            return Err(format!(
                                "direct backend expected `{name}(tasks, timeout=...)`"
                            ));
                        }
                    }
                    None if index == 0 && tasks_arg.is_none() => tasks_arg = Some(argument),
                    None if index == 1 && timeout_arg.is_none() => timeout_arg = Some(argument),
                    _ => {
                        return Err(format!(
                            "direct backend expected `{name}(tasks, timeout=...)`"
                        ))
                    }
                }
            }
            let tasks_arg = required_named_arg(
                tasks_arg,
                &format!("direct backend expected `{name}(tasks, timeout=...)`"),
            )?;
            let tasks = self.load_operand(&tasks_arg.value)?;
            let tasks = self.ensure_opaque(tasks)?;
            let task_payload_ty =
                infer_operand_type(&tasks_arg.value, &self.variable_types, &self.classes)
                    .map(|direct| match direct {
                        DirectType::Opaque(Type::Named(vec_name, args)) if vec_name == "list" => {
                            match args.as_slice() {
                                [Type::Named(task_name, task_args)] if task_name == "Task" => {
                                    task_args.first().cloned().unwrap_or(Type::Unit)
                                }
                                _ => Type::named("Unknown"),
                            }
                        }
                        _ => Type::named("Unknown"),
                    })
                    .unwrap_or(Type::named("Unknown"));
            let inst = if let Some(timeout_arg) = timeout_arg {
                let timeout = self.load_operand(&timeout_arg.value)?;
                let timeout = self.ensure_opaque(timeout)?;
                let callee = if name == "wait_any" {
                    self.wait_any_timeout_value
                } else {
                    self.wait_all_timeout_value
                };
                self.builder
                    .ins()
                    .call(callee, &[tasks.values[0], timeout.values[0]])
            } else {
                let callee = if name == "wait_any" {
                    self.wait_any
                } else {
                    self.wait_all
                };
                self.builder.ins().call(callee, &[tasks.values[0]])
            };
            let enum_name = if name == "wait_any" {
                "WaitAny"
            } else {
                "WaitAll"
            };
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::Named(enum_name.to_string(), vec![task_payload_ty]),
            ));
        }
        if matches!(
            name,
            "io::write"
                | "io::flush"
                | "io::read_line"
                | "fs::exists"
                | "fs::read_to_string"
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
                | "process::inherit"
                | "process::null"
                | "process::pipe"
                | "process::supervisor"
                | "process::start"
                | "process::run"
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
        ) {
            return self.compile_builtin_io_named_call(name, args);
        }
        if host_builtin_metadata(name).is_some() {
            return self.compile_host_builtin_named_call(name, args);
        }
        if name == "abs" {
            let [argument] = args else {
                return Err("direct backend expected `abs()` to receive one argument".to_string());
            };
            let loaded = self.load_operand(&argument.value)?;
            let return_ty = loaded.ty.clone();
            let value = self.ensure_opaque(loaded)?;
            let inst = self.builder.ins().call(self.abs_value, &[value.values[0]]);
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return self.coerce_value(result, &return_ty);
        }
        if matches!(name, "parse_int32" | "parse_int64" | "parse_float64") {
            let [argument] = args else {
                return Err(format!(
                    "direct backend expected `{}`() to receive one string argument",
                    name
                ));
            };
            let loaded = self.load_operand(&argument.value)?;
            let value = self.ensure_opaque(loaded)?;
            let func = match name {
                "parse_int32" => self.parse_int32,
                "parse_int64" => self.parse_int64,
                "parse_float64" => self.parse_float64,
                _ => unreachable!(),
            };
            let inst = self.builder.ins().call(func, &[value.values[0]]);
            let return_ty = match name {
                "parse_int32" => Type::Named(
                    "Result".to_string(),
                    vec![Type::named("int32"), Type::named("str")],
                ),
                "parse_int64" => Type::Named(
                    "Result".to_string(),
                    vec![Type::named("int64"), Type::named("str")],
                ),
                "parse_float64" => Type::Named(
                    "Result".to_string(),
                    vec![Type::named("float64"), Type::named("str")],
                ),
                _ => unreachable!(),
            };
            return Ok(
                self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), return_ty)
            );
        }
        if name == "min" || name == "max" {
            let [left_arg, right_arg] = args else {
                return Err(format!(
                    "direct backend expected `{}`() to receive two arguments",
                    name
                ));
            };
            let left = self.load_operand(&left_arg.value)?;
            let return_ty = left.ty.clone();
            let left = self.ensure_opaque(left)?;
            let right = self.load_operand(&right_arg.value)?;
            let right = self.ensure_opaque(right)?;
            let func = if name == "min" {
                self.min_value
            } else {
                self.max_value
            };
            let inst = self
                .builder
                .ins()
                .call(func, &[left.values[0], right.values[0]]);
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return self.coerce_value(result, &return_ty);
        }
        if name == "sqrt" {
            let [argument] = args else {
                return Err("direct backend expected `sqrt()` to receive one argument".to_string());
            };
            let loaded = self.load_operand(&argument.value)?;
            let return_ty = loaded.ty.clone();
            let value = self.ensure_opaque(loaded)?;
            let inst = self.builder.ins().call(self.sqrt_value, &[value.values[0]]);
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return self.coerce_value(result, &return_ty);
        }
        if name == "round" {
            let ordered = ordered_named_args(&["value"], args)?;
            let loaded = self.load_operand(&ordered[0].value)?;
            let input_ty = loaded.ty.clone();
            let return_ty = match input_ty.scalar_kind() {
                Some(ScalarKind::Float32 | ScalarKind::Float64) => {
                    DirectType::Scalar(ScalarKind::Int64)
                }
                _ => input_ty.clone(),
            };
            let value = self.ensure_opaque(loaded)?;
            let inst = self
                .builder
                .ins()
                .call(self.round_value, &[value.values[0]]);
            let result = self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("Unknown"),
            );
            return self.coerce_value(result, &return_ty);
        }
        if name == "divmod" {
            let ordered = ordered_named_args(&["left", "right"], args)?;
            let left = self.load_operand(&ordered[0].value)?;
            let operand_ty = direct_type_to_type(&left.ty);
            let left = self.ensure_opaque(left)?;
            self.tag_opaque_runtime_type(&left, &operand_ty)?;
            let right = self.load_operand(&ordered[1].value)?;
            let right = self.ensure_opaque(right)?;
            let inst = self
                .builder
                .ins()
                .call(self.divmod_value, &[left.values[0], right.values[0]]);
            let result_ty = Type::Tuple(vec![operand_ty.clone(), operand_ty]);
            return Ok(
                self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), result_ty)
            );
        }
        if let Some(type_name) = name.strip_suffix(".with_capacity") {
            if matches!(type_name, "list" | "set" | "dict") {
                let ordered = ordered_named_args(&["minimum"], args)?;
                let minimum = self
                    .load_operand_with_integer_hint(&ordered[0].value, Some(ScalarKind::Int64))?;
                let minimum = self.coerce_value(minimum, &DirectType::Scalar(ScalarKind::Int64))?;
                let empty = match type_name {
                    "list" => self.vec_empty,
                    "set" => self.set_empty,
                    "dict" => self.map_empty,
                    _ => unreachable!(),
                };
                let empty_call = self.builder.ins().call(empty, &[]);
                let collection = self.builder.inst_results(empty_call)[0];
                let zero = self.builder.ins().iconst(types::I64, 0);
                let opcode = self.builder.ins().iconst(types::I64, 4);
                let reserve = self.builder.ins().call(
                    self.collection_operation,
                    &[collection, zero, minimum.values[0], opcode],
                );
                self.release_opaque_handle(self.builder.inst_results(reserve)[0]);
                let result_ty = direct_type_to_type(target);
                return Ok(self.owned_opaque_result(vec![collection], result_ty));
            }
        }
        if matches!(name, "list" | "set" | "dict") {
            if !args.is_empty() {
                return Err(format!(
                    "direct backend expected `{}`() to take no arguments",
                    name
                ));
            }
            let func = match name {
                "list" => self.vec_empty,
                "set" => self.set_empty,
                "dict" => self.map_empty,
                _ => unreachable!(),
            };
            let inst = self.builder.ins().call(func, &[]);
            let ty = match name {
                "list" | "set" => Type::Named(name.to_string(), vec![Type::named("Unknown")]),
                "dict" => Type::Named(
                    "dict".to_string(),
                    vec![Type::named("Unknown"), Type::named("Unknown")],
                ),
                _ => unreachable!(),
            };
            return Ok(self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), ty));
        }
        let func_ref = *self
            .function_refs
            .get(name)
            .ok_or(format!("direct backend does not know function `{}`", name))?;
        let mut lowered_args = Vec::new();
        let expected = self
            .function_param_types
            .get(name)
            .cloned()
            .unwrap_or_default();
        let mut substitutions = HashMap::new();
        if let Some(return_type) = self.function_return_types.get(name) {
            collect_direct_runtime_type_substitutions(
                &direct_type_to_type(return_type),
                &direct_type_to_type(target),
                &mut substitutions,
            );
        }
        for (expected_ty, argument) in expected.iter().zip(args.iter()) {
            if let Some(actual_ty) =
                infer_operand_type(&argument.value, &self.variable_types, &self.classes)
            {
                collect_direct_runtime_type_substitutions(
                    &direct_type_to_type(expected_ty),
                    &direct_type_to_type(&actual_ty),
                    &mut substitutions,
                );
            }
        }
        let mut writeback_places = Vec::new();
        let mut mutable_sink_places = vec![None; expected.len()];
        for (index, argument) in args.iter().enumerate() {
            let semantic_expected = expected
                .get(index)
                .map(|expected_ty| {
                    let specialized =
                        substitute_type(&direct_type_to_type(expected_ty), &substitutions);
                    ensure_direct_type(
                        &specialized,
                        &self.classes,
                        &format!("specialized argument {} for `{name}`", index + 1),
                    )
                })
                .transpose()?;
            let loaded = if let Some(expected_ty) = semantic_expected.as_ref() {
                self.load_operand_for_target(&argument.value, expected_ty)?
            } else {
                self.load_operand(&argument.value)?
            };
            let semantic_coerced = if let Some(expected_ty) = semantic_expected.as_ref() {
                self.coerce_value(loaded, expected_ty)?
            } else {
                loaded
            };
            let coerced = if let Some(abi_expected) = expected.get(index) {
                self.coerce_value(semantic_coerced, abi_expected)?
            } else {
                semantic_coerced
            };
            if let Some(place) = &argument.writeback_place {
                writeback_places.push(place.clone());
                if let Some(slot) = mutable_sink_places.get_mut(index) {
                    *slot = Some(place.clone());
                }
            }
            if matches!(coerced.ty, DirectType::Opaque(_)) {
                lowered_args.push(self.transfer_opaque_arg(&coerced));
            } else {
                lowered_args.extend(coerced.values);
            }
        }
        let mutable_sinks = if mutable_sink_places.iter().any(Option::is_some) {
            let mut sinks = Vec::with_capacity(mutable_sink_places.len());
            for place in &mutable_sink_places {
                sinks.push(match place {
                    Some(place) => self.mutable_sink_for_place(place)?,
                    None => self.builder.ins().iconst(types::I64, 0),
                });
            }
            self.install_direct_mutable_sinks(&sinks)?;
            sinks
        } else {
            Vec::new()
        };
        let inst = self.builder.ins().call(func_ref, &lowered_args);
        let results = self.builder.inst_results(inst).to_vec();
        self.release_mutable_sinks(mutable_sinks);
        let (result, writebacks) = self.split_call_results(name, results)?;
        self.apply_writeback_places(&writeback_places, writebacks)?;
        Ok(result)
    }

    fn compile_host_builtin_named_call(
        &mut self,
        name: &str,
        args: &[MirArg],
    ) -> std::result::Result<ValueRef, String> {
        let metadata = host_builtin_metadata(name)
            .expect("host builtin codegen is only called for registered host builtins");
        if name == "sys::monotonic_time_ms" {
            if !args.is_empty() {
                return Err(format!(
                    "direct backend expected `{name}` to receive no arguments, found {}",
                    args.len()
                ));
            }
            debug_assert!(metadata.params.is_empty());
            let call = self.builder.ins().call(self.monotonic_time_ms, &[]);
            return Ok(ValueRef {
                values: self.builder.inst_results(call).to_vec(),
                ty: DirectType::Scalar(ScalarKind::Int64),
            });
        }
        // Checked MIR materializes builtin defaults before direct code generation, so
        // optional metadata parameters are present in this argument list too.
        let expected_names = metadata
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        let bound = ordered_optional_named_args(&expected_names, args)?;
        let mut loaded_args = Vec::with_capacity(bound.len());
        for (index, argument) in bound.into_iter().enumerate() {
            let argument = required_named_arg(
                argument,
                &format!(
                    "direct backend is missing argument {} for `{name}`",
                    index + 1
                ),
            )?;
            let loaded = self.load_operand(&argument.value)?;
            let loaded = self.ensure_opaque(loaded)?;
            loaded_args.push(loaded);
        }
        self.compile_host_builtin_loaded_call(name, &loaded_args)
    }

    fn compile_host_builtin_loaded_call(
        &mut self,
        name: &str,
        args: &[ValueRef],
    ) -> std::result::Result<ValueRef, String> {
        let metadata = host_builtin_metadata(name)
            .expect("host builtin codegen is only called for registered host builtins");
        if args.len() != metadata.params.len() {
            return Err(format!(
                "direct backend expected `{name}` to receive {} arguments, found {}",
                metadata.params.len(),
                args.len()
            ));
        }
        let count = self.builder.ins().iconst(types::I64, args.len() as i64);
        let buffer_call = self.builder.ins().call(self.arg_buffer_new, &[count]);
        let buffer = self.builder.inst_results(buffer_call)[0];
        for (index, loaded) in args.iter().enumerate() {
            let index = self.builder.ins().iconst(types::I64, index as i64);
            self.builder
                .ins()
                .call(self.arg_buffer_store, &[buffer, index, loaded.values[0]]);
        }
        let (name_ptr, name_len) = self.string_constant(metadata.qualified_name.as_bytes())?;
        let call = self
            .builder
            .ins()
            .call(self.host_builtin, &[name_ptr, name_len, buffer, count]);

        let semantic_type = metadata.return_type.clone();
        let result = self.owned_opaque_result(
            self.builder.inst_results(call).to_vec(),
            semantic_type.clone(),
        );
        let target = ensure_direct_type(
            &semantic_type,
            &self.classes,
            &format!("return value of `{name}`"),
        )?;
        self.coerce_value(result, &target)
    }

    fn compile_range(&mut self, args: &[MirArg]) -> std::result::Result<ValueRef, String> {
        let int_ty = DirectType::Scalar(ScalarKind::Int64);
        let (start_arg, stop_arg) = if args.iter().all(|arg| arg.name.is_none()) {
            match args {
                [stop] => (None, Some(stop)),
                [start, stop] => (Some(start), Some(stop)),
                _ => {
                    return Err(
                        "direct backend expected `range()` to receive one or two arguments"
                            .to_string(),
                    )
                }
            }
        } else {
            let mut start = None;
            let mut stop = None;
            let mut next_positional = 0usize;
            for arg in args {
                match arg.name.as_deref() {
                    Some("start") => start = Some(arg),
                    Some("stop") => stop = Some(arg),
                    Some(other) => {
                        return Err(format!(
                            "direct backend does not recognize `range()` argument `{}`",
                            other
                        ))
                    }
                    None => {
                        if next_positional == 0 {
                            start = Some(arg);
                        } else if next_positional == 1 {
                            stop = Some(arg);
                        } else {
                            return Err(
                                "direct backend expected `range()` to receive one or two arguments"
                                    .to_string(),
                            );
                        }
                        next_positional += 1;
                    }
                }
            }
            (start, stop)
        };

        let start = if let Some(argument) = start_arg {
            let loaded = self.load_operand(&argument.value)?;
            self.coerce_value(loaded, &int_ty)?
        } else {
            ValueRef {
                values: vec![self.builder.ins().iconst(types::I64, 0)],
                ty: int_ty.clone(),
            }
        };
        let stop_arg = required_named_arg(
            stop_arg,
            "direct backend expected `range()` to receive a `stop` argument",
        )?;
        let stop = self.load_operand(&stop_arg.value)?;
        let stop = self.coerce_value(stop, &int_ty)?;
        let inst = self
            .builder
            .ins()
            .call(self.range_new, &[start.values[0], stop.values[0]]);
        Ok(self.owned_opaque_result(
            self.builder.inst_results(inst).to_vec(),
            Type::named("Range"),
        ))
    }

    fn lower_optional_opaque_arg(
        &mut self,
        argument: Option<&MirArg>,
    ) -> std::result::Result<Value, String> {
        if let Some(argument) = argument {
            let loaded = self.load_operand(&argument.value)?;
            let value = self.ensure_opaque(loaded)?;
            Ok(value.values[0])
        } else {
            Ok(self.builder.ins().iconst(types::I64, 0))
        }
    }

    fn compile_builtin_io_named_call(
        &mut self,
        name: &str,
        args: &[MirArg],
    ) -> std::result::Result<ValueRef, String> {
        let expected_names: &[&str] = match name {
            "io::write" => &["text"],
            "io::flush" | "io::read_line" => &[],
            "fs::exists" | "fs::read_to_string" | "fs::read_bytes" | "fs::create_dir"
            | "fs::read_dir" | "fs::remove_file" | "fs::open" | "fs::create" | "fs::append"
            | "net::unix_listen" | "net::unix_connect" => &["path"],
            "process::inherit" | "process::null" | "process::pipe" | "process::supervisor" => &[],
            "process::start" => &[
                "command", "cwd", "env", "stdin", "stdout", "stderr", "group",
            ],
            "process::run" => &[
                "command", "cwd", "env", "stdin", "stdout", "stderr", "timeout", "group",
            ],
            "net::connect"
            | "net::listen"
            | "net::udp_bind"
            | "net::http_listen"
            | "net::websocket_listen" => &["address"],
            "net::websocket_connect" => &["url"],
            "fs::write_string" | "fs::append_string" => &["path", "text"],
            "fs::write_bytes" | "fs::append_bytes" => &["path", "bytes"],
            "net::connect_timeout" => &["address", "timeout"],
            "net::unix_connect_timeout" => &["path", "timeout"],
            "net::websocket_connect_timeout" => &["url", "timeout"],
            "net::tls_listen" => &["address", "cert_pem_path", "key_pem_path"],
            "net::tls_connect" => &["address", "server_name", "ca_pem_path"],
            "net::tls_connect_timeout" => &["address", "server_name", "ca_pem_path", "timeout"],
            "net::http_request_text" => &["method", "url", "body", "headers"],
            "net::http_request_text_timeout" => &["method", "url", "body", "headers", "timeout"],
            "net::http_request_bytes" => &["method", "url", "bytes", "headers"],
            "net::http_request_bytes_timeout" => &["method", "url", "bytes", "headers", "timeout"],
            _ => {
                return Err(format!(
                    "direct backend does not know builtin I/O call `{}`",
                    name
                ))
            }
        };
        let func = match name {
            "io::write" => self.io_write,
            "io::flush" => self.io_flush,
            "io::read_line" => self.io_read_line,
            "fs::exists" => self.fs_exists,
            "fs::read_to_string" => self.fs_read_to_string,
            "fs::read_bytes" => self.fs_read_bytes,
            "fs::write_string" => self.fs_write_string,
            "fs::write_bytes" => self.fs_write_bytes,
            "fs::append_string" => self.fs_append_string,
            "fs::append_bytes" => self.fs_append_bytes,
            "fs::create_dir" => self.fs_create_dir,
            "fs::read_dir" => self.fs_read_dir,
            "fs::remove_file" => self.fs_remove_file,
            "fs::open" => self.fs_open,
            "fs::create" => self.fs_create,
            "fs::append" => self.fs_append,
            "process::inherit" => self.process_inherit,
            "process::null" => self.process_null,
            "process::pipe" => self.process_pipe,
            "process::supervisor" => self.process_supervisor,
            "process::start" => self.process_start,
            "process::run" => self.process_run,
            "net::connect" => self.net_connect,
            "net::connect_timeout" => self.net_connect_timeout,
            "net::listen" => self.net_listen,
            "net::udp_bind" => self.net_udp_bind,
            "net::unix_listen" => self.net_unix_listen,
            "net::unix_connect" => self.net_unix_connect,
            "net::unix_connect_timeout" => self.net_unix_connect_timeout,
            "net::tls_listen" => self.net_tls_listen,
            "net::tls_connect" => self.net_tls_connect,
            "net::tls_connect_timeout" => self.net_tls_connect_timeout,
            "net::http_listen" => self.net_http_listen,
            "net::http_request_text" => self.net_http_request_text,
            "net::http_request_text_timeout" => self.net_http_request_text_timeout,
            "net::http_request_bytes" => self.net_http_request_bytes,
            "net::http_request_bytes_timeout" => self.net_http_request_bytes_timeout,
            "net::websocket_listen" => self.net_websocket_listen,
            "net::websocket_connect" => self.net_websocket_connect,
            "net::websocket_connect_timeout" => self.net_websocket_connect_timeout,
            _ => unreachable!(),
        };
        let bound = ordered_optional_named_args(expected_names, args)?;
        let mut lowered_args = Vec::new();
        for (index, argument) in bound.iter().enumerate() {
            let optional_timeout = matches!(
                name,
                "net::connect_timeout"
                    | "net::unix_connect_timeout"
                    | "process::run"
                    | "net::tls_connect_timeout"
                    | "net::http_request_text_timeout"
                    | "net::http_request_bytes_timeout"
                    | "net::websocket_connect_timeout"
            ) && index == expected_names.len() - 1;
            if optional_timeout {
                lowered_args.push(self.lower_optional_opaque_arg(*argument)?);
                continue;
            }
            let argument =
                required_named_arg(*argument, "direct backend is missing a builtin argument")?;
            let loaded = self.load_operand(&argument.value)?;
            let value = self.ensure_opaque(loaded)?;
            lowered_args.push(value.values[0]);
        }
        let inst = self.builder.ins().call(func, &lowered_args);
        let results = self.builder.inst_results(inst).to_vec();
        let io_error_ty = Type::Named("io.Error".to_string(), Vec::new());
        let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
        match name {
            "fs::exists" => {
                let result = self.owned_opaque_result(results, Type::named("bool"));
                self.coerce_value(result, &DirectType::Scalar(ScalarKind::Bool))
            }
            "io::write" | "io::flush" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                ),
            )),
            "io::read_line" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("Option".to_string(), vec![Type::named("str")]),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                ),
            )),
            "fs::read_to_string" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::named("str"),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                ),
            )),
            "fs::read_bytes" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![bytes_ty.clone(), io_error_ty.clone()],
                ),
            )),
            "fs::write_string" | "fs::write_bytes" | "fs::append_string" | "fs::append_bytes"
            | "fs::create_dir" | "fs::remove_file" => Ok(self.owned_opaque_result(
                results,
                Type::Named("Result".to_string(), vec![Type::Unit, io_error_ty.clone()]),
            )),
            "fs::read_dir" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("list".to_string(), vec![Type::named("str")]),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "fs::open" | "fs::create" | "fs::append" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("fs.File".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "process::inherit" | "process::null" | "process::pipe" => Ok(self.owned_opaque_result(
                results,
                Type::Named("process.Stdio".to_string(), Vec::new()),
            )),
            "process::supervisor" => Ok(self.owned_opaque_result(
                results,
                Type::Named("process.Supervisor".to_string(), Vec::new()),
            )),
            "process::start" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("process.Child".to_string(), Vec::new()),
                        Type::Named("process.Error".to_string(), Vec::new()),
                    ],
                ),
            )),
            "process::run" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("process.Completed".to_string(), Vec::new()),
                        Type::Named("process.Error".to_string(), Vec::new()),
                    ],
                ),
            )),
            "net::connect" | "net::connect_timeout" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TcpStream".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::listen" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TcpListener".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::udp_bind" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UdpSocket".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::unix_listen" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UnixListener".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::unix_connect" | "net::unix_connect_timeout" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UnixStream".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::tls_listen" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TlsListener".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::tls_connect" | "net::tls_connect_timeout" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TlsStream".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::http_listen" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.HttpListener".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::http_request_text"
            | "net::http_request_text_timeout"
            | "net::http_request_bytes"
            | "net::http_request_bytes_timeout" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.HttpResponse".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::websocket_listen" => Ok(self.owned_opaque_result(
                results,
                Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.WebSocketListener".to_string(), Vec::new()),
                        io_error_ty.clone(),
                    ],
                ),
            )),
            "net::websocket_connect" | "net::websocket_connect_timeout" => Ok(self
                .owned_opaque_result(
                    results,
                    Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.WebSocket".to_string(), Vec::new()),
                            io_error_ty.clone(),
                        ],
                    ),
                )),
            _ => unreachable!(),
        }
    }

    fn compile_for_range(
        &mut self,
        binding: &str,
        iterable: &Operand,
        body_label: &str,
        exit_label: &str,
    ) -> std::result::Result<(), String> {
        let Operand::Place(iterable_place) = iterable else {
            return Err(
                "direct backend requires `for range` iterables to live in a place".to_string(),
            );
        };
        let range = self.load_place(iterable_place)?;
        let range = self.ensure_opaque(range)?;
        let current_inst = self
            .builder
            .ins()
            .call(self.range_current, &[range.values[0]]);
        let current = self.builder.inst_results(current_inst)[0];
        let end_inst = self.builder.ins().call(self.range_end, &[range.values[0]]);
        let end = self.builder.inst_results(end_inst)[0];
        let has_next = self.builder.ins().icmp(IntCC::SignedLessThan, current, end);

        let next_block = self.builder.create_block();
        let body_block = self.compiled_block(body_label)?;
        let exit_block = self.compiled_block(exit_label)?;
        self.builder
            .ins()
            .brif(has_next, next_block, &[], exit_block, &[]);

        self.builder.switch_to_block(next_block);
        let binding_ty = self.type_of_place(binding)?;
        self.store_place(
            binding,
            ValueRef {
                values: vec![current],
                ty: DirectType::Scalar(ScalarKind::Int64),
            },
        )?;
        let advanced_inst = self
            .builder
            .ins()
            .call(self.range_advance, &[range.values[0]]);
        let advanced = self.owned_opaque_result(
            self.builder.inst_results(advanced_inst).to_vec(),
            Type::named("Range"),
        );
        self.store_place(iterable_place, advanced)?;
        self.builder.ins().jump(body_block, &[]);
        self.builder.seal_block(next_block);
        let _ = binding_ty;
        Ok(())
    }

    fn compile_trait_member_call(
        &mut self,
        object: &Operand,
        trait_name: &str,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
    ) -> std::result::Result<ValueRef, String> {
        let object = self.load_operand(object)?;
        let receiver_ty = direct_type_to_type(&object.ty);
        if let Type::Named(receiver_name, _) = &receiver_ty {
            self.compile_class_member_call(
                receiver_name,
                Some(receiver_ty.clone()),
                object,
                field,
                receiver_place,
                args,
                Some(trait_name),
            )
        } else {
            self.compile_opaque_member_call(
                &receiver_ty,
                object,
                field,
                receiver_place,
                args,
                Some(trait_name),
            )
        }
    }

    fn compile_member_call(
        &mut self,
        object: &Operand,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
    ) -> std::result::Result<ValueRef, String> {
        let object = self.load_operand(object)?;

        if let DirectType::Opaque(Type::Named(name, _)) = &object.ty {
            if let Some(
                member @ (BuiltinMember::DurationToMilliseconds | BuiltinMember::DurationToSeconds),
            ) = BuiltinMember::resolve_runtime(name, field)
            {
                if !args.is_empty() {
                    return Err(format!(
                        "direct backend expected `{}()` to take no arguments",
                        member.name()
                    ));
                }
                let unit_nanoseconds = match member {
                    BuiltinMember::DurationToMilliseconds => {
                        crate::runtime_value::NANOS_PER_MILLISECOND
                    }
                    BuiltinMember::DurationToSeconds => crate::runtime_value::NANOS_PER_SECOND,
                    _ => unreachable!("Duration conversion match is exhaustive"),
                };
                let unit_nanoseconds = self
                    .builder
                    .ins()
                    .iconst(types::I64, unit_nanoseconds as i64);
                let inst = self.builder.ins().call(
                    self.duration_to_float,
                    &[object.values[0], unit_nanoseconds],
                );
                return Ok(ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Scalar(ScalarKind::Float64),
                });
            }
        }

        if matches!(object.ty.scalar_kind(), Some(kind) if kind.is_float()) && field == "sqrt" {
            if !args.is_empty() {
                return Err("direct backend expected `sqrt()` to take no arguments".to_string());
            }
            let inst = self.builder.ins().call(self.sqrt_f64, &[object.values[0]]);
            return Ok(ValueRef {
                values: self.builder.inst_results(inst).to_vec(),
                ty: object.ty,
            });
        }
        if object.ty.scalar_kind().is_some()
            && matches!(field, "add" | "sub" | "mul" | "div")
            && args.len() == 1
        {
            let right = self.load_operand(&args[0].value)?;
            if let Some(element_type) = direct_array_element_type(&right.ty).cloned() {
                let element_direct =
                    ensure_direct_type(&element_type, &self.classes, "Array scalar operand")?;
                let left = self.coerce_value(object, &element_direct)?;
                let left = self.ensure_opaque(left)?;
                let array_ty = direct_type_to_type(&right.ty);
                let right = self.ensure_opaque(right)?;
                let scalar_left = self.builder.ins().iconst(types::I64, 1);
                let operation_code = match field {
                    "add" => 0,
                    "sub" => 1,
                    "mul" => 2,
                    _ => {
                        debug_assert_eq!(field, "div");
                        3
                    }
                };
                let operation = self.builder.ins().iconst(types::I64, operation_code);
                let checked_mode = self.builder.ins().iconst(types::I64, 0);
                let zero = self.builder.ins().iconst(types::I64, 0);
                let inst = self.builder.ins().call(
                    self.array_binary,
                    &[
                        left.values[0],
                        right.values[0],
                        scalar_left,
                        operation,
                        checked_mode,
                        zero,
                        zero,
                    ],
                );
                return Ok(
                    self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), array_ty)
                );
            }
        }

        match object.ty.clone() {
            DirectType::PlainClass(class_ty) => self.compile_class_member_call(
                class_ty.class_name.as_str(),
                Some(Type::named(&class_ty.class_name)),
                object,
                field,
                receiver_place,
                args,
                None,
            ),
            DirectType::Opaque(ty) => {
                if let Type::Named(_name, _type_args) = &ty {
                    return self.compile_opaque_member_call(
                        &ty,
                        object,
                        field,
                        receiver_place,
                        args,
                        None,
                    );
                }
                self.compile_opaque_member_call(&ty, object, field, receiver_place, args, None)
            }
            DirectType::Scalar(_) => {
                if matches!(object.ty.scalar_kind(), Some(kind) if kind.is_integer())
                    && matches!(
                        field,
                        "wrapping_add"
                            | "wrapping_sub"
                            | "wrapping_mul"
                            | "saturating_add"
                            | "saturating_sub"
                            | "saturating_mul"
                            | "wrapping_shl"
                            | "wrapping_shr"
                            | "saturating_shl"
                            | "saturating_shr"
                    )
                {
                    let argument_name = if field.ends_with("shl") || field.ends_with("shr") {
                        "count"
                    } else {
                        "rhs"
                    };
                    let ordered = ordered_named_args(&[argument_name], args)?;
                    let argument = ordered[0];
                    let target = object.ty.clone();
                    let left = self.ensure_opaque(object)?;
                    let right = self.load_operand_as_opaque_direct(&argument.value, &target)?;
                    let operation = match field {
                        "wrapping_add" | "saturating_add" => 0,
                        "wrapping_sub" | "saturating_sub" => 1,
                        "wrapping_shl" | "saturating_shl" => 3,
                        "wrapping_shr" | "saturating_shr" => 4,
                        _ => {
                            debug_assert!(matches!(field, "wrapping_mul" | "saturating_mul"));
                            2
                        }
                    };
                    let arithmetic_mode = if field.starts_with("wrapping_") { 1 } else { 2 };
                    let operation = self.builder.ins().iconst(types::I64, operation);
                    let arithmetic_mode = self.builder.ins().iconst(types::I64, arithmetic_mode);
                    let zero = self.builder.ins().iconst(types::I64, 0);
                    let inst = self.builder.ins().call(
                        self.integer_width_binary,
                        &[
                            left.values[0],
                            right.values[0],
                            operation,
                            arithmetic_mode,
                            zero,
                            zero,
                        ],
                    );
                    let result = self.owned_opaque_result(
                        self.builder.inst_results(inst).to_vec(),
                        direct_type_to_type(&target),
                    );
                    return self.coerce_value(result, &target);
                }
                if field == "to_float"
                    && matches!(object.ty.scalar_kind(), Some(kind) if kind.is_integer())
                {
                    if !args.is_empty() {
                        return Err(DIRECT_TO_FLOAT_ARITY_ERROR.to_string());
                    }
                    let value = if matches!(object.ty.scalar_kind(), Some(ScalarKind::Uint64)) {
                        self.builder
                            .ins()
                            .fcvt_from_uint(types::F64, object.values[0])
                    } else {
                        self.builder
                            .ins()
                            .fcvt_from_sint(types::F64, object.values[0])
                    };
                    return Ok(ValueRef {
                        values: vec![value],
                        ty: DirectType::Scalar(ScalarKind::Float64),
                    });
                }
                if field == "to_string" {
                    if !args.is_empty() {
                        return Err("direct backend expected `to_string()` to take no arguments"
                            .to_string());
                    }
                    let object = self.ensure_opaque(object)?;
                    let inst = self
                        .builder
                        .ins()
                        .call(self.stringify_value, &[object.values[0]]);
                    return Ok(self.owned_opaque_result(
                        self.builder.inst_results(inst).to_vec(),
                        Type::named("str"),
                    ));
                }
                let receiver_ty = direct_type_to_type(&object.ty);
                if self.find_trait_method(&receiver_ty, field).is_some() {
                    return self.compile_class_member_call(
                        &receiver_ty.to_string(),
                        Some(receiver_ty),
                        object,
                        field,
                        receiver_place,
                        args,
                        None,
                    );
                }
                Err(format!(
                    "direct backend does not support member call `.{}` on `{}`",
                    field,
                    render_direct_type(&object.ty)
                ))
            }
        }
    }

    fn compile_construct(
        &mut self,
        class_name: &str,
        fields: &[crate::mir::MirFieldInit],
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        let ty = match target {
            DirectType::PlainClass(class) if class.class_name == class_name => target.clone(),
            DirectType::Opaque(Type::Named(name, _)) if name == class_name => target.clone(),
            _ => ensure_direct_type(
                &Type::named(class_name),
                &self.classes,
                &format!("class `{}`", class_name),
            )?,
        };
        match &ty {
            DirectType::PlainClass(class_ty) => {
                let mut by_name = HashMap::new();
                for field in fields {
                    by_name.insert(field.name.clone(), field.value.clone());
                }

                let mut values = Vec::new();
                for field in &class_ty.fields {
                    let operand = by_name.get(&field.name).ok_or(format!(
                        "direct backend construction for `{}` is missing field `{}`",
                        class_name, field.name
                    ))?;
                    let value = self.load_operand_for_target(operand, &field.ty)?;
                    let coerced = self.coerce_value(value, &field.ty)?;
                    values.extend(coerced.values);
                }

                Ok(ValueRef {
                    values,
                    ty: ty.clone(),
                })
            }
            DirectType::Opaque(target_ty) => {
                self.compile_opaque_construct(class_name, fields, target_ty)
            }
            DirectType::Scalar(_) => Err(format!(
                "direct backend could not construct non-class type `{}`",
                class_name
            )),
        }
    }

    fn call_result_type(&self, name: &str) -> std::result::Result<DirectType, String> {
        self.function_return_types.get(name).cloned().ok_or(format!(
            "direct backend does not know return type for `{}`",
            name
        ))
    }

    fn resolve_view_place(&self, place: &str) -> std::result::Result<DirectViewPlace, String> {
        let (root, projection) = place.split_once('.').unwrap_or((place, ""));
        if let Some(source) = self.view_places.get(root) {
            return Ok(source.clone().project(projection));
        }
        Ok(DirectViewPlace::static_place(place.to_string()))
    }

    fn type_of_place(&self, place: &str) -> std::result::Result<DirectType, String> {
        let (virtual_root, virtual_projection) = place.split_once('.').unwrap_or((place, ""));
        if self.view_places.contains_key(virtual_root) {
            if let Some(mut ty) = self.variable_types.get(virtual_root).cloned() {
                for field in virtual_projection
                    .split('.')
                    .filter(|segment| !segment.is_empty())
                {
                    ty = direct_field_type(&ty, field, &self.classes).ok_or(format!(
                        "direct backend does not know field `{}` on `{}`",
                        field,
                        render_direct_type(&ty)
                    ))?;
                }
                return Ok(ty);
            }
        }
        let resolved = self.resolve_view_place(place)?;
        let place = resolved
            .alternatives
            .first()
            .ok_or_else(|| format!("direct backend view `{place}` has no place alternatives"))?
            .place
            .as_str();
        let mut segments = place.split('.');
        let root = segments
            .next()
            .ok_or("direct backend encountered an empty place".to_string())?;
        let mut ty = self.local_type(root)?;
        for field in segments {
            ty = direct_field_type(&ty, field, &self.classes).ok_or(format!(
                "direct backend does not know field `{}` on `{}`",
                field,
                render_direct_type(&ty)
            ))?;
        }
        Ok(ty)
    }

    fn load_operand(&mut self, operand: &Operand) -> std::result::Result<ValueRef, String> {
        match operand {
            Operand::Place(place) => self.load_place(place),
            Operand::MovePlace(place) => self.take_place(place),
            Operand::Function { name, signature } => {
                let thunk_ref = *self.function_thunk_refs.get(name).ok_or_else(|| {
                    format!("direct backend does not know function thunk for `{name}`")
                })?;
                let thunk_ptr = self.builder.ins().func_addr(types::I64, thunk_ref);
                let binder_ref = *self.function_default_binder_refs.get(name).ok_or_else(|| {
                    format!("direct backend does not know function default binder for `{name}`")
                })?;
                let binder_ptr = self.builder.ins().func_addr(types::I64, binder_ref);
                let (name_ptr, name_len) = self.string_constant(name.as_bytes())?;
                let signature_json = match serde_json::to_vec(signature) {
                    Ok(signature_json) => signature_json,
                    Err(error) => {
                        return Err(format!(
                            "failed to serialize function signature for `{name}`: {error}"
                        ))
                    }
                };
                let (signature_ptr, signature_len) = self.string_constant(&signature_json)?;
                let (path, span) = match self.function_frame_metadata.get(name).cloned() {
                    Some(metadata) => metadata,
                    None => {
                        return Err(format!(
                            "direct backend is missing source-frame metadata for function `{name}`"
                        ))
                    }
                };
                let (path_ptr, path_len) = self.string_constant(path.as_bytes())?;
                let line = self.builder.ins().iconst(types::I64, span.line as i64);
                let column = self.builder.ins().iconst(types::I64, span.column as i64);
                let call = self.builder.ins().call(
                    self.function_value,
                    &[
                        thunk_ptr,
                        binder_ptr,
                        name_ptr,
                        name_len,
                        signature_ptr,
                        signature_len,
                        path_ptr,
                        path_len,
                        line,
                        column,
                    ],
                );
                Ok(self.owned_opaque_result(
                    self.builder.inst_results(call).to_vec(),
                    signature.as_ref().clone(),
                ))
            }
            Operand::Int(value) => {
                if let Ok(narrowed) = i64::try_from(*value) {
                    return Ok(ValueRef {
                        values: vec![self.builder.ins().iconst(types::I64, narrowed)],
                        ty: DirectType::Scalar(ScalarKind::Int64),
                    });
                }
                let (ptr, len) = self.string_constant(value.to_string().as_bytes())?;
                let inst = self.builder.ins().call(self.box_uint_literal, &[ptr, len]);
                Ok(self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    Type::named("Unknown"),
                ))
            }
            Operand::Float(value) => Ok(ValueRef {
                values: vec![self.builder.ins().f64const(Ieee64::with_float(*value))],
                ty: DirectType::Scalar(ScalarKind::Float64),
            }),
            Operand::String(value) => {
                let (ptr, len) = self.string_constant(value.as_bytes())?;
                let inst = self.builder.ins().call(self.string_literal, &[ptr, len]);
                Ok(self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    Type::named("str"),
                ))
            }
            Operand::Duration(value) => {
                let low = self.builder.ins().iconst(types::I64, *value as i64);
                let high = self.builder.ins().iconst(types::I64, (*value >> 64) as i64);
                let inst = self.builder.ins().call(self.duration_literal, &[low, high]);
                Ok(self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    Type::named("Duration"),
                ))
            }
            Operand::Bool(value) => Ok(ValueRef {
                values: vec![self
                    .builder
                    .ins()
                    .iconst(types::I64, if *value { 1 } else { 0 })],
                ty: DirectType::Scalar(ScalarKind::Bool),
            }),
            Operand::Unit => Ok(unit_value(&mut self.builder)),
        }
    }

    fn operand_integer_kind(&self, operand: &Operand) -> Option<ScalarKind> {
        let kind = match operand {
            Operand::Place(place) | Operand::MovePlace(place) => {
                self.type_of_place(place).ok()?.scalar_kind()?
            }
            Operand::Int(_)
            | Operand::Function { .. }
            | Operand::Float(_)
            | Operand::String(_)
            | Operand::Duration(_)
            | Operand::Bool(_)
            | Operand::Unit => return None,
        };
        kind.is_integer().then_some(kind)
    }

    fn load_operand_with_integer_hint(
        &mut self,
        operand: &Operand,
        hint: Option<ScalarKind>,
    ) -> std::result::Result<ValueRef, String> {
        if let (Operand::Int(value), Some(kind)) = (operand, hint) {
            let raw = match kind {
                ScalarKind::Int32 | ScalarKind::Int64 => i64::try_from(*value).ok(),
                ScalarKind::Uint64 => u64::try_from(*value).ok().map(|value| value as i64),
                ScalarKind::Float32 | ScalarKind::Float64 | ScalarKind::Bool | ScalarKind::Unit => {
                    None
                }
            };
            if let Some(raw) = raw {
                return Ok(ValueRef {
                    values: vec![self.builder.ins().iconst(types::I64, raw)],
                    ty: DirectType::Scalar(kind),
                });
            }
        }
        self.load_operand(operand)
    }

    fn load_operand_for_target(
        &mut self,
        operand: &Operand,
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        let hint = target.scalar_kind().filter(|kind| kind.is_integer());
        self.load_operand_with_integer_hint(operand, hint)
    }

    fn load_operand_as_opaque_direct(
        &mut self,
        operand: &Operand,
        expected_ty: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        let loaded = self.load_operand_for_target(operand, expected_ty)?;
        let coerced = self.coerce_value(loaded, expected_ty)?;
        self.ensure_opaque(coerced)
    }

    fn load_place(&mut self, place: &str) -> std::result::Result<ValueRef, String> {
        let resolved = self.resolve_view_place(place)?;
        if resolved.alternatives.len() == 1 && resolved.alternatives[0].conditions.is_empty() {
            return self.load_static_place(&resolved.alternatives[0].place);
        }
        self.load_selected_view_place(place, resolved)
    }

    fn load_static_place(&mut self, place: &str) -> std::result::Result<ValueRef, String> {
        let mut segments = place.split('.');
        let root = segments
            .next()
            .ok_or("direct backend encountered an empty place".to_string())?;
        let mut value = self.load_root(root)?;
        for field in segments {
            value = self.extract_field(value, field)?;
        }
        Ok(value)
    }

    fn view_alternative_condition(&mut self, alternative: &DirectViewAlternative) -> Value {
        let mut conditions = alternative.conditions.iter();
        let Some((selector, expected)) = conditions.next() else {
            return self.builder.ins().iconst(types::I64, 1);
        };
        let selector = self.builder.use_var(*selector);
        let mut matches = self
            .builder
            .ins()
            .icmp_imm(IntCC::Equal, selector, *expected);
        for (selector, expected) in conditions {
            let selector = self.builder.use_var(*selector);
            let next = self
                .builder
                .ins()
                .icmp_imm(IntCC::Equal, selector, *expected);
            matches = self.builder.ins().band(matches, next);
        }
        matches
    }

    fn emit_returned_view_projection(
        &mut self,
        loan: &str,
        origin: &str,
    ) -> std::result::Result<(), String> {
        let sources = self.resolve_view_place(loan)?;
        let origins = self.resolve_view_place(origin)?;
        let mut alternatives = Vec::<(String, Vec<(Variable, i64)>)>::new();
        for source in &sources.alternatives {
            for origin in &origins.alternatives {
                let projection = if source.place == origin.place {
                    Some(String::new())
                } else {
                    source
                        .place
                        .strip_prefix(&format!("{}.", origin.place))
                        .map(str::to_string)
                };
                let Some(projection) = projection else {
                    continue;
                };
                let mut conditions = origin.conditions.clone();
                let mut compatible = true;
                for condition in &source.conditions {
                    if let Some((_, expected)) = conditions
                        .iter()
                        .find(|(selector, _)| selector == &condition.0)
                    {
                        if *expected != condition.1 {
                            compatible = false;
                            break;
                        }
                    } else {
                        conditions.push(*condition);
                    }
                }
                if compatible
                    && !alternatives
                        .iter()
                        .any(|candidate| candidate.0 == projection && candidate.1 == conditions)
                {
                    alternatives.push((projection, conditions));
                }
            }
        }
        if alternatives.is_empty() {
            return Err(format!(
                "direct returned loan `{loan}` has no projection within origin `{origin}`"
            ));
        }

        let merge = self.builder.create_block();
        let alternative_count = alternatives.len();
        for (index, (projection, conditions)) in alternatives.into_iter().enumerate() {
            if index + 1 < alternative_count {
                let selected = self.view_alternative_condition(&DirectViewAlternative {
                    place: String::new(),
                    conditions,
                });
                let set_block = self.builder.create_block();
                let next_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(selected, set_block, &[], next_block, &[]);
                self.builder.switch_to_block(set_block);
                self.set_returned_view_projection_value(&projection)?;
                self.builder.ins().jump(merge, &[]);
                self.builder.seal_block(set_block);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            } else {
                self.set_returned_view_projection_value(&projection)?;
                self.builder.ins().jump(merge, &[]);
            }
        }
        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(())
    }

    fn set_returned_view_projection_value(
        &mut self,
        projection: &str,
    ) -> std::result::Result<(), String> {
        let (projection_ptr, projection_len) = self.string_constant(projection.as_bytes())?;
        self.builder.ins().call(
            self.set_returned_view_projection,
            &[projection_ptr, projection_len],
        );
        Ok(())
    }

    fn load_selected_view_place(
        &mut self,
        view: &str,
        resolved: DirectViewPlace,
    ) -> std::result::Result<ValueRef, String> {
        let target_ty = self.type_of_place(view)?;
        let merge = self.builder.create_block();
        for abi in target_ty.abi_types() {
            self.builder.append_block_param(merge, abi);
        }

        let caller_owned = self.owned_opaque_temporaries.clone();
        let alternative_count = resolved.alternatives.len();
        let mut merged_owned = None;
        for (index, alternative) in resolved.alternatives.into_iter().enumerate() {
            if index + 1 < alternative_count {
                let selected = self.view_alternative_condition(&alternative);
                let load_block = self.builder.create_block();
                let next_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(selected, load_block, &[], next_block, &[]);
                self.builder.switch_to_block(load_block);
                self.owned_opaque_temporaries = caller_owned.clone();
                let value = self.load_static_place(&alternative.place)?;
                let value = self.coerce_value(value, &target_ty)?;
                let value_is_owned = self.temporary_owns_opaque(&value);
                if let Some(expected) = merged_owned {
                    if expected != value_is_owned {
                        return Err(format!(
                            "direct view `{view}` alternatives disagree on value ownership"
                        ));
                    }
                } else {
                    merged_owned = Some(value_is_owned);
                }
                self.clear_temporary_opaque_owned(&value);
                self.release_temporary_owned_since(&caller_owned);
                self.builder.ins().jump(merge, &value.values);
                self.builder.seal_block(load_block);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            } else {
                self.owned_opaque_temporaries = caller_owned.clone();
                let value = self.load_static_place(&alternative.place)?;
                let value = self.coerce_value(value, &target_ty)?;
                let value_is_owned = self.temporary_owns_opaque(&value);
                if let Some(expected) = merged_owned {
                    if expected != value_is_owned {
                        return Err(format!(
                            "direct view `{view}` alternatives disagree on value ownership"
                        ));
                    }
                } else {
                    merged_owned = Some(value_is_owned);
                }
                self.clear_temporary_opaque_owned(&value);
                self.release_temporary_owned_since(&caller_owned);
                self.builder.ins().jump(merge, &value.values);
            }
        }
        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        self.owned_opaque_temporaries = caller_owned;
        let value = ValueRef {
            values: self.builder.block_params(merge).to_vec(),
            ty: target_ty,
        };
        if merged_owned == Some(true) {
            self.mark_temporary_opaque_owned(&value);
        }
        Ok(value)
    }

    fn take_place(&mut self, place: &str) -> std::result::Result<ValueRef, String> {
        let segments = place.split('.').collect::<Vec<_>>();
        let Some(root) = segments.first().copied() else {
            return Err("direct backend encountered an empty move place".to_string());
        };
        let root_ty = self.local_type(root)?;
        if segments.len() == 1 {
            let vars = self.local_vars(root)?;
            let values = vars
                .iter()
                .map(|var| self.builder.use_var(*var))
                .collect::<Vec<_>>();
            for (var, abi) in vars.into_iter().zip(root_ty.abi_types()) {
                let zero = if abi == types::F64 {
                    self.builder.ins().f64const(Ieee64::with_float(0.0))
                } else {
                    self.builder.ins().iconst(abi, 0)
                };
                self.builder.def_var(var, zero);
            }
            let moved = ValueRef {
                values,
                ty: root_ty,
            };
            self.mark_temporary_opaque_owned(&moved);
            return Ok(moved);
        }

        let moved_ty = self.type_of_place(place)?;
        if matches!(&root_ty, DirectType::Opaque(_)) {
            let root_value = self.load_root(root)?;
            let path = segments[1..].join(".");
            let (field_ptr, field_len) = self.string_constant(path.as_bytes())?;
            let inst = self.builder.ins().call(
                self.instance_take_field,
                &[root_value.values[0], field_ptr, field_len],
            );
            let moved = ValueRef {
                values: self.builder.inst_results(inst).to_vec(),
                ty: DirectType::Opaque(direct_type_to_type(&moved_ty)),
            };
            self.mark_temporary_opaque_owned(&moved);
            return self.coerce_value(moved, &moved_ty);
        }

        let vars = self.local_vars(root)?;
        let mut current_ty = root_ty;
        let mut start = 0usize;
        let mut end = vars.len();
        for field in &segments[1..] {
            let (field_start, field_end, field_ty) =
                required_direct_field_slice(&current_ty, field)?;
            start += field_start;
            end = start + (field_end - field_start);
            current_ty = field_ty;
        }
        let values = vars[start..end]
            .iter()
            .map(|var| self.builder.use_var(*var))
            .collect::<Vec<_>>();
        for (var, abi) in vars[start..end].iter().copied().zip(current_ty.abi_types()) {
            let zero = if abi == types::F64 {
                self.builder.ins().f64const(Ieee64::with_float(0.0))
            } else {
                self.builder.ins().iconst(abi, 0)
            };
            self.builder.def_var(var, zero);
        }
        let moved = ValueRef {
            values,
            ty: current_ty,
        };
        self.mark_temporary_opaque_owned(&moved);
        Ok(moved)
    }

    fn load_root(&mut self, name: &str) -> std::result::Result<ValueRef, String> {
        let vars = self.local_vars(name)?;
        let ty = self.local_type(name)?;
        let values = vars
            .into_iter()
            .map(|var| self.builder.use_var(var))
            .collect::<Vec<_>>();
        let value = ValueRef { values, ty };
        if matches!(value.ty, DirectType::Opaque(_)) {
            self.clear_temporary_opaque_owned(&value);
        }
        Ok(value)
    }

    fn extract_field(
        &mut self,
        object: ValueRef,
        field: &str,
    ) -> std::result::Result<ValueRef, String> {
        match &object.ty {
            DirectType::PlainClass(_) => {
                let (start, end, field_ty) = required_direct_field_slice(&object.ty, field)?;
                Ok(ValueRef {
                    values: object.values[start..end].to_vec(),
                    ty: field_ty,
                })
            }
            DirectType::Opaque(Type::Tuple(elements)) => {
                let index = field.parse::<usize>().map_err(|_| {
                    format!("direct backend tuple projection `{field}` is not a fixed position")
                })?;
                let element_type = elements.get(index).cloned().ok_or_else(|| {
                    format!(
                        "direct backend tuple of length {} has no element at index {index}",
                        elements.len()
                    )
                })?;
                let index_value = self.builder.ins().iconst(types::I64, index as i64);
                let inst = self
                    .builder
                    .ins()
                    .call(self.tuple_element, &[object.values[0], index_value]);
                let element = self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    element_type.clone(),
                );
                let direct_type =
                    ensure_direct_type(&element_type, &self.classes, "tuple view element")?;
                self.coerce_value(element, &direct_type)
            }
            DirectType::Opaque(_) => {
                let (field_ptr, field_len) = self.string_constant(field.as_bytes())?;
                let inst = self.builder.ins().call(
                    self.instance_get_field,
                    &[object.values[0], field_ptr, field_len],
                );
                let loaded = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("Unknown")),
                };
                self.mark_temporary_opaque_owned(&loaded);
                if let Some(field_ty) = direct_field_type(&object.ty, field, &self.classes) {
                    self.coerce_value(loaded, &field_ty)
                } else {
                    Ok(loaded)
                }
            }
            DirectType::Scalar(_) => Err(format!(
                "direct backend does not know field `{}` on `{}`",
                field,
                render_direct_type(&object.ty)
            )),
        }
    }

    fn coerce_value(
        &mut self,
        value: ValueRef,
        target: &DirectType,
    ) -> std::result::Result<ValueRef, String> {
        self.coerce_value_at(value, target, None)
    }

    fn coerce_value_at(
        &mut self,
        value: ValueRef,
        target: &DirectType,
        span: Option<Span>,
    ) -> std::result::Result<ValueRef, String> {
        if &value.ty == target {
            let value = self.normalize_scalar_value(value)?;
            if matches!(target.scalar_kind(), Some(ScalarKind::Int32)) {
                self.emit_int32_bounds_check(value.values[0], span)?;
            }
            if let DirectType::Opaque(target_ty) = target {
                self.tag_opaque_runtime_type(&value, target_ty)?;
            }
            return Ok(value);
        }

        if let DirectType::Opaque(target_ty) = target {
            if matches!(value.ty.scalar_kind(), Some(ScalarKind::Unit))
                && matches!(target_ty, Type::Named(name, args) if name == "Option" && args.len() == 1)
            {
                let none =
                    self.compile_enum_variant_for_target("Option", "None", &[], Some(target))?;
                self.tag_opaque_runtime_type(&none, target_ty)?;
                return Ok(ValueRef {
                    values: none.values,
                    ty: target.clone(),
                });
            }
            if is_numeric_type_name(target_ty) {
                let boxed = self.ensure_opaque(value)?;
                let (target_ptr, target_len) =
                    self.string_constant(target_ty.to_string().as_bytes())?;
                let (line, column) = self.span_values(span);
                let inst = self.builder.ins().call(
                    self.cast_value,
                    &[boxed.values[0], target_ptr, target_len, line, column],
                );
                let casted = self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    direct_type_to_type(target),
                );
                self.tag_opaque_runtime_type(&casted, target_ty)?;
                return Ok(casted);
            }
            let mut boxed = self.ensure_opaque(value)?;
            self.tag_opaque_runtime_type(&boxed, target_ty)?;
            boxed.ty = target.clone();
            return Ok(boxed);
        }

        if matches!(value.ty, DirectType::Opaque(_)) {
            let result = match target {
                DirectType::Scalar(ScalarKind::Int32) => {
                    let inst = self.builder.ins().call(self.unbox_i64, &[value.values[0]]);
                    ValueRef {
                        values: self.builder.inst_results(inst).to_vec(),
                        ty: target.clone(),
                    }
                }
                DirectType::Scalar(ScalarKind::Int64) => {
                    let inst = self
                        .builder
                        .ins()
                        .call(self.unbox_int64, &[value.values[0]]);
                    ValueRef {
                        values: self.builder.inst_results(inst).to_vec(),
                        ty: target.clone(),
                    }
                }
                DirectType::Scalar(ScalarKind::Uint64) => {
                    let inst = self.builder.ins().call(self.unbox_u64, &[value.values[0]]);
                    ValueRef {
                        values: self.builder.inst_results(inst).to_vec(),
                        ty: target.clone(),
                    }
                }
                DirectType::Scalar(ScalarKind::Float32)
                | DirectType::Scalar(ScalarKind::Float64) => {
                    let inst = self.builder.ins().call(self.unbox_f64, &[value.values[0]]);
                    self.normalize_scalar_value(ValueRef {
                        values: self.builder.inst_results(inst).to_vec(),
                        ty: target.clone(),
                    })?
                }
                DirectType::Scalar(ScalarKind::Bool) => {
                    let inst = self.builder.ins().call(self.unbox_bool, &[value.values[0]]);
                    ValueRef {
                        values: self.builder.inst_results(inst).to_vec(),
                        ty: target.clone(),
                    }
                }
                DirectType::Scalar(ScalarKind::Unit) => unit_value(&mut self.builder),
                DirectType::PlainClass(class) => {
                    let mut values = Vec::new();
                    for field in &class.fields {
                        let (field_ptr, field_len) = self.string_constant(field.name.as_bytes())?;
                        let inst = self.builder.ins().call(
                            self.instance_get_field,
                            &[value.values[0], field_ptr, field_len],
                        );
                        let field_value = ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Opaque(Type::named("Unknown")),
                        };
                        self.mark_temporary_opaque_owned(&field_value);
                        let coerced = self.coerce_value_at(field_value, &field.ty, span)?;
                        values.extend(coerced.values);
                    }
                    ValueRef {
                        values,
                        ty: target.clone(),
                    }
                }
                DirectType::Opaque(_) => unreachable!("opaque target handled earlier"),
            };
            if matches!(target.scalar_kind(), Some(ScalarKind::Int32)) {
                self.emit_int32_bounds_check(result.values[0], span)?;
            }
            return Ok(result);
        }

        match (value.ty.scalar_kind(), target.scalar_kind()) {
            (Some(ScalarKind::Bool), Some(ScalarKind::Int32)) => Ok(ValueRef {
                values: value.values,
                ty: target.clone(),
            }),
            (Some(ScalarKind::Int32), Some(ScalarKind::Int64)) => Ok(ValueRef {
                values: value.values,
                ty: target.clone(),
            }),
            (Some(ScalarKind::Int64), Some(ScalarKind::Int32)) => {
                self.emit_int32_bounds_check(value.values[0], span)?;
                Ok(ValueRef {
                    values: value.values,
                    ty: target.clone(),
                })
            }
            (Some(lhs), Some(rhs)) if lhs.is_float() && rhs.is_float() => self
                .normalize_scalar_value(ValueRef {
                    values: value.values,
                    ty: target.clone(),
                }),
            (Some(ScalarKind::Int32), Some(ScalarKind::Bool)) => Ok(ValueRef {
                values: value.values,
                ty: target.clone(),
            }),
            (Some(ScalarKind::Unit), Some(ScalarKind::Int32)) => Ok(ValueRef {
                values: vec![self.builder.ins().iconst(types::I64, 0)],
                ty: target.clone(),
            }),
            _ => Err(format!(
                "direct backend encountered an unsupported value coercion from `{}` to `{}`",
                render_direct_type(&value.ty),
                render_direct_type(target)
            )),
        }
    }

    fn normalize_scalar_value(&mut self, value: ValueRef) -> std::result::Result<ValueRef, String> {
        match value.ty.scalar_kind() {
            Some(ScalarKind::Float32) => {
                let narrowed = self.builder.ins().fdemote(types::F32, value.values[0]);
                let widened = self.builder.ins().fpromote(types::F64, narrowed);
                Ok(ValueRef {
                    values: vec![widened],
                    ty: value.ty,
                })
            }
            _ => Ok(value),
        }
    }

    fn tag_opaque_runtime_type(
        &mut self,
        value: &ValueRef,
        ty: &Type,
    ) -> std::result::Result<(), String> {
        if runtime_type_is_wildcard(ty) {
            return Ok(());
        }
        let [raw] = value.values.as_slice() else {
            return Err(format!(
                "direct backend expected opaque `{ty}` to use one runtime value"
            ));
        };
        let encoded = crate::native_runtime::canonical_runtime_type_name(ty);
        let (type_ptr, type_len) = self.string_constant(encoded.as_bytes())?;
        self.builder
            .ins()
            .call(self.tag_value_type, &[*raw, type_ptr, type_len]);
        Ok(())
    }

    fn tag_raw_opaque_runtime_type(
        &mut self,
        raw: Value,
        ty: &DirectType,
    ) -> std::result::Result<(), String> {
        let DirectType::Opaque(ty) = ty else {
            return Ok(());
        };
        if runtime_type_is_wildcard(ty) {
            return Ok(());
        }
        let encoded = crate::native_runtime::canonical_runtime_type_name(ty);
        let (type_ptr, type_len) = self.string_constant(encoded.as_bytes())?;
        self.builder
            .ins()
            .call(self.tag_value_type, &[raw, type_ptr, type_len]);
        Ok(())
    }

    fn emit_int32_bounds_check(
        &mut self,
        value: Value,
        span: Option<Span>,
    ) -> std::result::Result<(), String> {
        // `value` is an `int32` iff biasing it by `-i32::MIN` lands inside the
        // unsigned 32-bit range. That is one add and one compare, where the
        // signed two-sided form needs two constants, two compares, and an or.
        // This check runs on the result of every narrow arithmetic operation,
        // so its cost is what separated `int32` loops from `int64` ones, where
        // the overflow flag comes free with the add.
        let biased = self.builder.ins().iadd_imm(value, -(i32::MIN as i64));
        let overflow =
            self.builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, biased, u32::MAX as i64);
        let fail_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(overflow, fail_block, &[], continue_block, &[]);
        self.builder.switch_to_block(fail_block);
        self.emit_pending_cleanups(true)?;
        let (line, column) = self.span_values(span);
        self.builder
            .ins()
            .call(self.fail_int32_overflow, &[value, line, column]);
        self.builder.ins().trap(TrapCode::INTEGER_OVERFLOW);
        self.builder.seal_block(fail_block);
        self.builder.switch_to_block(continue_block);
        self.builder.seal_block(continue_block);
        Ok(())
    }

    fn emit_vec_index_failure_guard(
        &mut self,
        object: Value,
        index: Value,
        line: Value,
        column: Value,
    ) -> std::result::Result<Value, String> {
        let len_inst = self.builder.ins().call(self.vec_len, &[object]);
        let len = self.builder.inst_results(len_inst)[0];
        let zero = self.builder.ins().iconst(types::I64, 0);
        let negative = self.builder.ins().icmp(IntCC::SignedLessThan, index, zero);
        let from_end = self.builder.ins().iadd(len, index);
        let normalized = self.builder.ins().select(negative, from_end, index);
        let below = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, normalized, zero);
        let above = self
            .builder
            .ins()
            .icmp(IntCC::SignedGreaterThanOrEqual, normalized, len);
        let out_of_bounds = self.builder.ins().bor(below, above);
        let fail_block = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(out_of_bounds, fail_block, &[], continue_block, &[]);
        self.builder.switch_to_block(fail_block);
        self.emit_pending_cleanups(true)?;
        self.builder
            .ins()
            .call(self.vec_index, &[object, index, line, column]);
        self.builder.ins().trap(TrapCode::unwrap_user(2));
        self.builder.seal_block(fail_block);
        self.builder.switch_to_block(continue_block);
        self.builder.seal_block(continue_block);
        Ok(normalized)
    }

    fn as_bool_value(&mut self, value: ValueRef) -> std::result::Result<Value, String> {
        match value.ty.scalar_kind() {
            Some(ScalarKind::Bool) | Some(ScalarKind::Int32) | Some(ScalarKind::Unit) => {
                let zero = self.builder.ins().iconst(types::I64, 0);
                Ok(self
                    .builder
                    .ins()
                    .icmp(IntCC::NotEqual, value.values[0], zero))
            }
            None if matches!(value.ty, DirectType::Opaque(_)) => {
                let inst = self
                    .builder
                    .ins()
                    .call(self.value_as_condition, &[value.values[0]]);
                Ok(self.builder.inst_results(inst)[0])
            }
            other => Err(format!(
                "direct backend cannot use `{}` as a branch condition",
                match other {
                    Some(kind) => render_direct_type(&DirectType::Scalar(kind)),
                    None => render_direct_type(&value.ty),
                }
            )),
        }
    }

    fn store_place(&mut self, place: &str, value: ValueRef) -> std::result::Result<(), String> {
        let resolved = self.resolve_view_place(place)?;
        self.store_resolved_view_place(resolved, value)
    }

    fn store_resolved_view_place(
        &mut self,
        resolved: DirectViewPlace,
        value: ValueRef,
    ) -> std::result::Result<(), String> {
        if resolved.alternatives.len() == 1 && resolved.alternatives[0].conditions.is_empty() {
            return self.store_static_place(&resolved.alternatives[0].place, value);
        }
        self.store_selected_view_place(resolved, value)
    }

    fn store_static_place(
        &mut self,
        place: &str,
        value: ValueRef,
    ) -> std::result::Result<(), String> {
        let mut segments = place.split('.').collect::<Vec<_>>();
        let root = segments.remove(0);
        let result = if segments.is_empty() {
            self.store_root(root, value)
        } else if matches!(self.variable_types.get(root), Some(DirectType::Opaque(_))) {
            let current = self.load_root(root)?;
            let current = self.ensure_opaque(current)?;
            let updated_value = self.ensure_opaque(value)?;
            let updated_value = self.transfer_owned_opaque_value(&updated_value);
            let field_path = segments.join(".");
            let (field_ptr, field_len) = self.string_constant(field_path.as_bytes())?;
            self.builder.ins().call(
                self.instance_set_field_owned,
                &[current.values[0], field_ptr, field_len, updated_value],
            );
            Ok(())
        } else {
            let root_value = self.load_root(root)?;
            let updated = self.replace_nested_field(root_value, &segments, value)?;
            self.store_root(root, updated)
        };
        result?;
        self.publish_mutable_root_write_through(root)?;
        self.refresh_cleanup_registrations_for_mutation(place)
    }

    fn store_selected_view_place(
        &mut self,
        resolved: DirectViewPlace,
        value: ValueRef,
    ) -> std::result::Result<(), String> {
        let merge = self.builder.create_block();
        let caller_owned = self.owned_opaque_temporaries.clone();
        let value_was_owned = self.temporary_owns_opaque(&value);
        let alternative_count = resolved.alternatives.len();
        for (index, alternative) in resolved.alternatives.into_iter().enumerate() {
            if index + 1 < alternative_count {
                let selected = self.view_alternative_condition(&alternative);
                let store_block = self.builder.create_block();
                let next_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(selected, store_block, &[], next_block, &[]);
                self.builder.switch_to_block(store_block);
                self.owned_opaque_temporaries = caller_owned.clone();
                self.store_static_place(&alternative.place, value.clone())?;
                self.release_temporary_owned_since(&caller_owned);
                self.builder.ins().jump(merge, &[]);
                self.builder.seal_block(store_block);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            } else {
                self.owned_opaque_temporaries = caller_owned.clone();
                self.store_static_place(&alternative.place, value.clone())?;
                self.release_temporary_owned_since(&caller_owned);
                self.builder.ins().jump(merge, &[]);
            }
        }
        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        self.owned_opaque_temporaries = caller_owned;
        if value_was_owned {
            self.clear_temporary_opaque_owned(&value);
        }
        Ok(())
    }

    fn replace_nested_field(
        &mut self,
        current: ValueRef,
        segments: &[&str],
        new_value: ValueRef,
    ) -> std::result::Result<ValueRef, String> {
        let (head, rest) = split_field_path_segments(segments)?;
        let (start, end, field_ty) = required_direct_field_slice(&current.ty, head)?;

        let replacement = if rest.is_empty() {
            self.coerce_value(new_value, &field_ty)?
        } else {
            let nested = ValueRef {
                values: current.values[start..end].to_vec(),
                ty: field_ty.clone(),
            };
            self.replace_nested_field(nested, rest, new_value)?
        };

        let mut values = Vec::with_capacity(current.values.len());
        values.extend_from_slice(&current.values[..start]);
        values.extend(replacement.values);
        values.extend_from_slice(&current.values[end..]);
        Ok(ValueRef {
            values,
            ty: current.ty,
        })
    }

    fn store_root(&mut self, name: &str, value: ValueRef) -> std::result::Result<(), String> {
        let expected = self.local_type(name)?;
        let value = self.coerce_value(value, &expected)?;
        let vars = self.local_vars(name)?;
        if matches!(expected, DirectType::Opaque(_)) {
            let stored = if self.temporary_owns_opaque(&value) {
                self.clear_temporary_opaque_owned(&value);
                value.values[0]
            } else {
                self.retain_opaque_handle(value.values[0])
            };
            self.release_root_if_opaque(name)?;
            self.builder.def_var(vars[0], stored);
            return Ok(());
        }
        for (var, compiled) in vars.into_iter().zip(value.values) {
            self.builder.def_var(var, compiled);
        }
        Ok(())
    }

    fn ensure_opaque(&mut self, value: ValueRef) -> std::result::Result<ValueRef, String> {
        match value.ty {
            DirectType::Opaque(_) => Ok(value),
            DirectType::Scalar(ScalarKind::Int32) => {
                let inst = self.builder.ins().call(self.box_i32, &[value.values[0]]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("int32")),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::Scalar(ScalarKind::Int64) => {
                let inst = self.builder.ins().call(self.box_i64, &[value.values[0]]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("int64")),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::Scalar(ScalarKind::Uint64) => {
                let inst = self.builder.ins().call(self.box_u64, &[value.values[0]]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("uint64")),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::Scalar(ScalarKind::Float32) | DirectType::Scalar(ScalarKind::Float64) => {
                let inst = self.builder.ins().call(self.box_f64, &[value.values[0]]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("float64")),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::Scalar(ScalarKind::Bool) => {
                let inst = self.builder.ins().call(self.box_bool, &[value.values[0]]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::named("bool")),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::Scalar(ScalarKind::Unit) => {
                let inst = self.builder.ins().call(self.box_unit, &[]);
                let boxed = ValueRef {
                    values: self.builder.inst_results(inst).to_vec(),
                    ty: DirectType::Opaque(Type::Unit),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
            DirectType::PlainClass(class) => {
                let (class_ptr, class_len) = self.string_constant(class.class_name.as_bytes())?;
                let init = self
                    .builder
                    .ins()
                    .call(self.instance_empty, &[class_ptr, class_len]);
                let current = self.builder.inst_results(init)[0];
                let mut start = 0usize;
                for field in &class.fields {
                    let end = start + field.ty.value_count();
                    let field_value = ValueRef {
                        values: value.values[start..end].to_vec(),
                        ty: field.ty.clone(),
                    };
                    let field_value = self.ensure_opaque(field_value)?;
                    let field_value = self.transfer_owned_opaque_value(&field_value);
                    let (field_ptr, field_len) = self.string_constant(field.name.as_bytes())?;
                    self.builder.ins().call(
                        self.instance_set_field_owned,
                        &[current, field_ptr, field_len, field_value],
                    );
                    start = end;
                }
                let boxed = ValueRef {
                    values: vec![current],
                    ty: DirectType::Opaque(Type::named(&class.class_name)),
                };
                self.mark_temporary_opaque_owned(&boxed);
                Ok(boxed)
            }
        }
    }

    fn binary_opcode(op: BinaryOp) -> i64 {
        match op {
            BinaryOp::Add => 0,
            BinaryOp::Sub => 1,
            BinaryOp::Mul => 2,
            BinaryOp::Div => 3,
            BinaryOp::Mod => 4,
            BinaryOp::Eq => 5,
            BinaryOp::NotEq => 6,
            BinaryOp::Less => 7,
            BinaryOp::LessEq => 8,
            BinaryOp::Greater => 9,
            BinaryOp::GreaterEq => 10,
            BinaryOp::And => 11,
            BinaryOp::Or => 12,
            BinaryOp::FloorDiv => 13,
            BinaryOp::Pow => 14,
            BinaryOp::BitAnd => 15,
            BinaryOp::BitOr => 16,
            BinaryOp::BitXor => 17,
            BinaryOp::Shl => 18,
            BinaryOp::Shr => 19,
        }
    }

    fn string_constant(&mut self, bytes: &[u8]) -> std::result::Result<(Value, Value), String> {
        declare_string_constant(self.object, self.string_data, &mut self.builder, bytes)
    }

    /// Compiles a tuple literal as one opaque aggregate handle.
    ///
    /// Frontend integration calls this from `Rvalue::TupleLiteral`; the
    /// semantic tuple type is passed explicitly so the opaque result keeps its
    /// exact structural runtime type. Elements are evaluated in source order,
    /// coerced to their semantic element types, then transferred into the
    /// aggregate. Destructive extraction is kept in a separate private-temp
    /// helper so this path cannot create user-visible partial moves.
    fn compile_tuple_literal(
        &mut self,
        elements: &[Operand],
        element_types: &[Type],
        tuple_type: Type,
    ) -> std::result::Result<ValueRef, String> {
        debug_assert_eq!(elements.len(), element_types.len());
        let count = self.builder.ins().iconst(types::I64, elements.len() as i64);
        let buffer = if elements.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let buffer_inst = self.builder.ins().call(self.arg_buffer_new, &[count]);
            let buffer = self.builder.inst_results(buffer_inst)[0];
            for (index, (element, element_type)) in elements.iter().zip(element_types).enumerate() {
                let direct_type = ensure_direct_type(element_type, &self.classes, "tuple element")?;
                let value = self.load_operand_for_target(element, &direct_type)?;
                let value = self.coerce_value(value, &direct_type)?;
                let value = self.ensure_opaque(value)?;
                let transferred = self.transfer_owned_opaque_value(&value);
                let index = self.builder.ins().iconst(types::I64, index as i64);
                self.builder
                    .ins()
                    .call(self.arg_buffer_store_owned, &[buffer, index, transferred]);
            }
            buffer
        };
        let inst = self.builder.ins().call(self.tuple_new, &[buffer, count]);
        let tuple =
            self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), tuple_type.clone());
        self.tag_opaque_runtime_type(&tuple, &tuple_type)?;
        Ok(tuple)
    }

    /// Compiles a constant-index read of a Copy tuple element.
    ///
    /// The checker must reject projection when the selected element is
    /// non-Copy. The runtime returns an independently owned boxed clone, which
    /// is then coerced back to the element's direct representation.
    fn compile_tuple_element(
        &mut self,
        tuple: &Operand,
        index: usize,
        element_type: &Type,
    ) -> std::result::Result<ValueRef, String> {
        validate_tuple_projection_operand(tuple)?;
        let tuple = self.load_operand(tuple)?;
        let tuple = self.ensure_opaque(tuple)?;
        let index = self.builder.ins().iconst(types::I64, index as i64);
        let inst = self
            .builder
            .ins()
            .call(self.tuple_element, &[tuple.values[0], index]);
        let element = self.owned_opaque_result(
            self.builder.inst_results(inst).to_vec(),
            element_type.clone(),
        );
        let direct_type = ensure_direct_type(element_type, &self.classes, "tuple element")?;
        self.coerce_value(element, &direct_type)
    }

    /// Destructively extracts one element from a private captured tuple.
    ///
    /// Lowering must first move the entire user-visible source into a `%t...`
    /// temporary. This operation may then empty selected slots while recursive
    /// destructuring proceeds; no public source projection remains live.
    fn compile_take_tuple_element(
        &mut self,
        place: &str,
        index: usize,
        element_type: &Type,
    ) -> std::result::Result<ValueRef, String> {
        validate_tuple_take_place(place)?;
        let tuple = self.load_place(place)?;
        let tuple = self.ensure_opaque(tuple)?;
        let index = self.builder.ins().iconst(types::I64, index as i64);
        let inst = self
            .builder
            .ins()
            .call(self.tuple_take_element, &[tuple.values[0], index]);
        let element = self.owned_opaque_result(
            self.builder.inst_results(inst).to_vec(),
            element_type.clone(),
        );
        let direct_type = ensure_direct_type(element_type, &self.classes, "tuple element")?;
        self.coerce_value(element, &direct_type)
    }

    fn compile_enum_variant_for_target(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        payloads: &[Operand],
        target: Option<&DirectType>,
    ) -> std::result::Result<ValueRef, String> {
        let (enum_ptr, enum_len) = self.string_constant(enum_name.as_bytes())?;
        let (variant_ptr, variant_len) = self.string_constant(variant_name.as_bytes())?;
        let expected_payload_types = target.and_then(|target_ty| {
            enum_variant_payload_types_for_target(enum_name, variant_name, target_ty, &self.classes)
        });
        let payload_buffer = if payloads.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let count = self.builder.ins().iconst(types::I64, payloads.len() as i64);
            let buffer_inst = self.builder.ins().call(self.arg_buffer_new, &[count]);
            let buffer = self.builder.inst_results(buffer_inst)[0];
            for (index, payload) in payloads.iter().enumerate() {
                let loaded = if let Some(expected_ty) = expected_payload_types
                    .as_ref()
                    .and_then(|types| types.get(index))
                {
                    self.load_operand_for_target(payload, expected_ty)?
                } else {
                    self.load_operand(payload)?
                };
                let loaded = if let Some(expected_payload_types) = expected_payload_types.as_ref() {
                    if let Some(expected_ty) = expected_payload_types.get(index) {
                        self.coerce_value(loaded, expected_ty)?
                    } else {
                        loaded
                    }
                } else {
                    loaded
                };
                let payload = self.ensure_opaque(loaded)?;
                let transferred = self.transfer_owned_opaque_value(&payload);
                let index_value = self.builder.ins().iconst(types::I64, index as i64);
                self.builder.ins().call(
                    self.arg_buffer_store_owned,
                    &[buffer, index_value, transferred],
                );
            }
            buffer
        };
        let payload_count = self.builder.ins().iconst(types::I64, payloads.len() as i64);
        let inst = self.builder.ins().call(
            self.enum_variant,
            &[
                enum_ptr,
                enum_len,
                variant_ptr,
                variant_len,
                payload_buffer,
                payload_count,
            ],
        );
        Ok(self.owned_opaque_result(
            self.builder.inst_results(inst).to_vec(),
            target
                .map(direct_type_to_type)
                .unwrap_or_else(|| Type::named(enum_name)),
        ))
    }

    fn compile_enum_variant_from_values(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        payloads: Vec<ValueRef>,
        target: Type,
    ) -> std::result::Result<ValueRef, String> {
        let (enum_ptr, enum_len) = self.string_constant(enum_name.as_bytes())?;
        let (variant_ptr, variant_len) = self.string_constant(variant_name.as_bytes())?;
        let count = self.builder.ins().iconst(types::I64, payloads.len() as i64);
        let buffer = if payloads.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let buffer_inst = self.builder.ins().call(self.arg_buffer_new, &[count]);
            let buffer = self.builder.inst_results(buffer_inst)[0];
            for (index, payload) in payloads.into_iter().enumerate() {
                let payload = self.ensure_opaque(payload)?;
                let transferred = self.transfer_owned_opaque_value(&payload);
                let index = self.builder.ins().iconst(types::I64, index as i64);
                self.builder
                    .ins()
                    .call(self.arg_buffer_store_owned, &[buffer, index, transferred]);
            }
            buffer
        };
        let inst = self.builder.ins().call(
            self.enum_variant,
            &[enum_ptr, enum_len, variant_ptr, variant_len, buffer, count],
        );
        Ok(self.owned_opaque_result(self.builder.inst_results(inst).to_vec(), target))
    }

    fn variant_matches_value(
        &mut self,
        value: Value,
        enum_name: &str,
        variant_name: &str,
    ) -> std::result::Result<Value, String> {
        let (enum_ptr, enum_len) = self.string_constant(enum_name.as_bytes())?;
        let (variant_ptr, variant_len) = self.string_constant(variant_name.as_bytes())?;
        let inst = self.builder.ins().call(
            self.variant_matches,
            &[value, enum_ptr, enum_len, variant_ptr, variant_len],
        );
        Ok(self.builder.inst_results(inst)[0])
    }

    fn compile_variant_payload(
        &mut self,
        scrutinee: ValueRef,
        index: usize,
    ) -> std::result::Result<ValueRef, String> {
        let scrutinee = self.ensure_opaque(scrutinee)?;
        let index = self.builder.ins().iconst(types::I64, index as i64);
        let inst = self
            .builder
            .ins()
            .call(self.variant_payload, &[scrutinee.values[0], index]);
        let payload = ValueRef {
            values: self.builder.inst_results(inst).to_vec(),
            ty: DirectType::Opaque(Type::named("Unknown")),
        };
        self.mark_temporary_opaque_owned(&payload);
        Ok(payload)
    }

    fn compile_take_variant_payload(
        &mut self,
        place: &str,
        index: usize,
    ) -> std::result::Result<ValueRef, String> {
        let scrutinee = self.load_place(place)?;
        self.compile_take_variant_payload_from_value(scrutinee, index)
    }

    fn compile_take_variant_payload_from_value(
        &mut self,
        scrutinee: ValueRef,
        index: usize,
    ) -> std::result::Result<ValueRef, String> {
        let scrutinee = self.ensure_opaque(scrutinee)?;
        let index = self.builder.ins().iconst(types::I64, index as i64);
        let inst = self
            .builder
            .ins()
            .call(self.variant_take_payload, &[scrutinee.values[0], index]);
        Ok(self.owned_opaque_result(
            self.builder.inst_results(inst).to_vec(),
            Type::named("Unknown"),
        ))
    }

    fn compile_try_assign(
        &mut self,
        target: &str,
        target_ty: DirectType,
        try_value: &Operand,
    ) -> std::result::Result<(), String> {
        let loaded = self.load_operand(try_value)?;
        let (source_error_ty, target_error_ty) = match (&loaded.ty, &self.return_type) {
            (
                DirectType::Opaque(Type::Named(source_name, source_args)),
                Type::Named(target_name, target_args),
            ) if source_name == "Result"
                && source_args.len() == 2
                && target_name == "Result"
                && target_args.len() == 2 =>
            {
                (source_args[1].clone(), target_args[1].clone())
            }
            _ => {
                return Err(format!(
                    "direct backend `try` for `{target}` requires Result types, found operand `{}` and return `{}`",
                    render_direct_type(&loaded.ty),
                    self.return_type
                ))
            }
        };
        let value = self.ensure_opaque(loaded)?;
        let source_is_owned = self.temporary_owns_opaque(&value);
        if source_is_owned {
            // Ownership must be modeled independently in each CFG successor. Keeping
            // this value in the compile-time temporary set while emitting the first
            // successor would make the second successor inherit the first one's
            // release/transfer decision.
            self.clear_temporary_opaque_owned(&value);
        }
        let ok = self.variant_matches_value(value.values[0], "Result", "Ok")?;
        let ok_block = self.builder.create_block();
        let err_block = self.builder.create_block();
        let join_block = self.builder.create_block();
        self.builder.ins().brif(ok, ok_block, &[], err_block, &[]);

        self.builder.switch_to_block(ok_block);
        if source_is_owned {
            self.mark_temporary_opaque_owned(&value);
        }
        let payload = if source_is_owned {
            self.compile_take_variant_payload_from_value(value.clone(), 0)?
        } else {
            self.compile_variant_payload(value.clone(), 0)?
        };
        if source_is_owned {
            self.clear_temporary_opaque_owned(&value);
            self.release_opaque_handle(value.values[0]);
        }
        let coerced = self.coerce_value(payload, &target_ty)?;
        self.store_place(target, coerced)?;
        self.release_all_temporary_owned();
        self.builder.ins().jump(join_block, &[]);
        self.builder.seal_block(ok_block);

        self.builder.switch_to_block(err_block);
        if source_is_owned {
            self.mark_temporary_opaque_owned(&value);
        }
        let error_result = if source_error_ty == target_error_ty {
            value.clone()
        } else {
            let payload = if source_is_owned {
                self.compile_take_variant_payload_from_value(value.clone(), 0)?
            } else {
                self.compile_variant_payload(value.clone(), 0)?
            };
            if source_is_owned {
                self.clear_temporary_opaque_owned(&value);
                self.release_opaque_handle(value.values[0]);
            }
            let converted =
                self.convert_try_error_via_from(payload, &source_error_ty, &target_error_ty)?;
            self.compile_enum_variant_from_values(
                "Result",
                "Err",
                vec![converted],
                self.return_type.clone(),
            )?
        };
        self.emit_return_value(error_result)?;
        self.builder.seal_block(err_block);

        self.builder.switch_to_block(join_block);
        self.builder.seal_block(join_block);
        Ok(())
    }

    fn convert_try_error_via_from(
        &mut self,
        payload: ValueRef,
        source_ty: &Type,
        target_ty: &Type,
    ) -> std::result::Result<ValueRef, String> {
        let method = self
            .trait_impls
            .iter()
            .filter(|trait_impl| {
                trait_impl.trait_name == "From" && trait_impl.trait_args.len() == 1
            })
            .filter_map(|trait_impl| {
                let mut type_params = BTreeSet::new();
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
                ) || crate::sema::substitute_type(&trait_impl.trait_args[0], &substitutions)
                    != *source_ty
                {
                    return None;
                }
                let method = trait_impl
                    .methods
                    .iter()
                    .find(|method| method.name == "from")?;
                Some((
                    crate::sema::trait_impl_specificity_parts(
                        &trait_impl.for_type,
                        &trait_impl.trait_args,
                    ),
                    method.clone(),
                ))
            })
            .max_by_key(|(specificity, _)| *specificity)
            .map(|(_, method)| method);
        let Some(method) = method else {
            return Err(format!(
                "direct backend could not find `From[{source_ty}] for {target_ty}` required by `try`"
            ));
        };
        let function_name = method.function_name;
        let expected = self
            .function_param_types
            .get(&function_name)
            .and_then(|parameters| parameters.first())
            .cloned()
            .unwrap_or(DirectType::Opaque(source_ty.clone()));
        let payload = self.coerce_value(payload, &expected)?;
        let arguments = if matches!(payload.ty, DirectType::Opaque(_)) {
            vec![self.transfer_opaque_arg(&payload)]
        } else {
            payload.values
        };
        let Some(function) = self.function_refs.get(&function_name).copied() else {
            return Err(format!(
                "direct backend does not know From function `{function_name}`"
            ));
        };
        let inst = self.builder.ins().call(function, &arguments);
        let results = self.builder.inst_results(inst).to_vec();
        let (converted, writebacks) = self.split_call_results(&function_name, results)?;
        if !writebacks.is_empty() {
            return Err(
                "direct backend From conversion unexpectedly returned writebacks".to_string(),
            );
        }
        Ok(converted)
    }

    fn mutable_sink_for_static_place(&mut self, place: &str) -> std::result::Result<Value, String> {
        let (root, projection) = place.split_once('.').unwrap_or((place, ""));
        if let Some(index) = self.mutable_param_indices.get(root).copied() {
            let index = self.builder.ins().iconst(types::I64, index as i64);
            let current = self.builder.ins().call(self.current_mutable_sink, &[index]);
            let parent = self.builder.inst_results(current)[0];
            let (projection_ptr, projection_len) = self.string_constant(projection.as_bytes())?;
            let projected = self.builder.ins().call(
                self.mutable_sink_project,
                &[parent, projection_ptr, projection_len],
            );
            return Ok(self.builder.inst_results(projected)[0]);
        }

        let cleanup_place = self
            .cleanup_places
            .iter()
            .filter_map(|cleanup_place| {
                if place == cleanup_place {
                    Some((cleanup_place.clone(), String::new()))
                } else {
                    place
                        .strip_prefix(&format!("{cleanup_place}."))
                        .map(|projection| (cleanup_place.clone(), projection.to_string()))
                }
            })
            .max_by_key(|(cleanup_place, _)| cleanup_place.len());
        let Some((cleanup_place, projection)) = cleanup_place else {
            return Ok(self.builder.ins().iconst(types::I64, 0));
        };
        let registration_variable = self
            .cleanup_registration_vars
            .get(&cleanup_place)
            .copied()
            .ok_or_else(|| {
                format!("direct backend has no cleanup registration for mutable place `{place}`")
            })?;
        let registration_id = self.builder.use_var(registration_variable);
        let root = self.load_static_place(&cleanup_place)?;
        let root = self.ensure_opaque(root)?;
        let root = self.transfer_owned_opaque_value(&root);
        let (projection_ptr, projection_len) = self.string_constant(projection.as_bytes())?;
        let sink = self.builder.ins().call(
            self.mutable_sink_new,
            &[registration_id, root, projection_ptr, projection_len],
        );
        Ok(self.builder.inst_results(sink)[0])
    }

    fn mutable_sink_for_resolved_place(
        &mut self,
        resolved: DirectViewPlace,
    ) -> std::result::Result<Value, String> {
        if resolved.alternatives.is_empty() {
            return Ok(self.builder.ins().iconst(types::I64, 0));
        }
        if resolved.alternatives.len() == 1 && resolved.alternatives[0].conditions.is_empty() {
            return self.mutable_sink_for_static_place(&resolved.alternatives[0].place);
        }

        let sink_slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            8,
            3,
        ));
        let sink_ptr = self.builder.ins().stack_addr(types::I64, sink_slot, 0);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().store(MemFlags::new(), zero, sink_ptr, 0);
        let merge = self.builder.create_block();
        let alternative_count = resolved.alternatives.len();
        for (index, alternative) in resolved.alternatives.into_iter().enumerate() {
            if index + 1 < alternative_count {
                let selected = self.view_alternative_condition(&alternative);
                let create_block = self.builder.create_block();
                let next_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(selected, create_block, &[], next_block, &[]);
                self.builder.switch_to_block(create_block);
                let sink = self.mutable_sink_for_static_place(&alternative.place)?;
                self.builder.ins().store(MemFlags::new(), sink, sink_ptr, 0);
                self.builder.ins().jump(merge, &[]);
                self.builder.seal_block(create_block);
                self.builder.switch_to_block(next_block);
                self.builder.seal_block(next_block);
            } else {
                let sink = self.mutable_sink_for_static_place(&alternative.place)?;
                self.builder.ins().store(MemFlags::new(), sink, sink_ptr, 0);
                self.builder.ins().jump(merge, &[]);
            }
        }
        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), sink_ptr, 0))
    }

    fn mutable_sink_for_place(&mut self, place: &str) -> std::result::Result<Value, String> {
        let resolved = self.resolve_view_place(place)?;
        self.mutable_sink_for_resolved_place(resolved)
    }

    fn install_direct_mutable_sinks(&mut self, sinks: &[Value]) -> std::result::Result<(), String> {
        let count = self.builder.ins().iconst(types::I64, sinks.len() as i64);
        let pointer = if sinks.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let byte_len = u32::try_from(sinks.len().saturating_mul(8))
                .map_err(|_| "direct mutable sink buffer is too large".to_string())?;
            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                byte_len,
                3,
            ));
            let pointer = self.builder.ins().stack_addr(types::I64, slot, 0);
            for (index, sink) in sinks.iter().copied().enumerate() {
                self.builder
                    .ins()
                    .store(MemFlags::new(), sink, pointer, (index as i32) * 8);
            }
            pointer
        };
        self.builder
            .ins()
            .call(self.set_next_mutable_sinks, &[pointer, count]);
        Ok(())
    }

    fn install_indirect_mutable_sinks(
        &mut self,
        public_sinks: &[Value],
        capture_sinks: &[(usize, Value)],
    ) -> std::result::Result<(), String> {
        let store_buffer = |compiler: &mut Self,
                            values: &[Value]|
         -> std::result::Result<Value, String> {
            if values.is_empty() {
                return Ok(compiler.builder.ins().iconst(types::I64, 0));
            }
            let byte_len = u32::try_from(values.len().saturating_mul(8))
                .map_err(|_| "direct mutable sink buffer is too large".to_string())?;
            let slot = compiler.builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                byte_len,
                3,
            ));
            let pointer = compiler.builder.ins().stack_addr(types::I64, slot, 0);
            for (index, value) in values.iter().copied().enumerate() {
                compiler
                    .builder
                    .ins()
                    .store(MemFlags::new(), value, pointer, (index as i32) * 8);
            }
            Ok(pointer)
        };
        let public_ptr = store_buffer(self, public_sinks)?;
        let capture_indices = capture_sinks
            .iter()
            .map(|(index, _)| self.builder.ins().iconst(types::I64, *index as i64))
            .collect::<Vec<_>>();
        let capture_values = capture_sinks
            .iter()
            .map(|(_, sink)| *sink)
            .collect::<Vec<_>>();
        let capture_indices_ptr = store_buffer(self, &capture_indices)?;
        let capture_values_ptr = store_buffer(self, &capture_values)?;
        let public_count = self
            .builder
            .ins()
            .iconst(types::I64, public_sinks.len() as i64);
        let capture_count = self
            .builder
            .ins()
            .iconst(types::I64, capture_sinks.len() as i64);
        self.builder.ins().call(
            self.set_next_indirect_mutable_sinks,
            &[
                public_ptr,
                public_count,
                capture_indices_ptr,
                capture_values_ptr,
                capture_count,
            ],
        );
        Ok(())
    }

    fn release_mutable_sinks(&mut self, sinks: impl IntoIterator<Item = Value>) {
        for sink in sinks {
            self.builder.ins().call(self.mutable_sink_release, &[sink]);
        }
    }

    fn publish_mutable_root_write_through(
        &mut self,
        root: &str,
    ) -> std::result::Result<(), String> {
        let Some(index) = self.mutable_param_indices.get(root).copied() else {
            return Ok(());
        };
        let index = self.builder.ins().iconst(types::I64, index as i64);
        let sink = self.builder.ins().call(self.current_mutable_sink, &[index]);
        let sink = self.builder.inst_results(sink)[0];
        let zero = self.builder.ins().iconst(types::I64, 0);
        let active = self.builder.ins().icmp(IntCC::NotEqual, sink, zero);
        let publish = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.ins().brif(active, publish, &[], done, &[]);
        self.builder.switch_to_block(publish);
        let current = self.load_root(root)?;
        let current = self.ensure_opaque(current)?;
        let current = self.transfer_owned_opaque_value(&current);
        self.builder
            .ins()
            .call(self.mutable_sink_store_owned, &[sink, current]);
        self.builder.ins().jump(done, &[]);
        self.builder.seal_block(publish);
        self.builder.switch_to_block(done);
        self.builder.seal_block(done);
        Ok(())
    }

    fn set_cleanup_active(&mut self, place: &str, active: bool) -> std::result::Result<(), String> {
        let Some(variable) = self.cleanup_active_vars.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup place `{}`",
                place
            ));
        };
        let value = self
            .builder
            .ins()
            .iconst(types::I64, if active { 1 } else { 0 });
        self.builder.def_var(variable, value);
        Ok(())
    }

    fn register_cleanup_for_place(&mut self, place: &str) -> std::result::Result<(), String> {
        let Some(registration_variable) = self.cleanup_registration_vars.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup registration for `{}`",
                place
            ));
        };
        let Some(thunk_ref) = self.cleanup_thunk_refs.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup thunk for `{}`",
                place
            ));
        };

        let loaded = self.load_place(place)?;
        let boxed = self.ensure_opaque(loaded)?;
        let count = self.builder.ins().iconst(types::I64, 1);
        let buffer = self.builder.ins().call(self.arg_buffer_new, &[count]);
        let buffer = self.builder.inst_results(buffer)[0];
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .call(self.arg_buffer_store, &[buffer, zero, boxed.values[0]]);
        if self.temporary_owns_opaque(&boxed) {
            self.clear_temporary_opaque_owned(&boxed);
            self.release_opaque_handle(boxed.values[0]);
        }
        let thunk_ptr = self.builder.ins().func_addr(types::I64, thunk_ref);
        let registration = self
            .builder
            .ins()
            .call(self.register_cleanup, &[thunk_ptr, buffer, count]);
        let registration_id = self.builder.inst_results(registration)[0];
        self.builder.def_var(registration_variable, registration_id);
        Ok(())
    }

    fn unregister_cleanup_for_place(&mut self, place: &str) -> std::result::Result<(), String> {
        let Some(variable) = self.cleanup_registration_vars.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup registration for `{}`",
                place
            ));
        };
        let registration_id = self.builder.use_var(variable);
        self.builder
            .ins()
            .call(self.unregister_cleanup, &[registration_id]);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(variable, zero);
        Ok(())
    }

    fn refresh_cleanup_registration_for_place(
        &mut self,
        place: &str,
    ) -> std::result::Result<(), String> {
        let Some(active_variable) = self.cleanup_active_vars.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup place `{}`",
                place
            ));
        };
        let Some(registration_variable) = self.cleanup_registration_vars.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup registration for `{}`",
                place
            ));
        };
        let Some(thunk_ref) = self.cleanup_thunk_refs.get(place).copied() else {
            return Err(format!(
                "direct backend does not know cleanup thunk for `{}`",
                place
            ));
        };

        let loaded = self.load_place(place)?;
        let boxed = self.ensure_opaque(loaded)?;
        let count = self.builder.ins().iconst(types::I64, 1);
        let buffer = self.builder.ins().call(self.arg_buffer_new, &[count]);
        let buffer = self.builder.inst_results(buffer)[0];
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .call(self.arg_buffer_store, &[buffer, zero, boxed.values[0]]);
        if self.temporary_owns_opaque(&boxed) {
            self.clear_temporary_opaque_owned(&boxed);
            self.release_opaque_handle(boxed.values[0]);
        }
        let active = self.builder.use_var(active_variable);
        let registration_id = self.builder.use_var(registration_variable);
        let thunk_ptr = self.builder.ins().func_addr(types::I64, thunk_ref);
        let refreshed = self.builder.ins().call(
            self.refresh_cleanup,
            &[active, registration_id, thunk_ptr, buffer, count],
        );
        let refreshed_id = self.builder.inst_results(refreshed)[0];
        self.builder.def_var(registration_variable, refreshed_id);
        Ok(())
    }

    fn refresh_cleanup_registrations_for_mutation(
        &mut self,
        mutated_place: &str,
    ) -> std::result::Result<(), String> {
        let cleanup_places = self
            .cleanup_places
            .clone()
            .into_iter()
            .filter(|place| direct_place_paths_overlap(place, mutated_place))
            .collect::<Vec<_>>();
        for place in cleanup_places {
            self.refresh_cleanup_registration_for_place(&place)?;
        }
        Ok(())
    }

    fn emit_pending_cleanups(
        &mut self,
        cancel_before_cleanup: bool,
    ) -> std::result::Result<(), String> {
        // Cleanup branches are generated off the caller's current control-flow
        // path. Temporaries that were already owned by that caller remain live
        // on its continuation and must not disappear from compile-time tracking
        // merely because the cleanup branch releases its own results. Track only
        // cleanup-created temporaries while emitting each run block, then restore
        // the caller's owners at the merge.
        let caller_owned_temporaries = std::mem::take(&mut self.owned_opaque_temporaries);
        for place in self.cleanup_places.clone().into_iter().rev() {
            let Some(variable) = self.cleanup_active_vars.get(&place).copied() else {
                continue;
            };
            let active = self.builder.use_var(variable);
            let zero = self.builder.ins().iconst(types::I64, 0);
            let should_run = self.builder.ins().icmp(IntCC::NotEqual, active, zero);
            let run_block = self.builder.create_block();
            let next_block = self.builder.create_block();
            self.builder
                .ins()
                .brif(should_run, run_block, &[], next_block, &[]);
            self.builder.switch_to_block(run_block);
            self.unregister_cleanup_for_place(&place)?;
            self.builder.def_var(variable, zero);
            self.emit_cleanup_for_place(&place, cancel_before_cleanup)?;
            self.release_all_temporary_owned();
            self.builder.ins().jump(next_block, &[]);
            self.builder.seal_block(run_block);
            self.builder.switch_to_block(next_block);
            self.builder.seal_block(next_block);
            self.owned_opaque_temporaries.clear();
        }
        self.owned_opaque_temporaries = caller_owned_temporaries;
        Ok(())
    }

    fn append_writeback_return_values(
        &mut self,
        values: &mut Vec<Value>,
    ) -> std::result::Result<(), String> {
        for (name, ty) in self.writeback_locals.clone() {
            let current = self.load_root(&name)?;
            let coerced = self.coerce_value(current, &ty)?;
            values.extend(self.export_return_value(coerced));
        }
        Ok(())
    }

    fn split_call_results(
        &mut self,
        function_name: &str,
        results: Vec<Value>,
    ) -> std::result::Result<(ValueRef, Vec<ValueRef>), String> {
        let result_ty = self.call_result_type(function_name)?;
        let result_count = result_ty.value_count();
        if results.len() < result_count {
            return Err(format!(
                "direct backend received too few call results for `{}`",
                function_name
            ));
        }
        let mut cursor = result_count;
        let mut writebacks = Vec::new();
        for ty in self
            .function_writeback_types
            .get(function_name)
            .cloned()
            .unwrap_or_default()
        {
            let count = ty.value_count();
            if results.len() < cursor + count {
                return Err(format!(
                    "direct backend received incomplete writeback results for `{}`",
                    function_name
                ));
            }
            writebacks.push(ValueRef {
                values: results[cursor..cursor + count].to_vec(),
                ty,
            });
            if matches!(
                writebacks.last().map(|value| &value.ty),
                Some(DirectType::Opaque(_))
            ) {
                let value = writebacks.last().expect("just pushed writeback");
                self.mark_temporary_opaque_owned(value);
            }
            cursor += count;
        }
        let result = ValueRef {
            values: results[..result_count].to_vec(),
            ty: result_ty,
        };
        if matches!(result.ty, DirectType::Opaque(_)) {
            self.mark_temporary_opaque_owned(&result);
        }
        Ok((result, writebacks))
    }

    fn apply_writeback_places(
        &mut self,
        places: &[String],
        values: Vec<ValueRef>,
    ) -> std::result::Result<(), String> {
        if places.len() != values.len() {
            return Err(format!(
                "direct backend expected {} writeback values but received {}",
                places.len(),
                values.len()
            ));
        }
        for (place, value) in places.iter().zip(values) {
            self.store_place(place, value)?;
        }
        Ok(())
    }

    fn emit_cleanup_for_place(
        &mut self,
        place: &str,
        cancel_before_cleanup: bool,
    ) -> std::result::Result<(), String> {
        let receiver_ty = self.type_of_place(place)?;
        match &receiver_ty {
            DirectType::PlainClass(class_ty) => {
                let has_close = self
                    .classes
                    .get(&class_ty.class_name)
                    .and_then(|class| class.methods.iter().find(|method| method.name == "close"))
                    .is_some();
                if has_close {
                    let operand = Operand::Place(place.to_string());
                    let _ = self.compile_member_call(&operand, "close", Some(place), &[])?;
                }
            }
            DirectType::Opaque(ty) => {
                let operand = Operand::Place(place.to_string());
                let loaded = self.load_operand(&operand)?;
                if matches!(ty, Type::Named(name, _) if name == "TaskGroup") {
                    let loaded = self.ensure_opaque(loaded)?;
                    let cancel_before = self
                        .builder
                        .ins()
                        .iconst(types::I64, if cancel_before_cleanup { 1 } else { 0 });
                    let result = self
                        .builder
                        .ins()
                        .call(self.task_group_close, &[loaded.values[0], cancel_before]);
                    self.release_opaque_handle(self.builder.inst_results(result)[0]);
                    return Ok(());
                }
                if self
                    .compile_opaque_member_call(ty, loaded, "close", Some(place), &[], None)
                    .is_ok()
                {
                    return Ok(());
                }
            }
            DirectType::Scalar(_) => {}
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compile_class_member_call(
        &mut self,
        class_name: &str,
        receiver_type_hint: Option<Type>,
        object: ValueRef,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
        trait_name: Option<&str>,
    ) -> std::result::Result<ValueRef, String> {
        let mut method = trait_name.and_then(|trait_name| {
            receiver_type_hint
                .as_ref()
                .and_then(|ty| self.find_trait_method_for_trait(ty, trait_name, field))
                .or_else(|| {
                    self.find_trait_method_for_trait(&Type::named(class_name), trait_name, field)
                })
                .cloned()
        });
        if trait_name.is_none() {
            method = find_method(self.classes.get(class_name), field).cloned();
            if method.is_none() {
                method = receiver_type_hint
                    .as_ref()
                    .and_then(|ty| self.find_trait_method(ty, field).cloned());
            }
            if method.is_none() {
                method = self
                    .find_trait_method(&Type::named(class_name), field)
                    .cloned();
            }
        }
        let Some(method) = method else {
            return Err(format!(
                "direct backend does not know method `{}.{}`",
                class_name, field
            ));
        };
        let method_function_name = method.function_name.clone();
        if method.receiver == Some(MirReceiverKind::BorrowMut) && receiver_place.is_none() {
            return Err(format!(
                "direct backend does not yet support temporary mutable receiver method `{}.{}`",
                class_name, field
            ));
        }
        let func_ref = *self.function_refs.get(&method_function_name).ok_or({
            format!(
                "direct backend does not know function `{}`",
                method_function_name
            )
        })?;
        let expected = self
            .function_param_types
            .get(&method_function_name)
            .cloned()
            .unwrap_or_default();
        let mut lowered_args = Vec::new();
        let mut writeback_places = Vec::new();
        let mut mutable_sink_places = vec![None; expected.len()];
        let receiver_expected = expected.first().cloned().unwrap_or(object.ty.clone());
        let receiver = self.coerce_value(object.clone(), &receiver_expected)?;
        if matches!(receiver.ty, DirectType::Opaque(_)) {
            lowered_args.push(self.transfer_opaque_arg(&receiver));
        } else {
            lowered_args.extend(receiver.values);
        }
        if method.receiver == Some(MirReceiverKind::BorrowMut) {
            let Some(place) = receiver_place else {
                return Err(format!(
                    "direct backend does not yet support temporary mutable receiver method `{}.{}`",
                    class_name, field
                ));
            };
            writeback_places.push(place.to_string());
            if let Some(slot) = mutable_sink_places.first_mut() {
                *slot = Some(place.to_string());
            }
        }
        for (index, argument) in args.iter().enumerate() {
            let loaded = if let Some(expected_ty) = expected.get(index + 1) {
                self.load_operand_for_target(&argument.value, expected_ty)?
            } else {
                self.load_operand(&argument.value)?
            };
            let coerced = if let Some(expected_ty) = expected.get(index + 1) {
                self.coerce_value(loaded, expected_ty)?
            } else {
                loaded
            };
            if let Some(place) = &argument.writeback_place {
                writeback_places.push(place.clone());
                if let Some(slot) = mutable_sink_places.get_mut(index + 1) {
                    *slot = Some(place.clone());
                }
            }
            if matches!(coerced.ty, DirectType::Opaque(_)) {
                lowered_args.push(self.transfer_opaque_arg(&coerced));
            } else {
                lowered_args.extend(coerced.values);
            }
        }
        let mutable_sinks = if mutable_sink_places.iter().any(Option::is_some) {
            let mut sinks = Vec::with_capacity(mutable_sink_places.len());
            for place in &mutable_sink_places {
                sinks.push(match place {
                    Some(place) => self.mutable_sink_for_place(place)?,
                    None => self.builder.ins().iconst(types::I64, 0),
                });
            }
            self.install_direct_mutable_sinks(&sinks)?;
            sinks
        } else {
            Vec::new()
        };
        let inst = self.builder.ins().call(func_ref, &lowered_args);
        let results = self.builder.inst_results(inst).to_vec();
        self.release_mutable_sinks(mutable_sinks);
        let (result, writebacks) = self.split_call_results(&method_function_name, results)?;
        self.apply_writeback_places(&writeback_places, writebacks)?;
        Ok(result)
    }

    fn compile_opaque_member_call(
        &mut self,
        object_ty: &Type,
        object: ValueRef,
        field: &str,
        receiver_place: Option<&str>,
        args: &[MirArg],
        trait_name: Option<&str>,
    ) -> std::result::Result<ValueRef, String> {
        if let Type::Named(name, _) = object_ty {
            let has_declared_class_method = find_method(self.classes.get(name), field).is_some();
            let has_noncolliding_trait_method = BuiltinMember::resolve(name, field).is_none()
                && self.find_trait_method(object_ty, field).is_some();
            if has_declared_class_method || has_noncolliding_trait_method {
                return self.compile_class_member_call(
                    name,
                    Some(object_ty.clone()),
                    object,
                    field,
                    receiver_place,
                    args,
                    None,
                );
            }
        }
        if is_fixed_width_integer_type(object_ty)
            && matches!(
                field,
                "wrapping_add"
                    | "wrapping_sub"
                    | "wrapping_mul"
                    | "saturating_add"
                    | "saturating_sub"
                    | "saturating_mul"
                    | "wrapping_shl"
                    | "wrapping_shr"
                    | "saturating_shl"
                    | "saturating_shr"
            )
        {
            let argument_name = if field.ends_with("shl") || field.ends_with("shr") {
                "count"
            } else {
                "rhs"
            };
            let ordered = ordered_named_args(&[argument_name], args)?;
            let argument = ordered[0];
            let target = ensure_direct_type(object_ty, &self.classes, "integer receiver")?;
            let left = self.ensure_opaque(object)?;
            let right = self.load_operand_as_opaque_direct(&argument.value, &target)?;
            let operation = match field {
                "wrapping_add" | "saturating_add" => 0,
                "wrapping_sub" | "saturating_sub" => 1,
                "wrapping_shl" | "saturating_shl" => 3,
                "wrapping_shr" | "saturating_shr" => 4,
                _ => {
                    debug_assert!(matches!(field, "wrapping_mul" | "saturating_mul"));
                    2
                }
            };
            let arithmetic_mode = if field.starts_with("wrapping_") { 1 } else { 2 };
            let operation = self.builder.ins().iconst(types::I64, operation);
            let arithmetic_mode = self.builder.ins().iconst(types::I64, arithmetic_mode);
            let zero = self.builder.ins().iconst(types::I64, 0);
            let inst = self.builder.ins().call(
                self.integer_width_binary,
                &[
                    left.values[0],
                    right.values[0],
                    operation,
                    arithmetic_mode,
                    zero,
                    zero,
                ],
            );
            let result = self
                .owned_opaque_result(self.builder.inst_results(inst).to_vec(), object_ty.clone());
            return self.coerce_value(result, &target);
        }
        if field == "to_float" && is_fixed_width_integer_type(object_ty) {
            if !args.is_empty() {
                return Err(DIRECT_TO_FLOAT_ARITY_ERROR.to_string());
            }
            let object = self.ensure_opaque(object)?;
            let inst = self
                .builder
                .ins()
                .call(self.integer_to_float, &[object.values[0]]);
            return Ok(ValueRef {
                values: self.builder.inst_results(inst).to_vec(),
                ty: DirectType::Scalar(ScalarKind::Float64),
            });
        }
        if field == "to_string" {
            if !args.is_empty() {
                return Err(
                    "direct backend expected `to_string()` to take no arguments".to_string()
                );
            }
            let object = self.ensure_opaque(object)?;
            let inst = self
                .builder
                .ins()
                .call(self.stringify_value, &[object.values[0]]);
            return Ok(self.owned_opaque_result(
                self.builder.inst_results(inst).to_vec(),
                Type::named("str"),
            ));
        }
        if field == "clone"
            || (field == "copy"
                && matches!(
                    object_ty,
                    Type::Named(name, _) if matches!(name.as_str(), "list" | "dict" | "set")
                ))
        {
            if !args.is_empty() {
                return Err(format!(
                    "direct backend expected `{field}()` to take no arguments"
                ));
            }
            let object = self.ensure_opaque(object)?;
            if matches!(object_ty, Type::Named(name, arguments) if name == "Array" && arguments.len() == 1)
            {
                let zero = self.builder.ins().iconst(types::I64, 0);
                let inst = self
                    .builder
                    .ins()
                    .call(self.array_clone, &[object.values[0], zero, zero]);
                return Ok(self.owned_opaque_result(
                    self.builder.inst_results(inst).to_vec(),
                    object_ty.clone(),
                ));
            }
            let inst = self
                .builder
                .ins()
                .call(self.clone_value, &[object.values[0]]);
            return Ok(self
                .owned_opaque_result(self.builder.inst_results(inst).to_vec(), object_ty.clone()));
        }
        if let Type::Named(name, class_args) = object_ty {
            if name == "str" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "__slice" => {
                        let [start_arg, has_start_arg, end_arg, has_end_arg, line_arg, column_arg] =
                            direct_internal_slice_args(args)?;
                        let start = self.load_operand_with_integer_hint(
                            &start_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let start =
                            self.coerce_value(start, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_start = self.load_operand(&has_start_arg.value)?;
                        let has_start =
                            self.coerce_value(has_start, &DirectType::Scalar(ScalarKind::Bool))?;
                        let end = self.load_operand_with_integer_hint(
                            &end_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let end = self.coerce_value(end, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_end = self.load_operand(&has_end_arg.value)?;
                        let has_end =
                            self.coerce_value(has_end, &DirectType::Scalar(ScalarKind::Bool))?;
                        let line = self.load_operand_with_integer_hint(
                            &line_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let line =
                            self.coerce_value(line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let column = self.load_operand_with_integer_hint(
                            &column_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let column =
                            self.coerce_value(column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.string_slice,
                            &[
                                object.values[0],
                                start.values[0],
                                has_start.values[0],
                                end.values[0],
                                has_end.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "len" | "byte_len" => {
                        if !args.is_empty() {
                            return Err(format!(
                                "direct backend expected `{field}()` to take no arguments"
                            ));
                        }
                        let function = match field {
                            "len" => self.string_len,
                            "byte_len" => self.string_byte_len,
                            _ => unreachable!(),
                        };
                        let inst = self.builder.ins().call(function, &[object.values[0]]);
                        let len = self.builder.inst_results(inst)[0];
                        Ok(ValueRef {
                            values: vec![len],
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "contains" | "starts_with" | "ends_with" => {
                        let [argument] = args else {
                            return Err(format!(
                                "direct backend expected `{}`() to receive one string argument",
                                field
                            ));
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let func = match field {
                            "contains" => self.string_contains,
                            "starts_with" => self.string_starts_with,
                            "ends_with" => self.string_ends_with,
                            _ => unreachable!(),
                        };
                        let inst = self
                            .builder
                            .ins()
                            .call(func, &[object.values[0], value.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "split" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `split()` to receive one string argument"
                                    .to_string(),
                            );
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.string_split, &[object.values[0], value.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![Type::named("str")]),
                        ))
                    }
                    "replace" => {
                        let [from_arg, to_arg] = args else {
                            return Err(
                                "direct backend expected `replace()` to receive `from` and `to` string arguments"
                                    .to_string(),
                            );
                        };
                        let loaded_from = self.load_operand(&from_arg.value)?;
                        let from = self.ensure_opaque(loaded_from)?;
                        let loaded_to = self.load_operand(&to_arg.value)?;
                        let to = self.ensure_opaque(loaded_to)?;
                        let inst = self.builder.ins().call(
                            self.string_replace,
                            &[object.values[0], from.values[0], to.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "add" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `add()` to receive one string argument"
                                    .to_string(),
                            );
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let binary_opcode = Self::binary_opcode(BinaryOp::Add);
                        let opcode = self.builder.ins().iconst(types::I64, binary_opcode);
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let inst = self.builder.ins().call(
                            self.binary_value,
                            &[opcode, object.values[0], value.values[0], zero, zero, zero],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "to_lower" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `to_lower()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.string_to_lower, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "to_upper" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `to_upper()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.string_to_upper, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "join" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `join()` to receive one list argument"
                                    .to_string(),
                            );
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.string_join, &[object.values[0], value.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "strip_prefix" | "strip_suffix" => {
                        let [argument] = args else {
                            return Err(format!(
                                "direct backend expected `{}`() to receive one string argument",
                                field
                            ));
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let func = match field {
                            "strip_prefix" => self.string_strip_prefix,
                            "strip_suffix" => self.string_strip_suffix,
                            _ => unreachable!(),
                        };
                        let inst = self
                            .builder
                            .ins()
                            .call(func, &[object.values[0], value.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                        ))
                    }
                    "trim" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `trim()` to take no arguments".to_string()
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.string_trim, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "random.Rng" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "next_int" => {
                        let ordered = ordered_named_args(&["lo", "hi"], args)?;
                        let lo_arg = ordered[0];
                        let hi_arg = ordered[1];
                        let lo = self.load_operand_with_integer_hint(
                            &lo_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let lo = self.coerce_value(lo, &DirectType::Scalar(ScalarKind::Int64))?;
                        let hi = self.load_operand_with_integer_hint(
                            &hi_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let hi = self.coerce_value(hi, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self.builder.ins().call(
                            self.rng_next_int,
                            &[object.values[0], lo.values[0], hi.values[0]],
                        );
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "next_float" => {
                        ordered_named_args(&[], args)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.rng_next_float, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Float64),
                        })
                    }
                    "shuffle" => {
                        let ordered = ordered_named_args(&["values"], args)?;
                        let argument = ordered[0];
                        let vector = self.load_operand(&argument.value)?;
                        let vector = self.ensure_opaque(vector)?;
                        let _ = self
                            .builder
                            .ins()
                            .call(self.rng_shuffle, &[object.values[0], vector.values[0]]);
                        let Some(place) = &argument.writeback_place else {
                            return Err(
                                "direct backend expected `shuffle()` to carry a mutable argument writeback place"
                                    .to_string(),
                            );
                        };
                        self.store_place(place, vector)?;
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "Array" {
                let object = self.ensure_opaque(object)?;
                let element_ty = class_args
                    .first()
                    .cloned()
                    .ok_or("direct backend expected Array to carry one dtype".to_string())?;
                let element_direct_ty =
                    ensure_direct_type(&element_ty, &self.classes, "Array dtype")?;
                let array_ty = Type::Named("Array".to_string(), vec![element_ty.clone()]);
                let zero = self.builder.ins().iconst(types::I64, 0);
                return match field {
                    "__slice" => {
                        let [start_arg, has_start_arg, end_arg, has_end_arg, line_arg, column_arg] =
                            direct_internal_slice_args(args)?;
                        let integer_hint = Some(ScalarKind::Int64);
                        let start =
                            self.load_operand_with_integer_hint(&start_arg.value, integer_hint)?;
                        let start =
                            self.coerce_value(start, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_start = self.load_operand(&has_start_arg.value)?;
                        let has_start =
                            self.coerce_value(has_start, &DirectType::Scalar(ScalarKind::Bool))?;
                        let end =
                            self.load_operand_with_integer_hint(&end_arg.value, integer_hint)?;
                        let end = self.coerce_value(end, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_end = self.load_operand(&has_end_arg.value)?;
                        let has_end =
                            self.coerce_value(has_end, &DirectType::Scalar(ScalarKind::Bool))?;
                        let line = self.load_operand_with_integer_hint(
                            &line_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let line =
                            self.coerce_value(line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let column = self.load_operand_with_integer_hint(
                            &column_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let column =
                            self.coerce_value(column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.array_slice,
                            &[
                                object.values[0],
                                start.values[0],
                                has_start.values[0],
                                end.values[0],
                                has_end.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            array_ty,
                        ))
                    }
                    "shape" => {
                        ordered_named_args(&[], args)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.array_shape, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![Type::named("int64")]),
                        ))
                    }
                    "len" => {
                        ordered_named_args(&[], args)?;
                        let inst = self.builder.ins().call(self.array_len, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "get" => {
                        let ordered = ordered_named_args(&["index"], args)?;
                        let coordinates = self.load_operand(&ordered[0].value)?;
                        let coordinates = self.ensure_opaque(coordinates)?;
                        let inst = self.builder.ins().call(
                            self.array_get,
                            &[object.values[0], coordinates.values[0], zero, zero],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty]),
                        ))
                    }
                    "set" => {
                        let ordered = ordered_named_args(&["index", "value"], args)?;
                        let coordinates = self.load_operand(&ordered[0].value)?;
                        let coordinates = self.ensure_opaque(coordinates)?;
                        let value = self
                            .load_operand_as_opaque_direct(&ordered[1].value, &element_direct_ty)?;
                        let inst = self.builder.ins().call(
                            self.array_set_in_place,
                            &[
                                object.values[0],
                                coordinates.values[0],
                                value.values[0],
                                zero,
                                zero,
                            ],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty]),
                        ))
                    }
                    "fill" => {
                        let ordered = ordered_named_args(&["value"], args)?;
                        let value = self
                            .load_operand_as_opaque_direct(&ordered[0].value, &element_direct_ty)?;
                        let inst = self.builder.ins().call(
                            self.array_fill_in_place,
                            &[object.values[0], value.values[0], zero, zero],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "__index" => {
                        let [coordinate_arg, line_arg, column_arg] = <&[MirArg; 3]>::try_from(args)
                            .expect("MIR lowering emits three arguments for Array indexing");
                        let coordinates = self.load_operand(&coordinate_arg.value)?;
                        let coordinates = self.ensure_opaque(coordinates)?;
                        let integer_hint = Some(ScalarKind::Int32);
                        let line =
                            self.load_operand_with_integer_hint(&line_arg.value, integer_hint)?;
                        let line =
                            self.coerce_value(line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let column =
                            self.load_operand_with_integer_hint(&column_arg.value, integer_hint)?;
                        let column =
                            self.coerce_value(column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.array_index,
                            &[
                                object.values[0],
                                coordinates.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        let result = self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            element_ty,
                        );
                        self.coerce_value(result, &element_direct_ty)
                    }
                    "__set_index" => {
                        let [coordinate_arg, value_arg, line_arg, column_arg] =
                            <&[MirArg; 4]>::try_from(args).expect(
                                "MIR lowering emits four arguments for Array indexed assignment",
                            );
                        let coordinates = self.load_operand(&coordinate_arg.value)?;
                        let coordinates = self.ensure_opaque(coordinates)?;
                        let value = self
                            .load_operand_as_opaque_direct(&value_arg.value, &element_direct_ty)?;
                        let integer_hint = Some(ScalarKind::Int32);
                        let line =
                            self.load_operand_with_integer_hint(&line_arg.value, integer_hint)?;
                        let line =
                            self.coerce_value(line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let column =
                            self.load_operand_with_integer_hint(&column_arg.value, integer_hint)?;
                        let column =
                            self.coerce_value(column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.array_set_index_in_place,
                            &[
                                object.values[0],
                                coordinates.values[0],
                                value.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "map" => {
                        let ordered = ordered_named_args(&["f"], args)?;
                        let callback = self.load_operand(&ordered[0].value)?;
                        let callback_type = direct_type_to_type(&callback.ty);
                        let result_type = match callback_type {
                            Type::Function { return_type, .. }
                            | Type::Closure { return_type, .. } => *return_type,
                            other => {
                                return Err(format!(
                                    "direct backend expected `Array.map` callback, found `{other}`"
                                ))
                            }
                        };
                        let result_dtype = self
                            .builder
                            .ins()
                            .iconst(types::I64, direct_array_dtype_code(&result_type)?);
                        let callback = self.ensure_opaque(callback)?;
                        let inst = self.builder.ins().call(
                            self.array_map,
                            &[
                                object.values[0],
                                callback.values[0],
                                result_dtype,
                                zero,
                                zero,
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Array".to_string(), vec![result_type]),
                        ))
                    }
                    "sum" | "min" | "max" | "mean" => {
                        ordered_named_args(&[], args)?;
                        let reduction_code = match field {
                            "sum" => 0,
                            "min" => 1,
                            "max" => 2,
                            _ => {
                                debug_assert_eq!(field, "mean");
                                3
                            }
                        };
                        let reduction = self.builder.ins().iconst(types::I64, reduction_code);
                        let inst = self.builder.ins().call(
                            self.array_reduce,
                            &[object.values[0], reduction, zero, zero],
                        );
                        let target = if field == "mean" {
                            DirectType::Scalar(ScalarKind::Float64)
                        } else {
                            element_direct_ty
                        };
                        let boxed = self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            direct_type_to_type(&target),
                        );
                        self.coerce_value(boxed, &target)
                    }
                    "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "saturating_add"
                    | "saturating_sub" | "saturating_mul" => {
                        let ordered = ordered_named_args(&["rhs"], args)?;
                        let rhs = self.load_operand(&ordered[0].value)?;
                        let rhs = if direct_array_element_type(&rhs.ty).is_some() {
                            rhs
                        } else {
                            self.coerce_value(rhs, &element_direct_ty)?
                        };
                        let rhs = self.ensure_opaque(rhs)?;
                        let operation_code = match field {
                            "wrapping_add" | "saturating_add" => 0,
                            "wrapping_sub" | "saturating_sub" => 1,
                            _ => {
                                debug_assert!(matches!(field, "wrapping_mul" | "saturating_mul"));
                                2
                            }
                        };
                        let arithmetic_mode = if field.starts_with("wrapping_") { 1 } else { 2 };
                        let operation = self.builder.ins().iconst(types::I64, operation_code);
                        let arithmetic_mode =
                            self.builder.ins().iconst(types::I64, arithmetic_mode);
                        let inst = self.builder.ins().call(
                            self.array_binary,
                            &[
                                object.values[0],
                                rhs.values[0],
                                zero,
                                operation,
                                arithmetic_mode,
                                zero,
                                zero,
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            array_ty,
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `Array.{field}`"
                    )),
                };
            }
            if name == "list" {
                let object = self.ensure_opaque(object)?;
                let element_ty = class_args
                    .first()
                    .cloned()
                    .unwrap_or(Type::named("Unknown"));
                let element_direct_ty =
                    ensure_direct_type(&element_ty, &self.classes, "Vec element")?;
                return match field {
                    "__slice" => {
                        let [start_arg, has_start_arg, end_arg, has_end_arg, line_arg, column_arg] =
                            direct_internal_slice_args(args)?;
                        let start = self.load_operand_with_integer_hint(
                            &start_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let start =
                            self.coerce_value(start, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_start = self.load_operand(&has_start_arg.value)?;
                        let has_start =
                            self.coerce_value(has_start, &DirectType::Scalar(ScalarKind::Bool))?;
                        let end = self.load_operand_with_integer_hint(
                            &end_arg.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let end = self.coerce_value(end, &DirectType::Scalar(ScalarKind::Int64))?;
                        let has_end = self.load_operand(&has_end_arg.value)?;
                        let has_end =
                            self.coerce_value(has_end, &DirectType::Scalar(ScalarKind::Bool))?;
                        let line = self.load_operand_with_integer_hint(
                            &line_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let line =
                            self.coerce_value(line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let column = self.load_operand_with_integer_hint(
                            &column_arg.value,
                            Some(ScalarKind::Int32),
                        )?;
                        let column =
                            self.coerce_value(column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.vec_slice,
                            &[
                                object.values[0],
                                start.values[0],
                                has_start.values[0],
                                end.values[0],
                                has_end.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![element_ty]),
                        ))
                    }
                    "len" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `len()` to take no arguments".to_string()
                            );
                        }
                        let inst = self.builder.ins().call(self.vec_len, &[object.values[0]]);
                        let len = self.builder.inst_results(inst)[0];
                        Ok(ValueRef {
                            values: vec![len],
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "is_empty" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `is_empty()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.vec_is_empty, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "append" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `append()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let result = self
                            .builder
                            .ins()
                            .call(self.vec_push_in_place, &[object.values[0], value]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "pop" => {
                        let index = match args {
                            [] => self.builder.ins().iconst(types::I64, -1),
                            [argument] => {
                                let loaded = self.load_operand_with_integer_hint(
                                    &argument.value,
                                    Some(ScalarKind::Int64),
                                )?;
                                self.coerce_value(loaded, &DirectType::Scalar(ScalarKind::Int64))?
                                    .values[0]
                            }
                            _ => {
                                return Err(
                                    "direct backend expected `pop()` to receive at most one index"
                                        .to_string(),
                                )
                            }
                        };
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self.builder.ins().iconst(types::I64, 0);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], zero, index, opcode],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            element_ty,
                        ))
                    }
                    "get" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `get()` to receive one index argument"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.vec_get, &[object.values[0], index.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            class_args
                                .first()
                                .cloned()
                                .unwrap_or(Type::named("Unknown")),
                        ))
                    }
                    "__index_option" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected internal optional vector indexing to receive one argument"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.vec_index_option, &[object.values[0], index.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            element_ty.clone(),
                        ))
                    }
                    "__take_index_option" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected internal consuming vector indexing to receive one argument"
                                    .to_string(),
                            );
                        };
                        let Some(receiver_place) = receiver_place else {
                            return Err(
                                "direct backend expected internal consuming vector indexing to provide its private receiver place"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self.builder.ins().call(
                            self.vec_take_index_in_place,
                            &[object.values[0], index.values[0]],
                        );
                        self.store_place(receiver_place, object.clone())?;
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty]),
                        ))
                    }
                    "__index" => {
                        let [argument, line_arg, column_arg] = args else {
                            return Err(
                                "direct backend expected internal vector indexing to receive index, line, and column"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let loaded_line = self.load_operand(&line_arg.value)?;
                        let line =
                            self.coerce_value(loaded_line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let loaded_column = self.load_operand(&column_arg.value)?;
                        let column = self
                            .coerce_value(loaded_column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let normalized_index = self.emit_vec_index_failure_guard(
                            object.values[0],
                            index.values[0],
                            line.values[0],
                            column.values[0],
                        )?;
                        let inst = self.builder.ins().call(
                            self.vec_index,
                            &[
                                object.values[0],
                                normalized_index,
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        let indexed = self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            element_ty,
                        );
                        self.coerce_value(indexed, &element_direct_ty)
                    }
                    "set" => {
                        let [index_arg, value_arg] = args else {
                            return Err(
                                "direct backend expected `set()` to receive index and value"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&index_arg.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let value = self
                            .load_operand_as_opaque_direct(&value_arg.value, &element_direct_ty)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let inst = self.builder.ins().call(
                            self.vec_set_in_place,
                            &[object.values[0], index.values[0], value],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Option".to_string(),
                                vec![class_args
                                    .first()
                                    .cloned()
                                    .unwrap_or(Type::named("Unknown"))],
                            ),
                        ))
                    }
                    "__set_index" => {
                        let [index_arg, value_arg, line_arg, column_arg] = args else {
                            return Err(
                                "direct backend expected internal indexed assignment to receive index, value, line, and column"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&index_arg.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let value = self
                            .load_operand_as_opaque_direct(&value_arg.value, &element_direct_ty)?;
                        let loaded_line = self.load_operand(&line_arg.value)?;
                        let line =
                            self.coerce_value(loaded_line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let loaded_column = self.load_operand(&column_arg.value)?;
                        let column = self
                            .coerce_value(loaded_column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let normalized_index = self.emit_vec_index_failure_guard(
                            object.values[0],
                            index.values[0],
                            line.values[0],
                            column.values[0],
                        )?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let result = self.builder.ins().call(
                            self.vec_set_index_in_place,
                            &[
                                object.values[0],
                                normalized_index,
                                value,
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "remove" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `remove()` to receive one value argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let opcode = self.builder.ins().iconst(types::I64, 1);
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], value.values[0], zero, opcode],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    "index" | "count" => {
                        let [argument] = args else {
                            return Err(format!(
                                "direct backend expected `{field}()` to receive one value argument"
                            ));
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let opcode = self
                            .builder
                            .ins()
                            .iconst(types::I64, if field == "index" { 2 } else { 3 });
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], value.values[0], zero, opcode],
                        );
                        let boxed = self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("int64"),
                        );
                        self.coerce_value(boxed, &DirectType::Scalar(ScalarKind::Int64))
                    }
                    "swap" => {
                        let [first_arg, second_arg] = args else {
                            return Err(
                                "direct backend expected `swap()` to receive two index arguments"
                                    .to_string(),
                            );
                        };
                        let loaded_first = self.load_operand(&first_arg.value)?;
                        let first = self
                            .coerce_value(loaded_first, &DirectType::Scalar(ScalarKind::Int64))?;
                        let loaded_second = self.load_operand(&second_arg.value)?;
                        let second = self
                            .coerce_value(loaded_second, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self.builder.ins().call(
                            self.vec_swap_in_place,
                            &[object.values[0], first.values[0], second.values[0]],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        let _ = inst;
                        Ok(unit_value(&mut self.builder))
                    }
                    "contains" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `contains()` to receive one value argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.vec_contains, &[object.values[0], value.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "insert" => {
                        let [index_arg, value_arg] = args else {
                            return Err(
                                "direct backend expected `insert()` to receive index and value"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&index_arg.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let value = self
                            .load_operand_as_opaque_direct(&value_arg.value, &element_direct_ty)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let inst = self.builder.ins().call(
                            self.vec_insert_in_place,
                            &[object.values[0], index.values[0], value],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        let _ = inst;
                        Ok(unit_value(&mut self.builder))
                    }
                    "clear" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `clear()` to take no arguments"
                                .to_string());
                        }
                        let result = self
                            .builder
                            .ins()
                            .call(self.vec_clear_in_place, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "reverse" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `reverse()` to take no arguments"
                                .to_string());
                        }
                        let result = self
                            .builder
                            .ins()
                            .call(self.vec_reverse_in_place, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "extend" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `extend()` to receive one list argument"
                                    .to_string(),
                            );
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let result = self
                            .builder
                            .ins()
                            .call(self.vec_extend_in_place, &[object.values[0], value]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "reserve" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `reserve()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let additional = self.load_operand_with_integer_hint(
                            &argument.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let additional =
                            self.coerce_value(additional, &DirectType::Scalar(ScalarKind::Int64))?;
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self.builder.ins().iconst(types::I64, 4);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], zero, additional.values[0], opcode],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "dict" {
                let object = self.ensure_opaque(object)?;
                let key_ty = class_args
                    .first()
                    .cloned()
                    .unwrap_or(Type::named("Unknown"));
                let value_ty = class_args.get(1).cloned().unwrap_or(Type::named("Unknown"));
                let key_direct_ty = ensure_direct_type(&key_ty, &self.classes, "dict key")?;
                let value_direct_ty = ensure_direct_type(&value_ty, &self.classes, "Map value")?;
                return match field {
                    "len" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `len()` to take no arguments".to_string()
                            );
                        }
                        let inst = self.builder.ins().call(self.map_len, &[object.values[0]]);
                        let len = self.builder.inst_results(inst)[0];
                        Ok(ValueRef {
                            values: vec![len],
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "is_empty" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `is_empty()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_is_empty, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "get" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `get()` to receive one key argument"
                                    .to_string(),
                            );
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&argument.value, &key_direct_ty)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_get, &[object.values[0], key.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![value_ty.clone()]),
                        ))
                    }
                    "__index" => {
                        let [key_arg, line_arg, column_arg] = args else {
                            return Err(
                                "direct backend expected internal map indexing to receive key, line, and column"
                                    .to_string(),
                            );
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&key_arg.value, &key_direct_ty)?;
                        let loaded_line = self.load_operand(&line_arg.value)?;
                        let line =
                            self.coerce_value(loaded_line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let loaded_column = self.load_operand(&column_arg.value)?;
                        let column = self
                            .coerce_value(loaded_column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let inst = self.builder.ins().call(
                            self.map_index,
                            &[
                                object.values[0],
                                key.values[0],
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        let indexed = self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            value_ty.clone(),
                        );
                        self.coerce_value(indexed, &value_direct_ty)
                    }
                    "set" => {
                        let [key_arg, value_arg] = args else {
                            return Err("direct backend expected `set()` to receive key and value"
                                .to_string());
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&key_arg.value, &key_direct_ty)?;
                        let value =
                            self.load_operand_as_opaque_direct(&value_arg.value, &value_direct_ty)?;
                        let key = self.transfer_owned_opaque_value(&key);
                        let value = self.transfer_owned_opaque_value(&value);
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_set_in_place, &[object.values[0], key, value]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![value_ty.clone()]),
                        ))
                    }
                    "__set_index" => {
                        let [key_arg, value_arg, line_arg, column_arg] = args else {
                            return Err(
                                "direct backend expected internal map indexed assignment to receive key, value, line, and column"
                                    .to_string(),
                            );
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&key_arg.value, &key_direct_ty)?;
                        let value =
                            self.load_operand_as_opaque_direct(&value_arg.value, &value_direct_ty)?;
                        let loaded_line = self.load_operand(&line_arg.value)?;
                        let line =
                            self.coerce_value(loaded_line, &DirectType::Scalar(ScalarKind::Int32))?;
                        let loaded_column = self.load_operand(&column_arg.value)?;
                        let column = self
                            .coerce_value(loaded_column, &DirectType::Scalar(ScalarKind::Int32))?;
                        let key = self.transfer_owned_opaque_value(&key);
                        let value = self.transfer_owned_opaque_value(&value);
                        let result = self.builder.ins().call(
                            self.map_set_index_in_place,
                            &[
                                object.values[0],
                                key,
                                value,
                                line.values[0],
                                column.values[0],
                            ],
                        );
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "remove" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `remove()` to receive one key argument"
                                    .to_string(),
                            );
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&argument.value, &key_direct_ty)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_remove_in_place, &[object.values[0], key.values[0]]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![value_ty.clone()]),
                        ))
                    }
                    "contains_key" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `contains_key()` to receive one key argument"
                                    .to_string(),
                            );
                        };
                        let key =
                            self.load_operand_as_opaque_direct(&argument.value, &key_direct_ty)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_contains_key, &[object.values[0], key.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "keys" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `keys()` to take no arguments".to_string()
                            );
                        }
                        let inst = self.builder.ins().call(self.map_keys, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![key_ty.clone()]),
                        ))
                    }
                    "values" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `values()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.map_values, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![value_ty.clone()]),
                        ))
                    }
                    "items" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `items()` to take no arguments"
                                .to_string());
                        }
                        let inst = self.builder.ins().call(self.map_items, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "list".to_string(),
                                vec![Type::Tuple(vec![key_ty.clone(), value_ty.clone()])],
                            ),
                        ))
                    }
                    "clear" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `clear()` to take no arguments"
                                .to_string());
                        }
                        let result = self
                            .builder
                            .ins()
                            .call(self.map_clear_in_place, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "update" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `update()` to receive one dict argument"
                                    .to_string(),
                            );
                        };
                        let loaded_value = self.load_operand(&argument.value)?;
                        let value = self.ensure_opaque(loaded_value)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let result = self
                            .builder
                            .ins()
                            .call(self.map_extend_in_place, &[object.values[0], value]);
                        self.release_opaque_handle(self.builder.inst_results(result)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "reserve" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `reserve()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let additional = self.load_operand_with_integer_hint(
                            &argument.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let additional =
                            self.coerce_value(additional, &DirectType::Scalar(ScalarKind::Int64))?;
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self.builder.ins().iconst(types::I64, 4);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], zero, additional.values[0], opcode],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "set" {
                let object = self.ensure_opaque(object)?;
                let element_ty = class_args
                    .first()
                    .cloned()
                    .unwrap_or(Type::named("Unknown"));
                let element_direct_ty =
                    ensure_direct_type(&element_ty, &self.classes, "set element")?;
                return match field {
                    "len" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `len()` to take no arguments".to_string()
                            );
                        }
                        let inst = self.builder.ins().call(self.set_len, &[object.values[0]]);
                        let len = self.builder.inst_results(inst)[0];
                        Ok(ValueRef {
                            values: vec![len],
                            ty: DirectType::Scalar(ScalarKind::Int64),
                        })
                    }
                    "is_empty" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `is_empty()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.set_is_empty, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "contains" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `contains()` to receive one value argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.set_contains, &[object.values[0], value.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "add" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `add()` to receive one value argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        self.builder
                            .ins()
                            .call(self.set_insert_in_place, &[object.values[0], value]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "remove" | "discard" => {
                        let [argument] = args else {
                            return Err(format!(
                                "direct backend expected `{field}()` to receive one value argument"
                            ));
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self
                            .builder
                            .ins()
                            .iconst(types::I64, if field == "remove" { 5 } else { 6 });
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], value.values[0], zero, opcode],
                        );
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    "clear" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `clear()` to take no arguments"
                                .to_string());
                        }
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self.builder.ins().iconst(types::I64, 7);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], zero, zero, opcode],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "reserve" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `reserve()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let additional = self.load_operand_with_integer_hint(
                            &argument.value,
                            Some(ScalarKind::Int64),
                        )?;
                        let additional =
                            self.coerce_value(additional, &DirectType::Scalar(ScalarKind::Int64))?;
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let opcode = self.builder.ins().iconst(types::I64, 4);
                        let inst = self.builder.ins().call(
                            self.collection_operation,
                            &[object.values[0], zero, additional.values[0], opcode],
                        );
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        if let Some(place) = receiver_place {
                            self.store_place(place, object.clone())?;
                        }
                        Ok(unit_value(&mut self.builder))
                    }
                    "__index_option" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected internal optional set indexing to receive one argument"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.set_index_option, &[object.values[0], index.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty]),
                        ))
                    }
                    "__take_index_option" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected internal consuming set indexing to receive one argument"
                                    .to_string(),
                            );
                        };
                        let Some(receiver_place) = receiver_place else {
                            return Err(
                                "direct backend expected internal consuming set indexing to provide its private receiver place"
                                    .to_string(),
                            );
                        };
                        let loaded_index = self.load_operand(&argument.value)?;
                        let index = self
                            .coerce_value(loaded_index, &DirectType::Scalar(ScalarKind::Int64))?;
                        let inst = self.builder.ins().call(
                            self.set_take_index_in_place,
                            &[object.values[0], index.values[0]],
                        );
                        self.store_place(receiver_place, object.clone())?;
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty]),
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "fs.File" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "read_all" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `read_all()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_read_all, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_bytes" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `read_bytes()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_read_bytes, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_all" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `write_all()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let loaded = self.load_operand(&argument.value)?;
                        let text = self.ensure_opaque(loaded)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_write_all, &[object.values[0], text.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "write_bytes" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `write_bytes()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        let loaded = self.load_operand(&argument.value)?;
                        let bytes = self.ensure_opaque(loaded)?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_write_bytes, &[object.values[0], bytes.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "flush" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `flush()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_flush, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.file_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "process.Child" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "stdin" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `stdin()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_stdin, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("process.Pipe".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "stdout" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `stdout()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_stdout, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("process.Pipe".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "stderr" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `stderr()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_stderr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("process.Pipe".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "wait" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_wait, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("process.Wait".to_string(), Vec::new()),
                        ))
                    }
                    "wait_or_none" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self.builder.ins().call(
                            self.process_child_wait_or_none,
                            &[object.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "process.ExitStatus".to_string(),
                                            Vec::new(),
                                        )],
                                    ),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "wait_ok" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_wait_ok, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("process.ExitStatus".to_string(), Vec::new()),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "kill" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `kill()` to take no arguments".to_string()
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_kill, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "terminate" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `terminate()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_terminate, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_child_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "process.Pipe" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "read_all" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `read_all()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_pipe_read_all, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_line" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_pipe_read_line, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_bytes" => {
                        let bound = ordered_optional_named_args(&["max_bytes", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `read_bytes()` to receive `max_bytes`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.process_pipe_read_bytes,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "list".to_string(),
                                            vec![Type::named("uint8")],
                                        )],
                                    ),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_all" => {
                        let bound = ordered_optional_named_args(&["text", "timeout"], args)?;
                        let argument = required_named_arg(
                            bound[0],
                            "direct backend expected `write_all()` to receive `text`",
                        )?;
                        let loaded = self.load_operand(&argument.value)?;
                        let text = self.ensure_opaque(loaded)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.process_pipe_write_all,
                            &[object.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_bytes" => {
                        let bound = ordered_optional_named_args(&["bytes", "timeout"], args)?;
                        let argument = required_named_arg(
                            bound[0],
                            "direct backend expected `write_bytes()` to receive `bytes`",
                        )?;
                        let loaded = self.load_operand(&argument.value)?;
                        let bytes = self.ensure_opaque(loaded)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.process_pipe_write_bytes,
                            &[object.values[0], bytes.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "flush" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `flush()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_pipe_flush, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_pipe_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "process.Completed" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "status" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `status()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_status, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("process.ExitStatus".to_string(), Vec::new()),
                        ))
                    }
                    "success" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `success()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_success, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "stdout" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `stdout()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_stdout, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "stderr" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `stderr()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_stderr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::named("str"),
                        ))
                    }
                    "stdout_bytes" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `stdout_bytes()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_stdout_bytes, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        ))
                    }
                    "stderr_bytes" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `stderr_bytes()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_stderr_bytes, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        ))
                    }
                    "check" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `check()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_completed_check, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "process.Supervisor" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "start" => {
                        let bound = ordered_optional_named_args(
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
                            args,
                        )?;
                        let required = |index: usize, label: &str| {
                            required_named_arg(
                                bound[index],
                                &format!(
                                    "direct backend expected `start()` to receive `{}`",
                                    label
                                ),
                            )
                        };
                        let name_loaded = self.load_operand(&required(0, "name")?.value)?;
                        let name = self.ensure_opaque(name_loaded)?;
                        let command_loaded = self.load_operand(&required(1, "command")?.value)?;
                        let command = self.ensure_opaque(command_loaded)?;
                        let cwd = if let Some(argument) = bound[2] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            self.compile_enum_variant_for_target("Option", "None", &[], None)?
                        };
                        let env = if let Some(argument) = bound[3] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let init = self.builder.ins().call(self.map_empty, &[]);
                            self.owned_opaque_result(
                                self.builder.inst_results(init).to_vec(),
                                Type::Named(
                                    "dict".to_string(),
                                    vec![Type::named("str"), Type::named("str")],
                                ),
                            )
                        };
                        let stdin = if let Some(argument) = bound[4] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let inst = self.builder.ins().call(self.process_null, &[]);
                            self.owned_opaque_result(
                                self.builder.inst_results(inst).to_vec(),
                                Type::Named("process.Stdio".to_string(), Vec::new()),
                            )
                        };
                        let stdout = if let Some(argument) = bound[5] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let inst = self.builder.ins().call(self.process_inherit, &[]);
                            self.owned_opaque_result(
                                self.builder.inst_results(inst).to_vec(),
                                Type::Named("process.Stdio".to_string(), Vec::new()),
                            )
                        };
                        let stderr = if let Some(argument) = bound[6] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let inst = self.builder.ins().call(self.process_inherit, &[]);
                            self.owned_opaque_result(
                                self.builder.inst_results(inst).to_vec(),
                                Type::Named("process.Stdio".to_string(), Vec::new()),
                            )
                        };
                        let restart = if let Some(argument) = bound[7] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            self.compile_enum_variant_for_target(
                                "process.RestartPolicy",
                                "OnFailure",
                                &[],
                                None,
                            )?
                        };
                        let backoff = if let Some(argument) = bound[8] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let milliseconds = self.builder.ins().iconst(types::I64, 100);
                            let unit_nanoseconds = self.builder.ins().iconst(
                                types::I64,
                                crate::runtime_value::NANOS_PER_MILLISECOND as i64,
                            );
                            let inst = self
                                .builder
                                .ins()
                                .call(self.duration_from_i64, &[milliseconds, unit_nanoseconds]);
                            self.owned_opaque_result(
                                self.builder.inst_results(inst).to_vec(),
                                Type::named("Duration"),
                            )
                        };
                        let max_restarts = if let Some(argument) = bound[9] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let minus_one = self.builder.ins().iconst(types::I64, -1);
                            self.ensure_opaque(ValueRef {
                                values: vec![minus_one],
                                ty: DirectType::Scalar(ScalarKind::Int32),
                            })?
                        };
                        let group = if let Some(argument) = bound[10] {
                            let loaded = self.load_operand(&argument.value)?;
                            self.ensure_opaque(loaded)?
                        } else {
                            let one = self.builder.ins().iconst(types::I64, 1);
                            self.ensure_opaque(ValueRef {
                                values: vec![one],
                                ty: DirectType::Scalar(ScalarKind::Bool),
                            })?
                        };
                        let name = self.transfer_owned_opaque_value(&name);
                        let command = self.transfer_owned_opaque_value(&command);
                        let cwd = self.transfer_owned_opaque_value(&cwd);
                        let env = self.transfer_owned_opaque_value(&env);
                        let stdin = self.transfer_owned_opaque_value(&stdin);
                        let stdout = self.transfer_owned_opaque_value(&stdout);
                        let stderr = self.transfer_owned_opaque_value(&stderr);
                        let restart = self.transfer_owned_opaque_value(&restart);
                        let backoff = self.transfer_owned_opaque_value(&backoff);
                        let max_restarts = self.transfer_owned_opaque_value(&max_restarts);
                        let group = self.transfer_owned_opaque_value(&group);
                        let inst = self.builder.ins().call(
                            self.process_supervisor_start,
                            &[
                                object.values[0],
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
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "wait" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_supervisor_wait, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("process.SupervisorWait".to_string(), Vec::new()),
                        ))
                    }
                    "wait_or_none" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self.builder.ins().call(
                            self.process_supervisor_wait_or_none,
                            &[object.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "process.SupervisorEvent".to_string(),
                                            Vec::new(),
                                        )],
                                    ),
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "stop" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `stop()` to take no arguments".to_string()
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_supervisor_stop, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("process.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "is_empty" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `is_empty()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_supervisor_is_empty, &[object.values[0]]);
                        Ok(ValueRef {
                            values: self.builder.inst_results(inst).to_vec(),
                            ty: DirectType::Scalar(ScalarKind::Bool),
                        })
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.process_supervisor_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.TcpListener" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "accept" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_listener_accept, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("net.TcpStream".to_string(), Vec::new()),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "local_addr" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `local_addr()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_listener_local_addr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_listener_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.TcpStream" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "read_all" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_read_all, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_line" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_read_line, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_bytes" => {
                        let bound = ordered_optional_named_args(&["max_bytes", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `read_bytes()` to receive `max_bytes`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tcp_stream_read_bytes,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "list".to_string(),
                                            vec![Type::named("uint8")],
                                        )],
                                    ),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_exact" => {
                        let bound = ordered_optional_named_args(&["count", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `read_exact()` to receive `count`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tcp_stream_read_exact,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_all" => {
                        let bound = ordered_optional_named_args(&["text", "timeout"], args)?;
                        let argument = required_named_arg(
                            bound[0],
                            "direct backend expected `write_all()` to receive `text`",
                        )?;
                        let loaded = self.load_operand(&argument.value)?;
                        let text = self.ensure_opaque(loaded)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tcp_stream_write_all,
                            &[object.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "write_bytes" => {
                        let bound = ordered_optional_named_args(&["bytes", "timeout"], args)?;
                        let argument = required_named_arg(
                            bound[0],
                            "direct backend expected `write_bytes()` to receive `bytes`",
                        )?;
                        let loaded = self.load_operand(&argument.value)?;
                        let bytes = self.ensure_opaque(loaded)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tcp_stream_write_bytes,
                            &[object.values[0], bytes.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "flush" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `flush()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_flush, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "local_addr" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `local_addr()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_local_addr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "peer_addr" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `peer_addr()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_peer_addr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "shutdown_read" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `shutdown_read()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_shutdown_read, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "shutdown_write" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `shutdown_write()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_shutdown_write, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "shutdown_both" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected `shutdown_both()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_shutdown_both, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tcp_stream_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.UdpSocket" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "send_text" => {
                        let bound =
                            ordered_optional_named_args(&["address", "text", "timeout"], args)?;
                        let address = required_named_arg(
                            bound[0],
                            "direct backend expected `send_text()` to receive `address`",
                        )?;
                        let text = required_named_arg(
                            bound[1],
                            "direct backend expected `send_text()` to receive `text`",
                        )?;
                        let loaded_address = self.load_operand(&address.value)?;
                        let address = self.ensure_opaque(loaded_address)?;
                        let loaded_text = self.load_operand(&text.value)?;
                        let text = self.ensure_opaque(loaded_text)?;
                        let timeout = self.lower_optional_opaque_arg(bound[2])?;
                        let inst = self.builder.ins().call(
                            self.udp_socket_send_text,
                            &[object.values[0], address.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "send_bytes" => {
                        let bound =
                            ordered_optional_named_args(&["address", "bytes", "timeout"], args)?;
                        let address = required_named_arg(
                            bound[0],
                            "direct backend expected `send_bytes()` to receive `address`",
                        )?;
                        let bytes = required_named_arg(
                            bound[1],
                            "direct backend expected `send_bytes()` to receive `bytes`",
                        )?;
                        let loaded_address = self.load_operand(&address.value)?;
                        let address = self.ensure_opaque(loaded_address)?;
                        let loaded_bytes = self.load_operand(&bytes.value)?;
                        let bytes = self.ensure_opaque(loaded_bytes)?;
                        let timeout = self.lower_optional_opaque_arg(bound[2])?;
                        let inst = self.builder.ins().call(
                            self.udp_socket_send_bytes,
                            &[
                                object.values[0],
                                address.values[0],
                                bytes.values[0],
                                timeout,
                            ],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "recv" => {
                        let bound = ordered_optional_named_args(&["max_bytes", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `recv()` to receive `max_bytes`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.udp_socket_recv,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "list".to_string(),
                                            vec![Type::named("uint8")],
                                        )],
                                    ),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "recv_from" => {
                        let bound = ordered_optional_named_args(&["max_bytes", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `recv_from()` to receive `max_bytes`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.udp_socket_recv_from,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "net.UdpDatagram".to_string(),
                                            Vec::new(),
                                        )],
                                    ),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "local_addr" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.udp_socket_local_addr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "peer_addr" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.udp_socket_peer_addr, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.udp_socket_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.UdpDatagram" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "address" => {
                        let results = self
                            .runtime_call_results(self.udp_datagram_address, &[object.values[0]]);
                        Ok(self.owned_opaque_result(results, Type::named("str")))
                    }
                    "bytes" => {
                        let results =
                            self.runtime_call_results(self.udp_datagram_bytes, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        ))
                    }
                    "text" => {
                        let results =
                            self.runtime_call_results(self.udp_datagram_text, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.HttpListener" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "accept" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.http_listener_accept, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("net.HttpExchange".to_string(), Vec::new()),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "local_addr" => {
                        let results = self.runtime_call_results(
                            self.http_listener_local_addr,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.http_listener_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.HttpExchange" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "method" => {
                        let results = self
                            .runtime_call_results(self.http_exchange_method, &[object.values[0]]);
                        Ok(self.owned_opaque_result(results, Type::named("str")))
                    }
                    "path" => {
                        let results =
                            self.runtime_call_results(self.http_exchange_path, &[object.values[0]]);
                        Ok(self.owned_opaque_result(results, Type::named("str")))
                    }
                    "headers" => {
                        let results = self
                            .runtime_call_results(self.http_exchange_headers, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "dict".to_string(),
                                vec![Type::named("str"), Type::named("str")],
                            ),
                        ))
                    }
                    "body_text" => {
                        let results = self.runtime_call_results(
                            self.http_exchange_body_text,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "body_bytes" => {
                        let results = self.runtime_call_results(
                            self.http_exchange_body_bytes,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        ))
                    }
                    "respond_text" => {
                        let bound = ordered_named_args(&["status", "text", "headers"], args)?;
                        let loaded_status = self.load_operand(&bound[0].value)?;
                        let status = self
                            .coerce_value(loaded_status, &DirectType::Scalar(ScalarKind::Int32))?;
                        let status = self.ensure_opaque(status)?;
                        let loaded_text = self.load_operand(&bound[1].value)?;
                        let text = self.ensure_opaque(loaded_text)?;
                        let loaded_headers = self.load_operand(&bound[2].value)?;
                        let headers = self.ensure_opaque(loaded_headers)?;
                        let text = self.transfer_owned_opaque_value(&text);
                        let headers = self.transfer_owned_opaque_value(&headers);
                        let inst = self.builder.ins().call(
                            self.http_exchange_respond_text,
                            &[object.values[0], status.values[0], text, headers],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "respond_bytes" => {
                        let bound = ordered_named_args(&["status", "bytes", "headers"], args)?;
                        let loaded_status = self.load_operand(&bound[0].value)?;
                        let status = self
                            .coerce_value(loaded_status, &DirectType::Scalar(ScalarKind::Int32))?;
                        let status = self.ensure_opaque(status)?;
                        let loaded_bytes = self.load_operand(&bound[1].value)?;
                        let bytes = self.ensure_opaque(loaded_bytes)?;
                        let loaded_headers = self.load_operand(&bound[2].value)?;
                        let headers = self.ensure_opaque(loaded_headers)?;
                        let bytes = self.transfer_owned_opaque_value(&bytes);
                        let headers = self.transfer_owned_opaque_value(&headers);
                        let inst = self.builder.ins().call(
                            self.http_exchange_respond_bytes,
                            &[object.values[0], status.values[0], bytes, headers],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "close" => Ok(unit_value(&mut self.builder)),
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.HttpResponse" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "status" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.http_response_status, &[object.values[0]]);
                        let status = self.builder.inst_results(inst)[0];
                        self.emit_int32_bounds_check(status, None)?;
                        Ok(ValueRef {
                            values: vec![status],
                            ty: DirectType::Scalar(ScalarKind::Int32),
                        })
                    }
                    "reason" => {
                        let results = self
                            .runtime_call_results(self.http_response_reason, &[object.values[0]]);
                        Ok(self.owned_opaque_result(results, Type::named("str")))
                    }
                    "headers" => {
                        let results = self
                            .runtime_call_results(self.http_response_headers, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "dict".to_string(),
                                vec![Type::named("str"), Type::named("str")],
                            ),
                        ))
                    }
                    "text" => {
                        let results =
                            self.runtime_call_results(self.http_response_text, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "bytes" => {
                        let results = self
                            .runtime_call_results(self.http_response_bytes, &[object.values[0]]);
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        ))
                    }
                    "close" => Ok(unit_value(&mut self.builder)),
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.WebSocketListener" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "accept" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.websocket_listener_accept, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("net.WebSocket".to_string(), Vec::new()),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "local_addr" => {
                        let results = self.runtime_call_results(
                            self.websocket_listener_local_addr,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => Ok(unit_value(&mut self.builder)),
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.WebSocket" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "send_text" => {
                        let bound = ordered_optional_named_args(&["text", "timeout"], args)?;
                        let text = required_named_arg(
                            bound[0],
                            "direct backend expected `send_text()` to receive `text`",
                        )?;
                        let loaded_text = self.load_operand(&text.value)?;
                        let text = self.ensure_opaque(loaded_text)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.websocket_send_text,
                            &[object.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "send_bytes" => {
                        let bound = ordered_optional_named_args(&["bytes", "timeout"], args)?;
                        let bytes = required_named_arg(
                            bound[0],
                            "direct backend expected `send_bytes()` to receive `bytes`",
                        )?;
                        let loaded_bytes = self.load_operand(&bytes.value)?;
                        let bytes = self.ensure_opaque(loaded_bytes)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.websocket_send_bytes,
                            &[object.values[0], bytes.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "recv_text" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.websocket_recv_text, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "recv_bytes" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.websocket_recv_bytes, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named(
                                        "Option".to_string(),
                                        vec![Type::Named(
                                            "list".to_string(),
                                            vec![Type::named("uint8")],
                                        )],
                                    ),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.websocket_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.UnixListener" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "accept" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.unix_listener_accept, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("net.UnixStream".to_string(), Vec::new()),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.unix_listener_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.UnixStream" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "read_line" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.unix_stream_read_line, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_exact" => {
                        let bound = ordered_optional_named_args(&["count", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `read_exact()` to receive `count`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.unix_stream_read_exact,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_all" => {
                        let bound = ordered_optional_named_args(&["text", "timeout"], args)?;
                        let text = required_named_arg(
                            bound[0],
                            "direct backend expected `write_all()` to receive `text`",
                        )?;
                        let loaded_text = self.load_operand(&text.value)?;
                        let text = self.ensure_opaque(loaded_text)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.unix_stream_write_all,
                            &[object.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.unix_stream_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.TlsListener" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "accept" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tls_listener_accept, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("net.TlsStream".to_string(), Vec::new()),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "local_addr" => {
                        let results = self.runtime_call_results(
                            self.tls_listener_local_addr,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            results,
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::named("str"),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tls_listener_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "net.TlsStream" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "read_line" => {
                        let bound = ordered_optional_named_args(&["timeout"], args)?;
                        let timeout = self.lower_optional_opaque_arg(bound[0])?;
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tls_stream_read_line, &[object.values[0], timeout]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "read_exact" => {
                        let bound = ordered_optional_named_args(&["count", "timeout"], args)?;
                        let count = required_named_arg(
                            bound[0],
                            "direct backend expected `read_exact()` to receive `count`",
                        )?;
                        let loaded_count = self.load_operand(&count.value)?;
                        let count = self
                            .coerce_value(loaded_count, &DirectType::Scalar(ScalarKind::Int32))?;
                        let count = self.ensure_opaque(count)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tls_stream_read_exact,
                            &[object.values[0], count.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                                    Type::Named("io.Error".to_string(), Vec::new()),
                                ],
                            ),
                        ))
                    }
                    "write_all" => {
                        let bound = ordered_optional_named_args(&["text", "timeout"], args)?;
                        let text = required_named_arg(
                            bound[0],
                            "direct backend expected `write_all()` to receive `text`",
                        )?;
                        let loaded_text = self.load_operand(&text.value)?;
                        let text = self.ensure_opaque(loaded_text)?;
                        let timeout = self.lower_optional_opaque_arg(bound[1])?;
                        let inst = self.builder.ins().call(
                            self.tls_stream_write_all,
                            &[object.values[0], text.values[0], timeout],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                            ),
                        ))
                    }
                    "close" => {
                        let inst = self
                            .builder
                            .ins()
                            .call(self.tls_stream_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if self.classes.contains_key(name)
                || (BuiltinMember::resolve(name, field).is_none()
                    && self.find_trait_method(object_ty, field).is_some())
            {
                if let Ok(result) = self.compile_class_member_call(
                    name,
                    Some(object_ty.clone()),
                    object.clone(),
                    field,
                    receiver_place,
                    args,
                    None,
                ) {
                    return Ok(result);
                }
            }
            if name == "Queue" {
                let object = self.ensure_opaque(object)?;
                let element_ty = class_args
                    .first()
                    .cloned()
                    .unwrap_or(Type::named("Unknown"));
                let element_direct_ty =
                    ensure_direct_type(&element_ty, &self.classes, "Queue element")?;
                return match field {
                    "put" => {
                        let mut value_arg: Option<&MirArg> = None;
                        let mut timeout_arg: Option<&MirArg> = None;
                        for argument in args {
                            match argument.name.as_deref() {
                                Some("value") => value_arg = Some(argument),
                                Some("timeout") => timeout_arg = Some(argument),
                                Some(other) => {
                                    return Err(format!(
                                        "direct backend expected `put()` arguments to use `value` and optional `timeout`, found `{}`",
                                        other
                                    ))
                                }
                                None if value_arg.is_none() => value_arg = Some(argument),
                                None if timeout_arg.is_none() => timeout_arg = Some(argument),
                                None => {
                                    return Err(
                                        "direct backend expected `put(value, timeout=...)`".to_string(),
                                    )
                                }
                            }
                        }
                        let Some(argument) = value_arg else {
                            return Err(
                                "direct backend expected `put()` to receive a value argument"
                                    .to_string(),
                            );
                        };
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let inst = if let Some(timeout_arg) = timeout_arg {
                            let timeout = self.load_operand(&timeout_arg.value)?;
                            let timeout = self.ensure_opaque(timeout)?;
                            let value = self.transfer_owned_opaque_value(&value);
                            self.builder.ins().call(
                                self.channel_send_timeout_value,
                                &[object.values[0], value, timeout.values[0]],
                            )
                        } else {
                            let value = self.transfer_owned_opaque_value(&value);
                            self.builder
                                .ins()
                                .call(self.channel_send, &[object.values[0], value])
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("SendError".to_string(), vec![element_ty.clone()]),
                                ],
                            ),
                        ))
                    }
                    "try_put" => {
                        let [argument] = args else {
                            return Err(
                                "direct backend expected `try_put()` to receive one argument"
                                    .to_string(),
                            );
                        };
                        if argument.name.as_deref() != Some("value") && argument.name.is_some() {
                            return Err(
                                "direct backend expected `try_put()` to receive only `value=`"
                                    .to_string(),
                            );
                        }
                        let value = self
                            .load_operand_as_opaque_direct(&argument.value, &element_direct_ty)?;
                        let value = self.transfer_owned_opaque_value(&value);
                        let inst = self
                            .builder
                            .ins()
                            .call(self.channel_try_send, &[object.values[0], value]);
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Result".to_string(),
                                vec![
                                    Type::Unit,
                                    Type::Named("SendError".to_string(), vec![element_ty.clone()]),
                                ],
                            ),
                        ))
                    }
                    "get" => {
                        let inst = match args {
                            [] => self
                                .builder
                                .ins()
                                .call(self.channel_recv, &[object.values[0]]),
                            [argument] => {
                                if argument.name.as_deref() != Some("timeout")
                                    && argument.name.is_some()
                                {
                                    return Err(
                                        "direct backend expected `get()` or `get(timeout=...)`"
                                            .to_string(),
                                    );
                                }
                                let timeout = self.load_operand(&argument.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                self.builder.ins().call(
                                    self.channel_recv_timeout_value,
                                    &[object.values[0], timeout.values[0]],
                                )
                            }
                            _ => {
                                return Err("direct backend expected `get()` or `get(timeout=...)`"
                                    .to_string())
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "QueueReceive".to_string(),
                                vec![class_args
                                    .first()
                                    .cloned()
                                    .unwrap_or(Type::named("Unknown"))],
                            ),
                        ))
                    }
                    "__get_in_task_group" => {
                        let [task_group] = args else {
                            return Err(
                                "direct backend expected internal `__get_in_task_group()` to receive one task-group argument"
                                    .to_string(),
                            );
                        };
                        let task_group = self.load_operand(&task_group.value)?;
                        let task_group = self.ensure_opaque(task_group)?;
                        let inst = self.builder.ins().call(
                            self.channel_recv_in_task_group,
                            &[object.values[0], task_group.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "QueueReceive".to_string(),
                                vec![class_args
                                    .first()
                                    .cloned()
                                    .unwrap_or(Type::named("Unknown"))],
                            ),
                        ))
                    }
                    "__get_with_registered_producers" => {
                        if !args.is_empty() {
                            return Err(
                                "direct backend expected internal `__get_with_registered_producers()` to take no arguments"
                                    .to_string(),
                            );
                        }
                        let inst = self.builder.ins().call(
                            self.channel_recv_with_registered_producers,
                            &[object.values[0]],
                        );
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "QueueReceive".to_string(),
                                vec![class_args
                                    .first()
                                    .cloned()
                                    .unwrap_or(Type::named("Unknown"))],
                            ),
                        ))
                    }
                    "get_or_none" => {
                        let inst = match args {
                            [] => self
                                .builder
                                .ins()
                                .call(self.channel_recv_or_none, &[object.values[0]]),
                            [argument] => {
                                if argument.name.as_deref() != Some("timeout")
                                    && argument.name.is_some()
                                {
                                    return Err(
                                        "direct backend expected `get_or_none()` or `get_or_none(timeout=...)`"
                                            .to_string(),
                                    );
                                }
                                let timeout = self.load_operand(&argument.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                self.builder.ins().call(
                                    self.channel_recv_or_none_timeout_value,
                                    &[object.values[0], timeout.values[0]],
                                )
                            }
                            _ => {
                                return Err(
                                    "direct backend expected `get_or_none()` or `get_or_none(timeout=...)`"
                                        .to_string(),
                                )
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named("Option".to_string(), vec![element_ty.clone()]),
                        ))
                    }
                    "get_or" => {
                        let bound = ordered_optional_named_args(&["default", "timeout"], args)?;
                        let default = required_named_arg(
                            bound[0],
                            "direct backend expected `get_or()` to receive `default`",
                        )?;
                        let loaded = self.load_operand(&default.value)?;
                        let loaded = self.ensure_opaque(loaded)?;
                        let inst = match bound[1] {
                            Some(timeout) => {
                                let timeout = self.load_operand(&timeout.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                let default = self.transfer_owned_opaque_value(&loaded);
                                self.builder.ins().call(
                                    self.channel_recv_or_value_timeout_value,
                                    &[object.values[0], default, timeout.values[0]],
                                )
                            }
                            None => {
                                let default = self.transfer_owned_opaque_value(&loaded);
                                self.builder
                                    .ins()
                                    .call(self.channel_recv_or_value, &[object.values[0], default])
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            class_args
                                .first()
                                .cloned()
                                .unwrap_or(Type::named("Unknown")),
                        ))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.channel_close, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "Task" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "result" => {
                        let inst = match args {
                            [] => self.builder.ins().call(self.task_join, &[object.values[0]]),
                            [argument] => {
                                if argument.name.as_deref() != Some("timeout")
                                    && argument.name.is_some()
                                {
                                    return Err(
                                        "direct backend expected `result()` or `result(timeout=...)`"
                                            .to_string(),
                                    );
                                }
                                let timeout = self.load_operand(&argument.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                self.builder.ins().call(
                                    self.task_join_timeout_value,
                                    &[object.values[0], timeout.values[0]],
                                )
                            }
                            _ => {
                                return Err(
                                    "direct backend expected `result()` or `result(timeout=...)`"
                                        .to_string(),
                                )
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "TaskResult".to_string(),
                                vec![class_args.first().cloned().unwrap_or(Type::Unit)],
                            ),
                        ))
                    }
                    "result_or_none" => {
                        let inst = match args {
                            [] => self
                                .builder
                                .ins()
                                .call(self.task_join_or_none, &[object.values[0]]),
                            [argument] => {
                                if argument.name.as_deref() != Some("timeout")
                                    && argument.name.is_some()
                                {
                                    return Err(
                                        "direct backend expected `result_or_none()` or `result_or_none(timeout=...)`"
                                            .to_string(),
                                    );
                                }
                                let timeout = self.load_operand(&argument.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                self.builder.ins().call(
                                    self.task_join_or_none_timeout_value,
                                    &[object.values[0], timeout.values[0]],
                                )
                            }
                            _ => {
                                return Err(
                                    "direct backend expected `result_or_none()` or `result_or_none(timeout=...)`"
                                        .to_string(),
                                )
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            Type::Named(
                                "Option".to_string(),
                                vec![class_args.first().cloned().unwrap_or(Type::Unit)],
                            ),
                        ))
                    }
                    "result_or" => {
                        let bound = ordered_optional_named_args(&["default", "timeout"], args)?;
                        let default = required_named_arg(
                            bound[0],
                            "direct backend expected `result_or()` to receive `default`",
                        )?;
                        let loaded = self.load_operand(&default.value)?;
                        let loaded = self.ensure_opaque(loaded)?;
                        let inst = match bound[1] {
                            Some(timeout) => {
                                let timeout = self.load_operand(&timeout.value)?;
                                let timeout = self.ensure_opaque(timeout)?;
                                let default = self.transfer_owned_opaque_value(&loaded);
                                self.builder.ins().call(
                                    self.task_join_or_value_timeout_value,
                                    &[object.values[0], default, timeout.values[0]],
                                )
                            }
                            None => {
                                let default = self.transfer_owned_opaque_value(&loaded);
                                self.builder
                                    .ins()
                                    .call(self.task_join_or_value, &[object.values[0], default])
                            }
                        };
                        Ok(self.owned_opaque_result(
                            self.builder.inst_results(inst).to_vec(),
                            class_args.first().cloned().unwrap_or(Type::Unit),
                        ))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            if name == "TaskGroup" {
                let object = self.ensure_opaque(object)?;
                return match field {
                    "cancel" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `cancel()` to take no arguments"
                                .to_string());
                        }
                        let inst = self
                            .builder
                            .ins()
                            .call(self.task_group_cancel, &[object.values[0]]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    "close" => {
                        if !args.is_empty() {
                            return Err("direct backend expected `close()` to take no arguments"
                                .to_string());
                        }
                        let cancel_before = self.builder.ins().iconst(types::I64, 0);
                        let inst = self
                            .builder
                            .ins()
                            .call(self.task_group_close, &[object.values[0], cancel_before]);
                        self.release_opaque_handle(self.builder.inst_results(inst)[0]);
                        Ok(unit_value(&mut self.builder))
                    }
                    _ => Err(format!(
                        "direct backend does not know runtime member `{}.{}`",
                        name, field
                    )),
                };
            }
            let _ = class_args;
        }

        let candidates = self.dynamic_method_candidates(field, trait_name);
        if candidates.is_empty() {
            return Err(format!(
                "direct backend does not know dynamic method `.{}` on `{}`",
                field, object_ty
            ));
        }
        if candidates.len() == 1 {
            let Type::Named(candidate_name, _) = &candidates[0].0 else {
                return Err(format!(
                    "direct backend does not know how to call dynamic method `.{}` for `{}`",
                    field, candidates[0].0
                ));
            };
            return self.compile_class_member_call(
                candidate_name,
                Some(candidates[0].0.clone()),
                object,
                field,
                receiver_place,
                args,
                trait_name,
            );
        }

        let result_ty = if candidates
            .iter()
            .map(|(_, method)| self.call_result_type(&method.function_name))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .windows(2)
            .all(|window| window[0] == window[1])
        {
            self.call_result_type(&candidates[0].1.function_name)?
        } else {
            DirectType::Opaque(Type::named("Unknown"))
        };

        let join_block = self.builder.create_block();
        let mut current_fallthrough = None;
        let result_vars = self.declare_temporary_result_storage(&result_ty)?;
        // Every runtime candidate starts from the same ownership state. Emitting
        // one candidate's taken edge can consume or release an owned projected
        // receiver, but that edge is skipped when control falls through to the
        // next candidate. Keep the compile-time ledger path-local so a later
        // candidate still emits the release owed by its runtime path.
        let caller_owned = self.owned_opaque_temporaries.clone();
        for (candidate_ty, _method) in candidates.iter() {
            let Type::Named(candidate_name, _) = candidate_ty else {
                continue;
            };
            self.owned_opaque_temporaries = caller_owned.clone();
            let matched = self.value_matches_runtime_type(object.values[0], candidate_ty)?;
            let then_block = self.builder.create_block();
            let else_block = self.builder.create_block();
            self.builder
                .ins()
                .brif(matched, then_block, &[], else_block, &[]);
            self.builder.switch_to_block(then_block);
            self.owned_opaque_temporaries = caller_owned.clone();
            let call_result = self.compile_class_member_call(
                candidate_name,
                Some(candidate_ty.clone()),
                object.clone(),
                field,
                receiver_place,
                args,
                trait_name,
            )?;
            let coerced_result = self.coerce_value(call_result, &result_ty)?;
            self.store_result_vars(&result_vars, &coerced_result)?;
            self.release_all_temporary_owned();
            self.builder.ins().jump(join_block, &[]);
            self.builder.seal_block(then_block);
            self.builder.switch_to_block(else_block);
            self.owned_opaque_temporaries = caller_owned.clone();
            current_fallthrough = Some(else_block);
        }
        if let Some(else_block) = current_fallthrough {
            self.builder.switch_to_block(else_block);
            self.emit_pending_cleanups(true)?;
            self.builder.ins().trap(TrapCode::unwrap_user(1));
            self.builder.seal_block(else_block);
        }
        self.builder.switch_to_block(join_block);
        self.builder.seal_block(join_block);
        // Every successful candidate released its branch-local temporaries
        // before jumping here. The restored fallthrough ledger belongs only to
        // the trapping no-match edge and must not be released a second time at
        // the join.
        self.owned_opaque_temporaries.clear();
        self.load_result_vars(&result_vars, result_ty)
    }

    fn compile_opaque_construct(
        &mut self,
        class_name: &str,
        fields: &[crate::mir::MirFieldInit],
        target_ty: &Type,
    ) -> std::result::Result<ValueRef, String> {
        let class = self.classes.get(class_name).cloned().ok_or(format!(
            "direct backend does not know class `{}`",
            class_name
        ))?;
        let (class_ptr, class_len) = self.string_constant(class_name.as_bytes())?;
        let init = self
            .builder
            .ins()
            .call(self.instance_empty, &[class_ptr, class_len]);
        let current =
            self.owned_opaque_result(self.builder.inst_results(init).to_vec(), target_ty.clone());
        let substitutions = match target_ty {
            Type::Named(name, args) if name == class_name => class
                .type_params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>(),
            _ => HashMap::new(),
        };
        for field in fields {
            let field_ty = class
                .fields
                .iter()
                .find(|candidate| candidate.name == field.name)
                .map(|candidate| substitute_type(&candidate.ty, &substitutions))
                .ok_or({
                    format!(
                        "direct backend construction for `{}` is missing field metadata for `{}`",
                        class_name, field.name
                    )
                })?;
            let field_ty = ensure_direct_type(
                &field_ty,
                &self.classes,
                &format!("field `{}` on class `{}`", field.name, class_name),
            )?;
            let loaded = self.load_operand_for_target(&field.value, &field_ty)?;
            let loaded = self.coerce_value(loaded, &field_ty)?;
            let loaded = self.ensure_opaque(loaded)?;
            let loaded = self.transfer_owned_opaque_value(&loaded);
            let (field_ptr, field_len) = self.string_constant(field.name.as_bytes())?;
            self.builder.ins().call(
                self.instance_set_field_owned,
                &[current.values[0], field_ptr, field_len, loaded],
            );
        }
        Ok(current)
    }

    fn compile_start_task(&mut self, task: TaskStart<'_>) -> std::result::Result<ValueRef, String> {
        let TaskStart {
            mode,
            stack_size,
            task_group,
            function,
            args,
            spawn_span,
            target,
        } = task;
        let function_value = self.load_operand(function)?;
        let function_value = self.ensure_opaque(function_value)?;
        let function_params =
            match infer_operand_type(function, &self.variable_types, &self.classes) {
                Some(DirectType::Opaque(Type::Function { params, .. })) => params,
                Some(DirectType::Opaque(Type::Closure { params, .. })) => *params,
                _ => Vec::new(),
            };
        let arg_count_value = self
            .builder
            .ins()
            .iconst(types::I64, function_params.len() as i64);
        let buffer_call = self
            .builder
            .ins()
            .call(self.arg_buffer_new, &[arg_count_value]);
        let buffer = self.builder.inst_results(buffer_call)[0];
        let guard_call = self
            .builder
            .ins()
            .call(self.task_arg_buffer_guard, &[buffer, arg_count_value]);
        let buffer_guard = self.builder.inst_results(guard_call)[0];
        let expected = function_value_param_types(
            &function_params,
            &self.classes,
            "task function-value parameter",
        )?;
        let binding = bind_function_value_args(
            &function_params,
            args,
            "task function value has no parameter named",
            "duplicate task function-value argument",
        )?;
        for (arg, index) in args.iter().zip(&binding.source_slots) {
            let index = *index;
            if arg.writeback_place.is_some() {
                return Err(
                    "direct backend does not yet support borrowed task-start arguments".to_string(),
                );
            }
            let expected_ty = &expected[index];
            let loaded = self.load_operand_for_target(&arg.value, expected_ty)?;
            let value = self.coerce_value(loaded, expected_ty)?;
            let value = self.ensure_opaque(value)?;
            let index_value = self.builder.ins().iconst(types::I64, index as i64);
            self.builder.ins().call(
                self.arg_buffer_store,
                &[buffer, index_value, value.values[0]],
            );
        }
        let transfer_defaults = self.builder.ins().iconst(types::I64, 1);
        self.builder.ins().call(
            self.function_bind_defaults,
            &[
                function_value.values[0],
                buffer,
                arg_count_value,
                transfer_defaults,
            ],
        );
        for (index, supplied) in binding.slots.iter().enumerate() {
            if supplied.is_some() {
                continue;
            }
            let raw =
                self.builder
                    .ins()
                    .load(types::I64, MemFlags::new(), buffer, (index as i32) * 8);
            self.tag_raw_opaque_runtime_type(raw, &expected[index])?;
        }
        let returns_handle_value = self
            .builder
            .ins()
            .iconst(types::I64, if mode.returns_handle { 1 } else { 0 });
        let result_is_copy_value = self
            .builder
            .ins()
            .iconst(types::I64, if mode.result_is_copy { 1 } else { 0 });
        let (stack_size_present_value, stack_size_value) = match stack_size {
            Some(stack_size) => {
                let target = DirectType::Scalar(ScalarKind::Int64);
                let value = self.load_operand_for_target(stack_size, &target)?;
                (
                    self.builder.ins().iconst(types::I64, 1),
                    self.coerce_value(value, &target)?.values[0],
                )
            }
            None => (
                self.builder.ins().iconst(types::I64, 0),
                self.builder.ins().iconst(types::I64, 0),
            ),
        };
        let group = self.load_operand(task_group)?;
        let group = self.ensure_opaque(group)?;
        let task_group_value = group.values[0];
        let current_function_name = self.current_function_name.clone();
        let current_function_path = self.current_function_path.clone();
        let (parent_function_ptr, parent_function_len) =
            self.string_constant(current_function_name.as_bytes())?;
        let (spawn_path_ptr, spawn_path_len) =
            self.string_constant(current_function_path.as_bytes())?;
        let spawn_line = self
            .builder
            .ins()
            .iconst(types::I64, spawn_span.line as i64);
        let spawn_column = self
            .builder
            .ins()
            .iconst(types::I64, spawn_span.column as i64);
        // Every operation that can trap after raw allocation has completed.
        // Transfer the buffer from the direct cleanup stack to the task runtime
        // only across this allocation-free call boundary.
        self.builder
            .ins()
            .call(self.task_arg_buffer_disarm, &[buffer_guard]);
        let call = self.builder.ins().call(
            self.start_task_call,
            &[
                function_value.values[0],
                buffer,
                arg_count_value,
                returns_handle_value,
                task_group_value,
                result_is_copy_value,
                stack_size_present_value,
                stack_size_value,
                parent_function_ptr,
                parent_function_len,
                spawn_path_ptr,
                spawn_path_len,
                spawn_line,
                spawn_column,
            ],
        );
        // MIR checking/lowering has already assigned every handle-returning
        // start to its concrete `Task[T]` target. Carry that exact target
        // through codegen instead of independently reconstructing or
        // defensively revalidating the result type here.
        let ty = if mode.returns_handle {
            target.clone()
        } else {
            DirectType::Scalar(ScalarKind::Unit)
        };
        match ty {
            DirectType::Opaque(ty) => {
                Ok(self.owned_opaque_result(self.builder.inst_results(call).to_vec(), ty))
            }
            _ => {
                self.release_opaque_handle(self.builder.inst_results(call)[0]);
                Ok(unit_value(&mut self.builder))
            }
        }
    }

    fn value_matches_type(
        &mut self,
        value: Value,
        type_name: &str,
    ) -> std::result::Result<Value, String> {
        let (ptr, len) = self.string_constant(type_name.as_bytes())?;
        let inst = self
            .builder
            .ins()
            .call(self.value_type_matches, &[value, ptr, len]);
        Ok(self.builder.inst_results(inst)[0])
    }

    fn value_matches_runtime_type(
        &mut self,
        value: Value,
        ty: &Type,
    ) -> std::result::Result<Value, String> {
        match ty {
            Type::TypeParam(_) => Ok(self.builder.ins().iconst(types::I64, 1)),
            Type::Unit => self.value_matches_type(value, "None"),
            Type::Module(path) => self.value_matches_type(value, &format!("module {}", path)),
            Type::Tuple(_) => {
                let pattern = crate::native_runtime::canonical_runtime_type_name(ty);
                self.value_matches_type(value, &pattern)
            }
            Type::Function { .. } | Type::Closure { .. } => {
                let pattern = crate::native_runtime::canonical_runtime_type_name(ty);
                self.value_matches_type(value, &pattern)
            }
            Type::Named(name, args) => {
                if args.is_empty() {
                    return self.value_matches_type(value, name);
                }

                let pattern = crate::native_runtime::canonical_runtime_type_name(ty);
                let exact = self.value_matches_type(value, &pattern)?;
                let base_matches = self.value_matches_type(value, name)?;

                let Some(class) = self.classes.get(name).cloned() else {
                    return Ok(exact);
                };
                let inspect_block = self.builder.create_block();
                let join_block = self.builder.create_block();
                self.builder.append_block_param(join_block, types::I64);
                let structural_block = self.builder.create_block();
                let tagged = self
                    .builder
                    .ins()
                    .call(self.value_has_runtime_type, &[value]);
                let tagged = self.builder.inst_results(tagged)[0];
                self.builder
                    .ins()
                    .brif(tagged, join_block, &[exact], structural_block, &[]);
                self.builder.switch_to_block(structural_block);
                self.builder
                    .ins()
                    .brif(base_matches, inspect_block, &[], join_block, &[exact]);
                self.builder.seal_block(structural_block);
                self.builder.switch_to_block(inspect_block);

                let mut structural = base_matches;
                let substitutions = class
                    .type_params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect::<HashMap<_, _>>();
                for field in &class.fields {
                    let field_ty = substitute_type(&field.ty, &substitutions);
                    if runtime_type_is_wildcard(&field_ty) {
                        continue;
                    }
                    let (field_ptr, field_len) = self.string_constant(field.name.as_bytes())?;
                    let inst = self
                        .builder
                        .ins()
                        .call(self.instance_get_field, &[value, field_ptr, field_len]);
                    let field_value = self.builder.inst_results(inst)[0];
                    let field_matches = self.value_matches_runtime_type(field_value, &field_ty)?;
                    self.release_opaque_handle(field_value);
                    structural = self.builder.ins().band(structural, field_matches);
                }
                let matched = self.builder.ins().bor(exact, structural);
                self.builder.ins().jump(join_block, &[matched]);
                self.builder.seal_block(inspect_block);
                self.builder.switch_to_block(join_block);
                self.builder.seal_block(join_block);
                Ok(self.builder.block_params(join_block)[0])
            }
        }
    }

    fn span_values(&mut self, span: Option<Span>) -> (Value, Value) {
        let (line, column) = span
            .map(|span| (span.line as i64, span.column as i64))
            .unwrap_or((0, 0));
        (
            self.builder.ins().iconst(types::I64, line),
            self.builder.ins().iconst(types::I64, column),
        )
    }

    fn dynamic_method_candidates(
        &self,
        field: &str,
        trait_name: Option<&str>,
    ) -> Vec<(Type, MirMethod)> {
        let mut candidates = Vec::new();
        if trait_name.is_none() {
            for class in self.classes.values() {
                if let Some(method) = class.methods.iter().find(|method| method.name == field) {
                    candidates.push((usize::MAX, Type::named(&class.name), method.clone()));
                }
            }
        }
        for trait_impl in &self.trait_impls {
            if trait_name.is_some_and(|name| trait_impl.trait_name != name) {
                continue;
            }
            if let Some(method) = trait_impl
                .methods
                .iter()
                .find(|method| method.name == field)
            {
                candidates.push((
                    crate::sema::trait_impl_specificity_parts(
                        &trait_impl.for_type,
                        &trait_impl.trait_args,
                    ),
                    trait_impl.for_type.clone(),
                    method.clone(),
                ));
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        candidates
            .into_iter()
            .map(|(_, ty, method)| (ty, method))
            .collect()
    }

    fn find_trait_method(&self, ty: &Type, field: &str) -> Option<&MirMethod> {
        self.find_trait_method_with_identity(ty, None, field)
    }

    fn find_trait_method_for_trait(
        &self,
        ty: &Type,
        trait_name: &str,
        field: &str,
    ) -> Option<&MirMethod> {
        self.find_trait_method_with_identity(ty, Some(trait_name), field)
    }

    fn find_trait_method_with_identity(
        &self,
        ty: &Type,
        trait_name: Option<&str>,
        field: &str,
    ) -> Option<&MirMethod> {
        let mut best = None;
        let mut best_specificity = 0usize;
        let mut ambiguous = false;
        for trait_impl in &self.trait_impls {
            if trait_name.is_some_and(|name| trait_impl.trait_name != name) {
                continue;
            }
            let mut type_params = BTreeSet::new();
            collect_type_params_from_type(&trait_impl.for_type, &mut type_params);
            let mut substitutions = HashMap::new();
            if !crate::sema::type_pattern_matches(
                &trait_impl.for_type,
                ty,
                &type_params,
                &mut substitutions,
            ) {
                continue;
            }
            let Some(method) = trait_impl
                .methods
                .iter()
                .find(|method| method.name == field)
            else {
                continue;
            };
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
        }
        if ambiguous {
            None
        } else {
            best
        }
    }

    fn declare_temporary_result_storage(
        &mut self,
        ty: &DirectType,
    ) -> std::result::Result<Vec<Variable>, String> {
        let mut vars = Vec::new();
        for abi in ty.abi_types() {
            let variable = Variable::from_u32(self.next_variable_index as u32);
            self.next_variable_index += 1;
            self.builder.declare_var(variable, abi);
            let zero = match abi {
                t if t == types::F64 => self.builder.ins().f64const(Ieee64::with_float(0.0)),
                _ => self.builder.ins().iconst(abi, 0),
            };
            self.builder.def_var(variable, zero);
            vars.push(variable);
        }
        Ok(vars)
    }

    fn store_result_vars(
        &mut self,
        vars: &[Variable],
        value: &ValueRef,
    ) -> std::result::Result<(), String> {
        if matches!(value.ty, DirectType::Opaque(_)) {
            let stored = if self.temporary_owns_opaque(value) {
                self.clear_temporary_opaque_owned(value);
                value.values[0]
            } else {
                self.retain_opaque_handle(value.values[0])
            };
            let Some(var) = vars.first() else {
                return Err("direct backend expected opaque temporary result storage".to_string());
            };
            self.builder.def_var(*var, stored);
            return Ok(());
        }

        for (var, compiled) in vars.iter().zip(value.values.iter()) {
            self.builder.def_var(*var, *compiled);
        }
        Ok(())
    }

    fn load_result_vars(
        &mut self,
        vars: &[Variable],
        ty: DirectType,
    ) -> std::result::Result<ValueRef, String> {
        let values = vars.iter().map(|var| self.builder.use_var(*var)).collect();
        let value = ValueRef { values, ty };
        if matches!(value.ty, DirectType::Opaque(_)) {
            self.mark_temporary_opaque_owned(&value);
        }
        Ok(value)
    }
}

fn unit_value(builder: &mut FunctionBuilder<'_>) -> ValueRef {
    ValueRef {
        values: vec![builder.ins().iconst(types::I64, 0)],
        ty: DirectType::Scalar(ScalarKind::Unit),
    }
}

fn find_method<'a>(class: Option<&'a MirClass>, field: &str) -> Option<&'a MirMethod> {
    let class = class?;
    class
        .methods
        .iter()
        .find(|&method| method.name == field)
        .map(|v| v as _)
}

fn declare_runtime_function(
    module: &mut ObjectModule,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
    result: Option<cranelift_codegen::ir::Type>,
) -> std::result::Result<FuncId, String> {
    let mut sig = module.make_signature();
    for param in params {
        sig.params.push(AbiParam::new(*param));
    }
    if let Some(result) = result {
        sig.returns.push(AbiParam::new(result));
    }
    match module.declare_function(name, Linkage::Import, &sig) {
        Ok(id) => Ok(id),
        Err(error) => Err(format!(
            "failed to declare runtime function `{}`: {}",
            name, error
        )),
    }
}

fn declare_string_constant(
    object: &mut ObjectModule,
    string_data: &mut HashMap<Vec<u8>, DataId>,
    builder: &mut FunctionBuilder<'_>,
    bytes: &[u8],
) -> std::result::Result<(Value, Value), String> {
    let id = if let Some(id) = string_data.get(bytes) {
        *id
    } else {
        let name = format!("aura_data_{}", string_data.len());
        let id = try_or_string_error!(
            object.declare_data(&name, Linkage::Local, false, false),
            "failed to declare string data: {}"
        );
        let mut data = DataDescription::new();
        data.define(bytes.to_vec().into_boxed_slice());
        try_or_string_error!(
            object.define_data(id, &data),
            "failed to define string data: {}"
        );
        string_data.insert(bytes.to_vec(), id);
        id
    };
    let global = object.declare_data_in_func(id, builder.func);
    let ptr = builder.ins().symbol_value(types::I64, global);
    let len = builder.ins().iconst(types::I64, bytes.len() as i64);
    Ok((ptr, len))
}

fn signature_for(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
    call_conv: CallConv,
) -> std::result::Result<Signature, String> {
    let mut signature = Signature::new(call_conv);
    let mut writeback_types = Vec::new();
    if function.receiver.is_some() {
        let receiver_ty = receiver_type(function, classes)?;
        for abi in receiver_ty.abi_types() {
            signature.params.push(AbiParam::new(abi));
        }
        if function.receiver == Some(MirReceiverKind::BorrowMut) {
            writeback_types.push(receiver_ty);
        }
    }
    for param in &function.params {
        let ty = ensure_direct_type(
            &param.ty,
            classes,
            &format!("parameter `{}` on `{}`", param.name, function.name),
        )?;
        for abi in ty.abi_types() {
            signature.params.push(AbiParam::new(abi));
        }
        if param.passing == MirReceiverKind::BorrowMut {
            writeback_types.push(ty);
        }
    }
    let return_ty = ensure_direct_type(
        &function.return_type,
        classes,
        &format!("return type of `{}`", function.name),
    )?;
    for abi in return_ty.abi_types() {
        signature.returns.push(AbiParam::new(abi));
    }
    for ty in writeback_types {
        for abi in ty.abi_types() {
            signature.returns.push(AbiParam::new(abi));
        }
    }
    Ok(signature)
}

fn receiver_type(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
) -> std::result::Result<DirectType, String> {
    let mut receiver_ty = None;
    for local in &function.local_types {
        if local.name == "self" {
            receiver_ty = Some(&local.ty);
            break;
        }
    }
    let Some(receiver_ty) = receiver_ty else {
        return Err(format!(
            "direct backend could not find receiver local type for `{}`",
            function.name
        ));
    };
    ensure_direct_type(
        receiver_ty,
        classes,
        &format!("receiver of `{}`", function.name),
    )
}

#[cfg(test)]
fn cleanup_place_type(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
    place: &str,
    function_return_types: &HashMap<String, DirectType>,
) -> std::result::Result<DirectType, String> {
    let reachable = reachable_direct_block_labels(function)?;
    cleanup_place_type_in_reachable(function, classes, place, function_return_types, &reachable)
}

fn cleanup_place_type_in_reachable(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
    place: &str,
    function_return_types: &HashMap<String, DirectType>,
    reachable: &HashSet<String>,
) -> std::result::Result<DirectType, String> {
    let mut root_types = HashMap::new();
    if function.receiver.is_some() {
        if let Some(local) = function
            .local_types
            .iter()
            .find(|local| local.name == "self")
        {
            root_types.insert(
                "self".to_string(),
                ensure_direct_type(&local.ty, classes, "cleanup receiver")?,
            );
        }
    }
    for param in &function.params {
        root_types.insert(
            param.name.clone(),
            ensure_direct_type(
                &param.ty,
                classes,
                &format!("cleanup parameter `{}`", param.name),
            )?,
        );
    }
    for local in &function.local_types {
        root_types
            .entry(local.name.clone())
            .or_insert(ensure_direct_type(
                &local.ty,
                classes,
                &format!("cleanup local `{}`", local.name),
            )?);
    }
    for block in &function.blocks {
        if !reachable.contains(&block.label) {
            continue;
        }
        for instruction in &block.instructions {
            let Instruction::Assign { target, value } = instruction else {
                continue;
            };
            if target.contains('.') || root_types.contains_key(target) {
                continue;
            }
            if let Some(ty) = infer_rvalue_type(value, &root_types, function_return_types, classes)
            {
                root_types.insert(target.clone(), ty);
            }
        }
    }

    let mut segments = place.split('.');
    let root = segments
        .next()
        .ok_or("direct backend encountered an empty cleanup place".to_string())?;
    let mut ty = root_types.get(root).cloned().ok_or({
        format!(
            "direct backend does not know cleanup place `{}` in `{}`",
            place, function.name
        )
    })?;
    for field in segments {
        ty = direct_field_type(&ty, field, classes).ok_or({
            format!(
                "direct backend does not know cleanup field `{}` on `{}`",
                field,
                render_direct_type(&ty)
            )
        })?;
    }
    Ok(ty)
}

fn declare_root_variables(
    builder: &mut FunctionBuilder<'_>,
    variable_index: &mut usize,
    variables: &mut HashMap<String, Vec<Variable>>,
    variable_types: &mut HashMap<String, DirectType>,
    name: String,
    ty: DirectType,
    initial: Option<&[Value]>,
) {
    let initial_values = if let Some(values) = initial {
        values.to_vec()
    } else {
        ty.zero_values(builder)
    };
    let abi_types = ty.abi_types();
    let mut declared = Vec::new();
    for (offset, abi_ty) in abi_types.into_iter().enumerate() {
        let variable = Variable::from_u32(*variable_index as u32);
        *variable_index += 1;
        builder.declare_var(variable, abi_ty);
        builder.def_var(variable, initial_values[offset]);
        declared.push(variable);
    }
    variables.insert(name.clone(), declared);
    variable_types.insert(name, ty);
}

fn direct_type_contains_unknown(ty: &DirectType) -> bool {
    fn type_contains_unknown(ty: &Type) -> bool {
        match ty {
            Type::Named(name, args) => name == "Unknown" || args.iter().any(type_contains_unknown),
            Type::Tuple(elements) => elements.iter().any(type_contains_unknown),
            Type::Function {
                params,
                return_type,
                ..
            } => {
                params.iter().any(|param| type_contains_unknown(&param.ty))
                    || type_contains_unknown(return_type.as_ref())
            }
            Type::Closure {
                params,
                return_type,
                captures,
                ..
            } => {
                params.iter().any(|param| type_contains_unknown(&param.ty))
                    || captures
                        .iter()
                        .any(|capture| type_contains_unknown(&capture.ty))
                    || type_contains_unknown(return_type.as_ref())
            }
            Type::TypeParam(_) | Type::Module(_) | Type::Unit => false,
        }
    }

    match ty {
        DirectType::Scalar(_) => false,
        DirectType::PlainClass(class) => class
            .fields
            .iter()
            .any(|field| direct_type_contains_unknown(&field.ty)),
        DirectType::Opaque(ty) => type_contains_unknown(ty),
    }
}

fn collect_cleanup_places(function: &MirFunction, reachable: &HashSet<String>) -> Vec<String> {
    let mut cleanup_places = Vec::<String>::new();
    for block in &function.blocks {
        if !reachable.contains(&block.label) {
            continue;
        }
        for instruction in &block.instructions {
            let Instruction::PushCleanup { place } = instruction else {
                continue;
            };
            if !cleanup_places.contains(place) {
                cleanup_places.push(place.clone());
            }
        }
    }
    cleanup_places
}

fn direct_place_paths_overlap(left: &str, right: &str) -> bool {
    let left_segments = left.split('.').collect::<Vec<_>>();
    let right_segments = right.split('.').collect::<Vec<_>>();
    if left_segments.first() != right_segments.first() {
        return false;
    }
    let shared = left_segments
        .iter()
        .zip(right_segments.iter())
        .take_while(|(lhs, rhs)| lhs == rhs)
        .count();
    shared == left_segments.len() || shared == right_segments.len()
}

fn validate_module(
    module: &MirModule,
) -> std::result::Result<HashMap<String, HashSet<String>>, String> {
    crate::mir::validate_loan_flow(module)?;
    let mut classes = HashMap::new();
    for class in &module.classes {
        classes.insert(class.name.clone(), class.clone());
    }
    for class in &module.classes {
        for field in &class.fields {
            ensure_direct_type(
                &field.ty,
                &classes,
                &format!("field `{}.{}`", class.name, field.name),
            )?;
        }
    }
    let mut reachable_by_function = HashMap::new();
    for function in module.functions.iter().chain(module.top_level.iter()) {
        let reachable = reachable_direct_block_labels(function)?;
        validate_function_in_reachable(function, &classes, &reachable)?;
        reachable_by_function.insert(function.name.clone(), reachable);
    }
    Ok(reachable_by_function)
}

#[cfg(test)]
fn validate_function(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
) -> std::result::Result<(), String> {
    let reachable = reachable_direct_block_labels(function)?;
    validate_function_in_reachable(function, classes, &reachable)
}

fn validate_function_in_reachable(
    function: &MirFunction,
    classes: &HashMap<String, MirClass>,
    reachable: &HashSet<String>,
) -> std::result::Result<(), String> {
    if function.receiver.is_some() {
        receiver_type(function, classes)?;
    }
    for param in &function.params {
        ensure_direct_type(
            &param.ty,
            classes,
            &format!("parameter `{}` on `{}`", param.name, function.name),
        )?;
    }
    ensure_direct_type(
        &function.return_type,
        classes,
        &format!("return type of `{}`", function.name),
    )?;
    for local in &function.local_types {
        ensure_direct_type(
            &local.ty,
            classes,
            &format!("local `{}` on `{}`", local.name, function.name),
        )?;
    }
    for block in &function.blocks {
        if !reachable.contains(&block.label) {
            continue;
        }
        for instruction in &block.instructions {
            match instruction {
                Instruction::Safepoint => {}
                Instruction::BeginLoan { .. }
                | Instruction::BeginReturnedLoan { .. }
                | Instruction::Reborrow { .. }
                | Instruction::ReadLoan { .. }
                | Instruction::EndLoan { .. }
                | Instruction::ReturnLoan { .. } => {}
                Instruction::WriteLoan { value, .. } => validate_rvalue(value, classes)?,
                Instruction::Assign { value, .. } => validate_rvalue(value, classes)?,
                Instruction::Eval { value } => validate_operand(value)?,
                Instruction::PushCleanup { .. } | Instruction::PopCleanup { .. } => {}
            }
        }
        match &block.terminator {
            Terminator::Return(operand) => validate_operand(operand)?,
            Terminator::Goto(_) => {}
            Terminator::Branch { condition, .. } => {
                validate_non_consuming_operand(condition, "a branch condition")?
            }
            Terminator::ForRange { iterable, .. } => validate_operand(iterable)?,
            Terminator::Match { scrutinee, .. } => validate_operand(scrutinee)?,
            Terminator::AssertFail {
                message, captures, ..
            } => {
                if let Some(message) = message {
                    validate_operand(message)?;
                }
                if !captures.is_empty() && captures.len() != 2 {
                    return Err(
                        "direct backend requires exactly two assertion captures when captures are present"
                            .to_string(),
                    );
                }
                for capture in captures {
                    validate_operand(&capture.value)?;
                }
            }
            other => {
                return Err(format!(
                    "direct backend does not yet support MIR terminator `{:?}`",
                    other
                ))
            }
        }
    }
    Ok(())
}

fn validate_rvalue(
    rvalue: &Rvalue,
    classes: &HashMap<String, MirClass>,
) -> std::result::Result<(), String> {
    match rvalue {
        Rvalue::Use(operand) => validate_operand(operand),
        Rvalue::ModuleConstant { .. } => Ok(()),
        Rvalue::Closure {
            signature,
            captures,
            ..
        } => {
            ensure_direct_type(signature, classes, "closure signature")?;
            for capture in captures.iter() {
                validate_operand(&capture.value)?;
                ensure_direct_type(&capture.ty, classes, "closure capture")?;
            }
            Ok(())
        }
        Rvalue::FormatString { parts } => {
            for part in parts {
                match part {
                    MirFormatPart::Value(value) | MirFormatPart::Formatted { value, .. } => {
                        validate_non_consuming_operand(value, "format-string interpolation")?;
                    }
                    MirFormatPart::Literal(_) => {}
                }
            }
            Ok(())
        }
        Rvalue::Unary { value, .. } => validate_non_consuming_operand(value, "a unary expression"),
        Rvalue::Cast { value, ty, .. } => {
            validate_non_consuming_operand(value, "a cast expression")?;
            ensure_direct_type(ty, classes, "cast target")?;
            Ok(())
        }
        Rvalue::Binary { left, right, .. } => {
            validate_non_consuming_operand(left, "the left side of a binary expression")?;
            validate_non_consuming_operand(right, "the right side of a binary expression")
        }
        Rvalue::Call { callee, args } => {
            if matches!(callee, CallTarget::Member { field, .. } if field == "__slice") {
                direct_internal_slice_args(args)?;
            }
            match callee {
                CallTarget::Name(_) => {}
                CallTarget::Value(function) => {
                    validate_non_consuming_operand(function, "an indirect-call target")?
                }
                CallTarget::Extern(call) => {
                    for param in &call.params {
                        ensure_direct_type(&param.ty, classes, "extern parameter")?;
                        direct_ffi_type_for_source(&param.ty, Some(param.passing))?;
                    }
                    ensure_direct_type(&call.return_type, classes, "extern return")?;
                    direct_ffi_type_for_source(&call.return_type, None)?;
                }
                CallTarget::Member { object, .. } | CallTarget::TraitMember { object, .. } => {
                    validate_operand(object)?
                }
            }
            for argument in args {
                validate_operand(&argument.value)?;
            }
            Ok(())
        }
        Rvalue::VecLiteral {
            elements,
            element_type,
        } => {
            ensure_direct_type(element_type, classes, "Vec element")?;
            for element in elements {
                validate_operand(element)?;
            }
            Ok(())
        }
        Rvalue::TupleLiteral {
            elements,
            element_types,
        } => {
            if elements.len() != element_types.len() {
                return Err(format!(
                    "direct backend received tuple literal arity {} with {} element types",
                    elements.len(),
                    element_types.len()
                ));
            }
            for (element, element_type) in elements.iter().zip(element_types) {
                ensure_direct_type(element_type, classes, "tuple element")?;
                validate_operand(element)?;
            }
            Ok(())
        }
        Rvalue::TupleElement {
            tuple,
            element_type,
            ..
        } => {
            validate_tuple_projection_operand(tuple)?;
            ensure_direct_type(element_type, classes, "tuple element")?;
            Ok(())
        }
        Rvalue::TupleTakeElement {
            place,
            element_type,
            ..
        } => {
            validate_tuple_take_place(place)?;
            ensure_direct_type(element_type, classes, "tuple element")?;
            Ok(())
        }
        Rvalue::MapLiteral {
            entries,
            key_type,
            value_type,
        } => {
            ensure_direct_type(key_type, classes, "dict key")?;
            ensure_direct_type(value_type, classes, "Map value")?;
            for entry in entries {
                validate_operand(&entry.key)?;
                validate_operand(&entry.value)?;
            }
            Ok(())
        }
        Rvalue::SetLiteral {
            elements,
            element_type,
        } => {
            ensure_direct_type(element_type, classes, "set element")?;
            for element in elements {
                validate_operand(element)?;
            }
            Ok(())
        }
        Rvalue::Construct { class_name, fields } => {
            ensure_direct_type(
                &Type::named(class_name),
                classes,
                &format!("class `{}`", class_name),
            )?;
            for field in fields {
                validate_operand(&field.value)?;
            }
            Ok(())
        }
        Rvalue::Member { object, .. } => validate_non_consuming_operand(object, "a field read"),
        Rvalue::EnumVariant { payloads, .. } => {
            for payload in payloads {
                validate_operand(payload)?;
            }
            Ok(())
        }
        Rvalue::VariantPayload { scrutinee, .. } => validate_operand(scrutinee),
        Rvalue::Try { value } => validate_operand(value),
        Rvalue::StartTask {
            stack_size,
            task_group,
            function,
            args,
            ..
        } => {
            if let Some(stack_size) = stack_size {
                validate_non_consuming_operand(stack_size, "a task stack size")?;
            }
            validate_non_consuming_operand(task_group, "a task-group receiver")?;
            validate_non_consuming_operand(function, "a task function value")?;
            for argument in args {
                validate_operand(&argument.value)?;
            }
            Ok(())
        }
    }
}

fn validate_operand(operand: &Operand) -> std::result::Result<(), String> {
    match operand {
        Operand::Place(_)
        | Operand::MovePlace(_)
        | Operand::Function { .. }
        | Operand::Int(_)
        | Operand::Bool(_)
        | Operand::Unit
        | Operand::Float(_)
        | Operand::String(_)
        | Operand::Duration(_) => Ok(()),
    }
}

fn validate_non_consuming_operand(
    operand: &Operand,
    context: &str,
) -> std::result::Result<(), String> {
    validate_operand(operand)?;
    if matches!(operand, Operand::MovePlace(_)) {
        return Err(format!(
            "direct backend only permits `MovePlace` in consuming contexts, not in {context}"
        ));
    }
    Ok(())
}

fn validate_tuple_projection_operand(operand: &Operand) -> std::result::Result<(), String> {
    validate_non_consuming_operand(operand, "tuple indexed access").map_err(|_| {
        "direct backend refuses consuming tuple projection; indexed access only reads Copy elements"
            .to_string()
    })
}

fn validate_tuple_take_place(place: &str) -> std::result::Result<(), String> {
    if place.starts_with("%t") {
        return Ok(());
    }
    Err(
        "destructive tuple extraction is internal to whole-tuple destructuring and requires a private captured temporary"
            .to_string(),
    )
}

fn ensure_direct_type(
    ty: &Type,
    classes: &HashMap<String, MirClass>,
    context: &str,
) -> std::result::Result<DirectType, String> {
    direct_type(ty, classes).ok_or({
        format!(
            "direct backend does not yet support {} with type `{}`",
            context, ty
        )
    })
}

fn direct_ffi_type_for_source(
    ty: &Type,
    passing: Option<MirReceiverKind>,
) -> std::result::Result<DirectFfiType, String> {
    let Type::Named(name, args) = ty else {
        if *ty == Type::Unit && passing.is_none() {
            return Ok(DirectFfiType::scalar(FfiType::Unit));
        }
        return Err(format!("direct backend cannot lower `{ty}` through FFI v0"));
    };
    if args.is_empty() {
        let scalar = match name.as_str() {
            "bool" => Some(FfiType::Bool),
            "int8" => Some(FfiType::I8),
            "int16" => Some(FfiType::I16),
            "int32" => Some(FfiType::I32),
            "int" | "int64" => Some(FfiType::I64),
            "uint8" => Some(FfiType::U8),
            "uint16" => Some(FfiType::U16),
            "uint32" => Some(FfiType::U32),
            "uint64" => Some(FfiType::U64),
            "float32" => Some(FfiType::F32),
            "float64" => Some(FfiType::F64),
            "str" if passing == Some(MirReceiverKind::Borrow) => Some(FfiType::StringView),
            _ => None,
        };
        if let Some(ffi_type) = scalar {
            if ffi_type != FfiType::StringView {
                if let Some(passing) = passing {
                    if passing != MirReceiverKind::Borrow {
                        return Err(format!(
                            "direct backend cannot pass `{ty}` with ownership mode `{passing:?}` through FFI v0"
                        ));
                    }
                }
            }
            return Ok(DirectFfiType::scalar(ffi_type));
        }
        if name == "str" {
            return Err(
                "direct backend can pass `str` through FFI v0 only as a shared view".to_string(),
            );
        }
        if passing == Some(MirReceiverKind::BorrowMut) {
            return Err(format!(
                "direct backend cannot mutably borrow opaque handle `{ty}` through FFI v0"
            ));
        }
        return Ok(DirectFfiType::opaque(name.clone()));
    }
    if name == "list" && args.as_slice() == [Type::named("uint8")] {
        return Ok(DirectFfiType::scalar(match passing {
            Some(MirReceiverKind::Borrow) => FfiType::BytesView,
            Some(MirReceiverKind::BorrowMut) => FfiType::BytesViewMut,
            Some(MirReceiverKind::Value) => {
                return Err(
                    "direct backend cannot pass `own list[uint8]` through FFI v0".to_string(),
                )
            }
            None => {
                return Err("direct backend cannot return `list[uint8]` through FFI v0".to_string())
            }
        }));
    }
    Err(format!("direct backend cannot lower `{ty}` through FFI v0"))
}

fn direct_type(ty: &Type, classes: &HashMap<String, MirClass>) -> Option<DirectType> {
    let mut visiting = BTreeSet::new();
    direct_type_inner(ty, classes, &mut visiting)
}

fn direct_type_inner(
    ty: &Type,
    classes: &HashMap<String, MirClass>,
    visiting: &mut BTreeSet<String>,
) -> Option<DirectType> {
    match ty {
        Type::Unit => Some(DirectType::Scalar(ScalarKind::Unit)),
        Type::TypeParam(name) => Some(DirectType::Opaque(Type::TypeParam(name.clone()))),
        Type::Module(path) => Some(DirectType::Opaque(Type::Module(path.clone()))),
        Type::Tuple(elements) => {
            for element in elements {
                direct_type_inner(element, classes, visiting)?;
            }
            Some(DirectType::Opaque(Type::Tuple(elements.clone())))
        }
        Type::Function { .. } | Type::Closure { .. } => Some(DirectType::Opaque(ty.clone())),
        Type::Named(name, args) if args.is_empty() && name == "int32" => {
            Some(DirectType::Scalar(ScalarKind::Int32))
        }
        Type::Named(name, args) if args.is_empty() && name == "int64" => {
            Some(DirectType::Scalar(ScalarKind::Int64))
        }
        Type::Named(name, args) if args.is_empty() && name == "uint64" => {
            Some(DirectType::Scalar(ScalarKind::Uint64))
        }
        Type::Named(name, args) if args.is_empty() && name == "bool" => {
            Some(DirectType::Scalar(ScalarKind::Bool))
        }
        Type::Named(name, args) if args.is_empty() && name == "float32" => {
            Some(DirectType::Scalar(ScalarKind::Float32))
        }
        Type::Named(name, args) if args.is_empty() && name == "float64" => {
            Some(DirectType::Scalar(ScalarKind::Float64))
        }
        Type::Named(name, args) if args.is_empty() && name == "random.Rng" => {
            Some(DirectType::Opaque(Type::named(name)))
        }
        Type::Named(name, args) if args.is_empty() => {
            if let Some(class) = classes.get(name) {
                if !visiting.insert(name.clone()) {
                    return Some(DirectType::Opaque(Type::Named(name.clone(), vec![])));
                }
                let mut fields = Vec::new();
                for field in &class.fields {
                    let Some(field_ty) = direct_type_inner(&field.ty, classes, visiting) else {
                        visiting.remove(name);
                        return Some(DirectType::Opaque(Type::Named(name.clone(), vec![])));
                    };
                    if matches!(field_ty, DirectType::Opaque(_)) {
                        visiting.remove(name);
                        return Some(DirectType::Opaque(Type::Named(name.clone(), vec![])));
                    }
                    fields.push(PlainClassField {
                        name: field.name.clone(),
                        ty: field_ty,
                    });
                }
                visiting.remove(name);
                return Some(DirectType::PlainClass(PlainClassType {
                    class_name: name.clone(),
                    fields,
                }));
            }
            Some(DirectType::Opaque(Type::Named(name.clone(), vec![])))
        }
        Type::Named(name, args) => {
            Some(DirectType::Opaque(Type::Named(name.clone(), args.clone())))
        }
    }
}

fn collect_type_params_from_type(ty: &Type, collected: &mut BTreeSet<String>) {
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

fn host_builtin_return_type(name: &str) -> Option<Type> {
    host_builtin_metadata(name).map(|metadata| metadata.return_type.clone())
}
fn infer_rvalue_type(
    rvalue: &Rvalue,
    variable_types: &HashMap<String, DirectType>,
    function_return_types: &HashMap<String, DirectType>,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    match rvalue {
        Rvalue::Use(operand) => infer_operand_type(operand, variable_types, classes),
        Rvalue::ModuleConstant { .. } => None,
        Rvalue::Closure { signature, .. } => Some(DirectType::Opaque(signature.clone())),
        Rvalue::FormatString { .. } => Some(DirectType::Opaque(Type::named("str"))),
        Rvalue::Unary { op, value, .. } => {
            match (op, infer_operand_type(value, variable_types, classes)?) {
                (UnaryOp::Neg, DirectType::Scalar(kind)) if kind.is_integer() => {
                    Some(DirectType::Scalar(kind))
                }
                (UnaryOp::Neg, DirectType::Scalar(kind)) if kind.is_float() => {
                    Some(DirectType::Scalar(kind))
                }
                (UnaryOp::Not, _) => Some(DirectType::Scalar(ScalarKind::Bool)),
                _ => None,
            }
        }
        Rvalue::Cast { ty, .. } => direct_type(ty, classes),
        Rvalue::Binary {
            op, left, right, ..
        } => match op {
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Less
            | BinaryOp::LessEq
            | BinaryOp::Greater
            | BinaryOp::GreaterEq
            | BinaryOp::And
            | BinaryOp::Or => Some(DirectType::Scalar(ScalarKind::Bool)),
            BinaryOp::Add | BinaryOp::Sub
                if matches!(
                    infer_operand_type(left, variable_types, classes),
                    Some(DirectType::Opaque(Type::Named(ref name, _))) if name == "Duration"
                ) =>
            {
                Some(DirectType::Opaque(Type::named("Duration")))
            }
            BinaryOp::Mul
                if [left, right].iter().any(|operand| {
                    matches!(
                        infer_operand_type(operand, variable_types, classes),
                        Some(DirectType::Opaque(Type::Named(ref name, _))) if name == "Duration"
                    )
                }) =>
            {
                Some(DirectType::Opaque(Type::named("Duration")))
            }
            BinaryOp::FloorDiv
                if matches!(
                    infer_operand_type(left, variable_types, classes),
                    Some(DirectType::Opaque(Type::Named(ref name, _))) if name == "Duration"
                ) =>
            {
                Some(DirectType::Opaque(Type::named("Duration")))
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::FloorDiv
            | BinaryOp::Mod
            | BinaryOp::Pow
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::Shl
            | BinaryOp::Shr => infer_operand_type(left, variable_types, classes),
        },
        Rvalue::Call { callee, args } => match callee {
            CallTarget::Value(function) => {
                let DirectType::Opaque(
                    Type::Function { return_type, .. } | Type::Closure { return_type, .. },
                ) = infer_operand_type(function, variable_types, classes)?
                else {
                    return None;
                };
                direct_type(&return_type, classes)
            }
            CallTarget::Extern(call) => direct_type(&call.return_type, classes),
            CallTarget::Name(name) if name == "print" => Some(DirectType::Scalar(ScalarKind::Unit)),
            CallTarget::Name(name) if name == "random::Rng" => {
                Some(DirectType::Opaque(Type::named("random.Rng")))
            }
            CallTarget::Name(name) if name == "random::secure_int" => {
                Some(DirectType::Scalar(ScalarKind::Int64))
            }
            CallTarget::Name(name) if name == "random::secure_bytes" => Some(DirectType::Opaque(
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
            )),
            CallTarget::Name(name) if name == "range" => {
                Some(DirectType::Opaque(Type::named("Range")))
            }
            CallTarget::Name(name) if name == "Queue" => Some(DirectType::Opaque(Type::Named(
                "Queue".to_string(),
                vec![Type::named("Unknown")],
            ))),
            CallTarget::Name(name) if name == "list" => Some(DirectType::Opaque(Type::Named(
                "list".to_string(),
                vec![Type::named("Unknown")],
            ))),
            CallTarget::Name(name) if name == "set" => Some(DirectType::Opaque(Type::Named(
                "set".to_string(),
                vec![Type::named("Unknown")],
            ))),
            CallTarget::Name(name) if name == "dict" => Some(DirectType::Opaque(Type::Named(
                "dict".to_string(),
                vec![Type::named("Unknown"), Type::named("Unknown")],
            ))),
            CallTarget::Name(name) if name == "TaskGroup" => {
                Some(DirectType::Opaque(Type::named("TaskGroup")))
            }
            CallTarget::Name(name) if name == "cancelled" => {
                Some(DirectType::Scalar(ScalarKind::Bool))
            }
            CallTarget::Name(name) if name == "yield_now" => {
                Some(DirectType::Scalar(ScalarKind::Unit))
            }
            CallTarget::Name(name) if name == "sleep" => Some(DirectType::Scalar(ScalarKind::Unit)),
            CallTarget::Name(name) if host_builtin_return_type(name).is_some() => {
                direct_type(&host_builtin_return_type(name)?, classes)
            }
            CallTarget::Name(name) if name == "select" => {
                let mut queue_payload = Type::Unit;
                let mut task_payload = Type::Unit;
                for argument in args {
                    match infer_operand_type(&argument.value, variable_types, classes) {
                        Some(DirectType::Opaque(Type::Named(source, type_args)))
                            if source == "Queue" =>
                        {
                            queue_payload = type_args.first().cloned().unwrap_or(Type::Unit);
                        }
                        Some(DirectType::Opaque(Type::Named(source, type_args)))
                            if source == "Task" =>
                        {
                            task_payload = type_args.first().cloned().unwrap_or(Type::Unit);
                        }
                        _ => {}
                    }
                }
                Some(DirectType::Opaque(Type::Named(
                    "SelectOutcome".to_string(),
                    vec![queue_payload, task_payload],
                )))
            }
            CallTarget::Name(name) if matches!(name.as_str(), "wait_any" | "wait_all") => {
                let task_payload = args
                    .first()
                    .and_then(|argument| {
                        infer_operand_type(&argument.value, variable_types, classes)
                    })
                    .map(|direct| match direct {
                        DirectType::Opaque(Type::Named(vec_name, args)) if vec_name == "list" => {
                            match args.as_slice() {
                                [Type::Named(task_name, task_args)] if task_name == "Task" => {
                                    task_args.first().cloned().unwrap_or(Type::Unit)
                                }
                                _ => Type::named("Unknown"),
                            }
                        }
                        _ => Type::named("Unknown"),
                    })
                    .unwrap_or(Type::named("Unknown"));
                Some(DirectType::Opaque(Type::Named(
                    if name == "wait_any" {
                        "WaitAny".to_string()
                    } else {
                        "WaitAll".to_string()
                    },
                    vec![task_payload],
                )))
            }
            CallTarget::Name(name) if name == "io::write" || name == "io::flush" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                )))
            }
            CallTarget::Name(name) if name == "io::read_line" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("Option".to_string(), vec![Type::named("str")]),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "fs::exists" => {
                Some(DirectType::Scalar(ScalarKind::Bool))
            }
            CallTarget::Name(name) if name == "fs::read_to_string" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::named("str"),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "fs::read_bytes" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("list".to_string(), vec![Type::named("uint8")]),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "fs::write_string"
                        | "fs::write_bytes"
                        | "fs::append_string"
                        | "fs::append_bytes"
                        | "fs::create_dir"
                        | "fs::remove_file"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                )))
            }
            CallTarget::Name(name) if name == "fs::read_dir" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("list".to_string(), vec![Type::named("str")]),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(name.as_str(), "fs::open" | "fs::create" | "fs::append") =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("fs.File".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "process::inherit" | "process::null" | "process::pipe"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "process.Stdio".to_string(),
                    Vec::new(),
                )))
            }
            CallTarget::Name(name) if name == "process::supervisor" => Some(DirectType::Opaque(
                Type::Named("process.Supervisor".to_string(), Vec::new()),
            )),
            CallTarget::Name(name) if name == "process::start" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("process.Child".to_string(), Vec::new()),
                        Type::Named("process.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "process::run" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("process.Completed".to_string(), Vec::new()),
                        Type::Named("process.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(name.as_str(), "net::connect" | "net::connect_timeout") =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TcpStream".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::listen" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TcpListener".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::udp_bind" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UdpSocket".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::unix_listen" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UnixListener".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "net::unix_connect" | "net::unix_connect_timeout"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.UnixStream".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::tls_listen" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TlsListener".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "net::tls_connect" | "net::tls_connect_timeout"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.TlsStream".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::http_listen" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.HttpListener".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "net::http_request_text"
                        | "net::http_request_text_timeout"
                        | "net::http_request_bytes"
                        | "net::http_request_bytes_timeout"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.HttpResponse".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "net::websocket_listen" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.WebSocketListener".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name)
                if matches!(
                    name.as_str(),
                    "net::websocket_connect" | "net::websocket_connect_timeout"
                ) =>
            {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![
                        Type::Named("net.WebSocket".to_string(), Vec::new()),
                        Type::Named("io.Error".to_string(), Vec::new()),
                    ],
                )))
            }
            CallTarget::Name(name) if name == "parse_int32" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![Type::named("int32"), Type::named("str")],
                )))
            }
            CallTarget::Name(name) if name == "parse_int64" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![Type::named("int64"), Type::named("str")],
                )))
            }
            CallTarget::Name(name) if name == "parse_float64" => {
                Some(DirectType::Opaque(Type::Named(
                    "Result".to_string(),
                    vec![Type::named("float64"), Type::named("str")],
                )))
            }
            CallTarget::Name(name) if name == "round" => {
                let operand = args.first().and_then(|argument| {
                    infer_operand_type(&argument.value, variable_types, classes)
                })?;
                Some(match operand.scalar_kind() {
                    Some(ScalarKind::Float32 | ScalarKind::Float64) => {
                        DirectType::Scalar(ScalarKind::Int64)
                    }
                    _ => operand,
                })
            }
            CallTarget::Name(name) if name == "divmod" => {
                let operand = args.first().and_then(|argument| {
                    infer_operand_type(&argument.value, variable_types, classes)
                })?;
                let operand = direct_type_to_type(&operand);
                Some(DirectType::Opaque(Type::Tuple(vec![
                    operand.clone(),
                    operand,
                ])))
            }
            CallTarget::Name(name)
                if matches!(
                    name.strip_prefix("Duration."),
                    Some(field)
                        if BuiltinAssociatedFunction::resolve("Duration", field).is_some()
                ) =>
            {
                Some(DirectType::Opaque(Type::named("Duration")))
            }
            CallTarget::Name(name) => function_return_types.get(name).cloned(),
            CallTarget::Member { object, field, .. }
            | CallTarget::TraitMember { object, field, .. } => {
                let object_ty = infer_operand_type(object, variable_types, classes)?;
                if matches!(object_ty.scalar_kind(), Some(kind) if kind.is_float())
                    && field == "sqrt"
                {
                    return Some(object_ty);
                }
                if object_ty.scalar_kind().is_some() && field == "to_string" {
                    return Some(DirectType::Opaque(Type::named("str")));
                }
                if matches!(object_ty.scalar_kind(), Some(kind) if kind.is_integer())
                    && field == "to_float"
                {
                    return Some(DirectType::Scalar(ScalarKind::Float64));
                }
                if matches!(object_ty.scalar_kind(), Some(kind) if kind.is_integer())
                    && matches!(
                        field.as_str(),
                        "wrapping_add"
                            | "wrapping_sub"
                            | "wrapping_mul"
                            | "saturating_add"
                            | "saturating_sub"
                            | "saturating_mul"
                            | "wrapping_shl"
                            | "wrapping_shr"
                            | "saturating_shl"
                            | "saturating_shr"
                    )
                {
                    return Some(object_ty);
                }
                if object_ty.scalar_kind().is_some()
                    && matches!(field.as_str(), "add" | "sub" | "mul" | "div")
                {
                    if let Some(argument) = args.first() {
                        if let Some(array_ty) =
                            infer_operand_type(&argument.value, variable_types, classes)
                        {
                            if direct_array_element_type(&array_ty).is_some() {
                                return Some(array_ty);
                            }
                        }
                    }
                }
                match object_ty {
                    DirectType::PlainClass(class_ty) => {
                        let method = find_method(classes.get(&class_ty.class_name), field)?;
                        function_return_types.get(&method.function_name).cloned()
                    }
                    DirectType::Opaque(Type::Named(name, args)) => {
                        if let Some(method) = find_method(classes.get(&name), field) {
                            return function_return_types.get(&method.function_name).cloned();
                        }
                        builtin_opaque_member_return_type(&Type::Named(name, args), field, classes)
                            .or_else(|| Some(DirectType::Opaque(Type::named("Unknown"))))
                    }
                    DirectType::Opaque(ty) => {
                        builtin_opaque_member_return_type(&ty, field, classes)
                            .or_else(|| Some(DirectType::Opaque(Type::named("Unknown"))))
                    }
                    DirectType::Scalar(_) => None,
                }
            }
        },
        Rvalue::VecLiteral { element_type, .. } => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![element_type.clone()],
        ))),
        Rvalue::TupleLiteral { element_types, .. } => {
            Some(DirectType::Opaque(Type::Tuple(element_types.clone())))
        }
        Rvalue::TupleElement { element_type, .. }
        | Rvalue::TupleTakeElement { element_type, .. } => direct_type(element_type, classes),
        Rvalue::MapLiteral {
            key_type,
            value_type,
            ..
        } => Some(DirectType::Opaque(Type::Named(
            "dict".to_string(),
            vec![key_type.clone(), value_type.clone()],
        ))),
        Rvalue::SetLiteral { element_type, .. } => Some(DirectType::Opaque(Type::Named(
            "set".to_string(),
            vec![element_type.clone()],
        ))),
        Rvalue::Construct { class_name, .. } => direct_type(&Type::named(class_name), classes),
        Rvalue::Member { object, field } => {
            let ty = infer_operand_type(object, variable_types, classes)?;
            direct_field_type(&ty, field, classes)
        }
        Rvalue::EnumVariant { enum_name, .. } => Some(DirectType::Opaque(Type::named(enum_name))),
        Rvalue::VariantPayload {
            scrutinee,
            variant_name,
            index,
        } => infer_variant_payload_type(scrutinee, variant_name, *index, variable_types, classes)
            .or_else(|| Some(DirectType::Opaque(Type::named("Unknown")))),
        Rvalue::Try { value } => infer_try_type(value, variable_types, classes)
            .or_else(|| Some(DirectType::Opaque(Type::named("Unknown")))),
        Rvalue::StartTask {
            returns_handle,
            function,
            ..
        } => {
            if *returns_handle {
                infer_operand_type(function, variable_types, classes).and_then(|ty| match ty {
                    DirectType::Opaque(
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. },
                    ) => Some(DirectType::Opaque(Type::Named(
                        "Task".to_string(),
                        vec![*return_type],
                    ))),
                    _ => None,
                })
            } else {
                Some(DirectType::Scalar(ScalarKind::Unit))
            }
        }
    }
}

fn builtin_opaque_member_return_type(
    object_ty: &Type,
    field: &str,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    let Type::Named(name, args) = object_ty else {
        return None;
    };
    if args.is_empty()
        && matches!(
            BuiltinMember::resolve(name, field),
            Some(BuiltinMember::DurationToMilliseconds | BuiltinMember::DurationToSeconds)
        )
    {
        return Some(DirectType::Scalar(ScalarKind::Float64));
    }
    if args.is_empty() && field == "to_float" && is_fixed_width_integer_type(object_ty) {
        return Some(DirectType::Scalar(ScalarKind::Float64));
    }
    if args.is_empty()
        && is_fixed_width_integer_type(object_ty)
        && matches!(
            field,
            "wrapping_add"
                | "wrapping_sub"
                | "wrapping_mul"
                | "saturating_add"
                | "saturating_sub"
                | "saturating_mul"
                | "wrapping_shl"
                | "wrapping_shr"
                | "saturating_shl"
                | "saturating_shr"
        )
    {
        return direct_type(object_ty, classes);
    }
    if args.is_empty()
        && field == "to_string"
        && matches!(
            name.as_str(),
            "bool"
                | "int8"
                | "int16"
                | "int32"
                | "int64"
                | "int128"
                | "intsize"
                | "uint8"
                | "uint16"
                | "uint32"
                | "uint64"
                | "uint128"
                | "uintsize"
                | "float32"
                | "float64"
        )
    {
        return Some(DirectType::Opaque(Type::named("str")));
    }
    match (name.as_str(), field) {
        ("random.Rng", "next_int") => Some(DirectType::Scalar(ScalarKind::Int64)),
        ("random.Rng", "next_float") => Some(DirectType::Scalar(ScalarKind::Float64)),
        ("random.Rng", "shuffle") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("str", "len") | ("str", "byte_len") => direct_type(&Type::named("int64"), classes),
        ("str", "contains") | ("str", "starts_with") | ("str", "ends_with") => {
            Some(DirectType::Scalar(ScalarKind::Bool))
        }
        ("str", "split") => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("str")],
        ))),
        ("str", "replace")
        | ("str", "add")
        | ("str", "to_lower")
        | ("str", "to_upper")
        | ("str", "trim")
        | ("str", "__slice")
        | ("str", "clone") => Some(DirectType::Opaque(Type::named("str"))),
        ("str", "to_bytes") => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("uint8")],
        ))),
        ("str", "join") => Some(DirectType::Opaque(Type::named("str"))),
        ("str", "strip_prefix") | ("str", "strip_suffix") => Some(DirectType::Opaque(Type::Named(
            "Option".to_string(),
            vec![Type::named("str")],
        ))),
        ("Array", "shape") => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            vec![Type::named("int64")],
        ))),
        ("Array", "len") => Some(DirectType::Scalar(ScalarKind::Int64)),
        ("Array", "clone")
        | ("Array", "__slice")
        | ("Array", "wrapping_add")
        | ("Array", "wrapping_sub")
        | ("Array", "wrapping_mul")
        | ("Array", "saturating_add")
        | ("Array", "saturating_sub")
        | ("Array", "saturating_mul") => Some(DirectType::Opaque(Type::Named(
            "Array".to_string(),
            args.clone(),
        ))),
        ("Array", "get") | ("Array", "set") => direct_type(
            &Type::Named(
                "Option".to_string(),
                vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
            ),
            classes,
        ),
        ("Array", "fill") | ("Array", "__set_index") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("Array", "__index") | ("Array", "sum") | ("Array", "min") | ("Array", "max") => {
            direct_type(args.first().unwrap_or(&Type::named("Unknown")), classes)
        }
        ("Array", "mean") => Some(DirectType::Scalar(ScalarKind::Float64)),
        ("Array", "map") => Some(DirectType::Opaque(Type::Named(
            "Array".to_string(),
            vec![Type::named("Unknown")],
        ))),
        ("list", "len") => direct_type(&Type::named("int64"), classes),
        ("list", "is_empty") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("list", "copy") => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            args.clone(),
        ))),
        ("list", "__slice") => Some(DirectType::Opaque(Type::Named(
            "list".to_string(),
            args.clone(),
        ))),
        ("list", "append")
        | ("list", "extend")
        | ("list", "clear")
        | ("list", "reverse")
        | ("list", "swap")
        | ("list", "insert")
        | ("list", "remove")
        | ("list", "reserve")
        | ("list", "__set_index") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("list", "contains") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("list", "get") | ("list", "__index_option") | ("list", "__take_index_option") => {
            direct_type(
                &Type::Named(
                    "Option".to_string(),
                    vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
                ),
                classes,
            )
        }
        ("list", "pop") | ("list", "set") | ("list", "__index") => {
            direct_type(args.first().unwrap_or(&Type::named("Unknown")), classes)
        }
        ("list", "index") | ("list", "count") => direct_type(&Type::named("int64"), classes),
        ("dict", "len") => direct_type(&Type::named("int64"), classes),
        ("dict", "is_empty") | ("dict", "contains_key") => {
            Some(DirectType::Scalar(ScalarKind::Bool))
        }
        ("dict", "copy") => Some(DirectType::Opaque(Type::Named(
            "dict".to_string(),
            args.clone(),
        ))),
        ("dict", "get") | ("dict", "set") | ("dict", "remove") => direct_type(
            &Type::Named(
                "Option".to_string(),
                vec![args.get(1).cloned().unwrap_or(Type::named("Unknown"))],
            ),
            classes,
        ),
        ("dict", "keys") => direct_type(
            &Type::Named(
                "list".to_string(),
                vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
            ),
            classes,
        ),
        ("dict", "values") => direct_type(
            &Type::Named(
                "list".to_string(),
                vec![args.get(1).cloned().unwrap_or(Type::named("Unknown"))],
            ),
            classes,
        ),
        ("dict", "items") => direct_type(
            &Type::Named(
                "list".to_string(),
                vec![Type::Tuple(vec![
                    args.first().cloned().unwrap_or(Type::named("Unknown")),
                    args.get(1).cloned().unwrap_or(Type::named("Unknown")),
                ])],
            ),
            classes,
        ),
        ("dict", "clear") | ("dict", "update") | ("dict", "reserve") => {
            Some(DirectType::Scalar(ScalarKind::Unit))
        }
        ("dict", "__index") => direct_type(args.get(1).unwrap_or(&Type::named("Unknown")), classes),
        ("dict", "__set_index") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("set", "len") => direct_type(&Type::named("int64"), classes),
        ("set", "is_empty") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("set", "copy") => Some(DirectType::Opaque(Type::Named(
            "set".to_string(),
            args.clone(),
        ))),
        ("set", "contains") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("set", "add")
        | ("set", "remove")
        | ("set", "discard")
        | ("set", "clear")
        | ("set", "reserve") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("set", "__index_option") | ("set", "__take_index_option") => direct_type(
            &Type::Named(
                "Option".to_string(),
                vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
            ),
            classes,
        ),
        ("Queue", "put") | ("Queue", "try_put") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named(
                        "SendError".to_string(),
                        vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
                    ),
                ],
            ),
            classes,
        ),
        ("Queue", "get" | "__get_in_task_group" | "__get_with_registered_producers") => {
            direct_type(
                &Type::Named(
                    "QueueReceive".to_string(),
                    vec![args.first().cloned().unwrap_or(Type::named("Unknown"))],
                ),
                classes,
            )
        }
        ("Queue", "close") | ("TaskGroup", "cancel") | ("TaskGroup", "close") => {
            Some(DirectType::Scalar(ScalarKind::Unit))
        }
        ("Task", "result") => direct_type(
            &Type::Named(
                "TaskResult".to_string(),
                vec![args.first().cloned().unwrap_or(Type::Unit)],
            ),
            classes,
        ),
        ("fs.File", "read_all") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("fs.File", "read_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("fs.File", "write_all") | ("fs.File", "write_bytes") | ("fs.File", "flush") => {
            direct_type(
                &Type::Named(
                    "Result".to_string(),
                    vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                ),
                classes,
            )
        }
        ("fs.File", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("process.Child", "stdin") | ("process.Child", "stdout") | ("process.Child", "stderr") => {
            direct_type(
                &Type::Named(
                    "Option".to_string(),
                    vec![Type::Named("process.Pipe".to_string(), Vec::new())],
                ),
                classes,
            )
        }
        ("process.Child", "wait") => direct_type(
            &Type::Named("process.Wait".to_string(), Vec::new()),
            classes,
        ),
        ("process.Child", "wait_or_none") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("process.ExitStatus".to_string(), Vec::new())],
                    ),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Child", "wait_ok") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("process.ExitStatus".to_string(), Vec::new()),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Child", "kill") | ("process.Child", "terminate") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Child", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("process.Pipe", "read_all") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Pipe", "read_line") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Pipe", "read_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                    ),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Pipe", "write_all")
        | ("process.Pipe", "write_bytes")
        | ("process.Pipe", "flush") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Pipe", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("process.Completed", "status") => direct_type(
            &Type::Named("process.ExitStatus".to_string(), Vec::new()),
            classes,
        ),
        ("process.Completed", "success") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("process.Completed", "stdout") | ("process.Completed", "stderr") => {
            direct_type(&Type::named("str"), classes)
        }
        ("process.Completed", "stdout_bytes") | ("process.Completed", "stderr_bytes") => {
            direct_type(
                &Type::Named("list".to_string(), vec![Type::named("uint8")]),
                classes,
            )
        }
        ("process.Supervisor", "start") | ("process.Supervisor", "stop") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Supervisor", "wait") => direct_type(
            &Type::Named("process.SupervisorWait".to_string(), Vec::new()),
            classes,
        ),
        ("process.Supervisor", "wait_or_none") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named(
                            "process.SupervisorEvent".to_string(),
                            Vec::new(),
                        )],
                    ),
                    Type::Named("process.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("process.Supervisor", "is_empty") => Some(DirectType::Scalar(ScalarKind::Bool)),
        ("process.Supervisor", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.TcpListener", "accept") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("net.TcpStream".to_string(), Vec::new()),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpListener", "local_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpListener", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.TcpStream", "read_all")
        | ("net.TcpStream", "local_addr")
        | ("net.TcpStream", "peer_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpStream", "read_line") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpStream", "read_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                    ),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpStream", "read_exact") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TcpStream", "write_all")
        | ("net.TcpStream", "write_bytes")
        | ("net.TcpStream", "flush")
        | ("net.TcpStream", "shutdown_read")
        | ("net.TcpStream", "shutdown_write")
        | ("net.TcpStream", "shutdown_both") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
            ),
            classes,
        ),
        ("net.TcpStream", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.UdpSocket", "send_text") | ("net.UdpSocket", "send_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
            ),
            classes,
        ),
        ("net.UdpSocket", "recv") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                    ),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UdpSocket", "recv_from") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("net.UdpDatagram".to_string(), Vec::new())],
                    ),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UdpSocket", "local_addr") | ("net.UdpSocket", "peer_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UdpSocket", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.UdpDatagram", "address") => direct_type(&Type::named("str"), classes),
        ("net.UdpDatagram", "bytes") => direct_type(
            &Type::Named("list".to_string(), vec![Type::named("uint8")]),
            classes,
        ),
        ("net.UdpDatagram", "text") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.HttpListener", "accept") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("net.HttpExchange".to_string(), Vec::new()),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.HttpListener", "local_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.HttpListener", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.HttpExchange", "method") | ("net.HttpExchange", "path") => {
            direct_type(&Type::named("str"), classes)
        }
        ("net.HttpExchange", "headers") | ("net.HttpResponse", "headers") => direct_type(
            &Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            ),
            classes,
        ),
        ("net.HttpExchange", "body_text") | ("net.HttpResponse", "text") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.HttpExchange", "body_bytes") | ("net.HttpResponse", "bytes") => direct_type(
            &Type::Named("list".to_string(), vec![Type::named("uint8")]),
            classes,
        ),
        ("net.HttpExchange", "respond_text") | ("net.HttpExchange", "respond_bytes") => {
            direct_type(
                &Type::Named(
                    "Result".to_string(),
                    vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                ),
                classes,
            )
        }
        ("net.HttpExchange", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.HttpResponse", "status") => direct_type(&Type::named("int32"), classes),
        ("net.HttpResponse", "reason") => direct_type(&Type::named("str"), classes),
        ("net.HttpResponse", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.WebSocketListener", "accept") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("net.WebSocket".to_string(), Vec::new()),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.WebSocketListener", "local_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.WebSocketListener", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.WebSocket", "send_text") | ("net.WebSocket", "send_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
            ),
            classes,
        ),
        ("net.WebSocket", "recv_text") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.WebSocket", "recv_bytes") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                    ),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.WebSocket", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.UnixListener", "accept") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("net.UnixStream".to_string(), Vec::new()),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UnixListener", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.UnixStream", "read_line") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UnixStream", "read_exact") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.UnixStream", "write_all") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
            ),
            classes,
        ),
        ("net.UnixStream", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.TlsListener", "accept") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("net.TlsStream".to_string(), Vec::new()),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TlsListener", "local_addr") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::named("str"),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TlsListener", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        ("net.TlsStream", "read_line") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TlsStream", "read_exact") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                    Type::Named("io.Error".to_string(), Vec::new()),
                ],
            ),
            classes,
        ),
        ("net.TlsStream", "write_all") => direct_type(
            &Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
            ),
            classes,
        ),
        ("net.TlsStream", "close") => Some(DirectType::Scalar(ScalarKind::Unit)),
        _ => None,
    }
}

fn infer_variant_payload_type(
    scrutinee: &Operand,
    variant_name: &str,
    index: usize,
    variable_types: &HashMap<String, DirectType>,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    let scrutinee_ty = infer_operand_type(scrutinee, variable_types, classes)?;
    let DirectType::Opaque(Type::Named(name, args)) = scrutinee_ty else {
        return None;
    };
    let payload_ty = match (name.as_str(), args.as_slice(), variant_name, index) {
        ("Option", [inner], "Some", 0) => inner.clone(),
        ("Result", [ok, _], "Ok", 0) => ok.clone(),
        ("Result", [_, err], "Err", 0) => err.clone(),
        ("SendError", [inner], "Closed" | "Cancelled" | "TimedOut" | "Full", 0) => inner.clone(),
        ("QueueReceive", [inner], "Item", 0) => inner.clone(),
        ("TaskResult", [inner], "Ready", 0) => inner.clone(),
        ("TaskResult", [_], "Error", 0) => Type::named("str"),
        ("WaitAny", [_], "Ready" | "Error", 0) => Type::named("int64"),
        ("WaitAny", [inner], "Ready", 1) => inner.clone(),
        ("WaitAny", [_], "Error", 1) => Type::named("str"),
        ("WaitAll", [inner], "Ready", 0) => Type::Named("list".to_string(), vec![inner.clone()]),
        ("WaitAll", [_], "Error", 0) => Type::named("int64"),
        ("WaitAll", [_], "Error", 1) => Type::named("str"),
        ("SelectOutcome", [_, _], "Queue", 0) => Type::named("int64"),
        ("SelectOutcome", [queue, _], "Queue", 1) => {
            Type::Named("QueueReceive".to_string(), vec![queue.clone()])
        }
        ("SelectOutcome", [_, _], "Task", 0) => Type::named("int64"),
        ("SelectOutcome", [_, task], "Task", 1) => {
            Type::Named("TaskResult".to_string(), vec![task.clone()])
        }
        ("SelectOutcome", [_, _], "Deadline", 0) => Type::named("int64"),
        _ => return None,
    };
    direct_type(&payload_ty, classes)
}

fn enum_variant_payload_types_for_target(
    enum_name: &str,
    variant_name: &str,
    target: &DirectType,
    classes: &HashMap<String, MirClass>,
) -> Option<Vec<DirectType>> {
    let DirectType::Opaque(Type::Named(name, args)) = target else {
        return None;
    };
    if name != enum_name {
        return None;
    }
    let payload_types = match (name.as_str(), args.as_slice(), variant_name) {
        ("Option", [inner], "Some") => vec![inner.clone()],
        ("Option", [_], "None") => Vec::new(),
        ("Result", [ok, _], "Ok") => vec![ok.clone()],
        ("Result", [_, err], "Err") => vec![err.clone()],
        ("SendError", [inner], "Closed" | "Cancelled" | "TimedOut" | "Full") => {
            vec![inner.clone()]
        }
        ("QueueReceive", [inner], "Item") => vec![inner.clone()],
        ("QueueReceive", [_], "Closed" | "TimedOut" | "Cancelled") => Vec::new(),
        ("TaskResult", [inner], "Ready") => vec![inner.clone()],
        ("TaskResult", [_], "Error") => vec![Type::named("str")],
        ("TaskResult", [_], "TimedOut" | "Cancelled") => Vec::new(),
        ("WaitAny", [inner], "Ready") => vec![Type::named("int64"), inner.clone()],
        ("WaitAny", [_], "Error") => vec![Type::named("int64"), Type::named("str")],
        ("WaitAny", [_], "TimedOut" | "Cancelled") => Vec::new(),
        ("WaitAll", [inner], "Ready") => vec![Type::Named("list".to_string(), vec![inner.clone()])],
        ("WaitAll", [_], "Error") => vec![Type::named("int64"), Type::named("str")],
        ("WaitAll", [_], "TimedOut" | "Cancelled") => Vec::new(),
        ("SelectOutcome", [queue, _], "Queue") => vec![
            Type::named("int64"),
            Type::Named("QueueReceive".to_string(), vec![queue.clone()]),
        ],
        ("SelectOutcome", [_, task], "Task") => vec![
            Type::named("int64"),
            Type::Named("TaskResult".to_string(), vec![task.clone()]),
        ],
        ("SelectOutcome", [_, _], "Deadline") => vec![Type::named("int64")],
        ("SelectOutcome", [_, _], "Cancelled") => Vec::new(),
        _ => return None,
    };
    Some(
        payload_types
            .iter()
            .map(|payload_ty| {
                direct_type(payload_ty, classes).unwrap_or(DirectType::Opaque(payload_ty.clone()))
            })
            .collect::<Vec<_>>(),
    )
}

fn infer_try_type(
    value: &Operand,
    variable_types: &HashMap<String, DirectType>,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    let value_ty = infer_operand_type(value, variable_types, classes)?;
    let DirectType::Opaque(Type::Named(name, args)) = value_ty else {
        return None;
    };
    match (name.as_str(), args.as_slice()) {
        ("Result", [ok, _]) => direct_type(ok, classes),
        _ => None,
    }
}

fn direct_field_type(
    ty: &DirectType,
    field: &str,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    if let Some((_, _, field_ty)) = ty.field_slice(field) {
        return Some(field_ty);
    }
    if let DirectType::Opaque(Type::Tuple(elements)) = ty {
        let index = field.parse::<usize>().ok()?;
        return direct_type(elements.get(index)?, classes);
    }
    let DirectType::Opaque(Type::Named(class_name, args)) = ty else {
        return None;
    };
    let class = classes.get(class_name)?;
    if args.len() != class.type_params.len() {
        return None;
    }
    let field_info = class
        .fields
        .iter()
        .find(|candidate| candidate.name == field)?;
    let substitutions = class
        .type_params
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect::<HashMap<_, _>>();
    direct_type(&substitute_type(&field_info.ty, &substitutions), classes)
}

fn infer_operand_type(
    operand: &Operand,
    variable_types: &HashMap<String, DirectType>,
    classes: &HashMap<String, MirClass>,
) -> Option<DirectType> {
    match operand {
        Operand::Place(place) | Operand::MovePlace(place) => {
            let mut segments = place.split('.');
            let root = segments.next()?;
            let mut ty = variable_types.get(root)?.clone();
            for field in segments {
                ty = direct_field_type(&ty, field, classes)?;
            }
            Some(ty)
        }
        Operand::Int(value) => {
            if i64::try_from(*value).is_ok() {
                Some(DirectType::Scalar(ScalarKind::Int64))
            } else {
                Some(DirectType::Opaque(Type::named("Unknown")))
            }
        }
        Operand::Float(_) => Some(DirectType::Scalar(ScalarKind::Float64)),
        Operand::Bool(_) => Some(DirectType::Scalar(ScalarKind::Bool)),
        Operand::Unit => Some(DirectType::Scalar(ScalarKind::Unit)),
        Operand::String(_) => Some(DirectType::Opaque(Type::named("str"))),
        Operand::Duration(_) => Some(DirectType::Opaque(Type::named("Duration"))),
        Operand::Function { signature, .. } => Some(DirectType::Opaque(signature.as_ref().clone())),
    }
}

fn render_direct_type(ty: &DirectType) -> String {
    match ty {
        DirectType::Scalar(ScalarKind::Int32) => "int32".to_string(),
        DirectType::Scalar(ScalarKind::Int64) => "int64".to_string(),
        DirectType::Scalar(ScalarKind::Uint64) => "uint64".to_string(),
        DirectType::Scalar(ScalarKind::Float32) => "float32".to_string(),
        DirectType::Scalar(ScalarKind::Float64) => "float64".to_string(),
        DirectType::Scalar(ScalarKind::Bool) => "bool".to_string(),
        DirectType::Scalar(ScalarKind::Unit) => "None".to_string(),
        DirectType::PlainClass(class) => class.class_name.clone(),
        DirectType::Opaque(ty) => ty.to_string(),
    }
}

fn thunk_string_constant(
    codegen: &mut NativeCodegen<'_>,
    builder: &mut FunctionBuilder<'_>,
    bytes: &[u8],
) -> std::result::Result<(Value, Value), String> {
    let id = if let Some(id) = codegen.string_data.get(bytes) {
        *id
    } else {
        let name = format!("aura_data_{}", codegen.string_data.len());
        let id = match codegen
            .object
            .declare_data(&name, Linkage::Local, false, false)
        {
            Ok(id) => id,
            Err(error) => return Err(format!("failed to declare string data: {}", error)),
        };
        let mut data = DataDescription::new();
        data.define(bytes.to_vec().into_boxed_slice());
        if let Err(error) = codegen.object.define_data(id, &data) {
            return Err(format!("failed to define string data: {}", error));
        }
        codegen.string_data.insert(bytes.to_vec(), id);
        id
    };
    let global = codegen.object.declare_data_in_func(id, builder.func);
    let ptr = builder.ins().symbol_value(types::I64, global);
    let len = builder.ins().iconst(types::I64, bytes.len() as i64);
    Ok((ptr, len))
}

fn box_thunk_value(
    codegen: &mut NativeCodegen<'_>,
    builder: &mut FunctionBuilder<'_>,
    values: &[Value],
    ty: &DirectType,
) -> std::result::Result<Value, String> {
    match ty {
        DirectType::Opaque(_) => values.first().copied().ok_or({
            format!(
                "task-start thunk expected an opaque value for `{}`",
                render_direct_type(ty)
            )
        }),
        DirectType::Scalar(ScalarKind::Int32) => {
            let box_i32 = codegen
                .object
                .declare_func_in_func(codegen.box_i32, builder.func);
            let inst = builder.ins().call(box_i32, &[values[0]]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::Scalar(ScalarKind::Int64) => {
            let box_i64 = codegen
                .object
                .declare_func_in_func(codegen.box_i64, builder.func);
            let inst = builder.ins().call(box_i64, &[values[0]]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::Scalar(ScalarKind::Uint64) => {
            let box_u64 = codegen
                .object
                .declare_func_in_func(codegen.box_u64, builder.func);
            let inst = builder.ins().call(box_u64, &[values[0]]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::Scalar(ScalarKind::Float32) | DirectType::Scalar(ScalarKind::Float64) => {
            let box_f64 = codegen
                .object
                .declare_func_in_func(codegen.box_f64, builder.func);
            let inst = builder.ins().call(box_f64, &[values[0]]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::Scalar(ScalarKind::Bool) => {
            let box_bool = codegen
                .object
                .declare_func_in_func(codegen.box_bool, builder.func);
            let inst = builder.ins().call(box_bool, &[values[0]]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::Scalar(ScalarKind::Unit) => {
            let box_unit = codegen
                .object
                .declare_func_in_func(codegen.box_unit, builder.func);
            let inst = builder.ins().call(box_unit, &[]);
            Ok(builder.inst_results(inst)[0])
        }
        DirectType::PlainClass(class) => {
            let instance_empty = codegen
                .object
                .declare_func_in_func(codegen.instance_empty, builder.func);
            let instance_set_field_owned = codegen
                .object
                .declare_func_in_func(codegen.instance_set_field_owned, builder.func);
            let (class_ptr, class_len) =
                thunk_string_constant(codegen, builder, class.class_name.as_bytes())?;
            let init = builder.ins().call(instance_empty, &[class_ptr, class_len]);
            let current = builder.inst_results(init)[0];
            let mut start = 0usize;
            for field in &class.fields {
                let end = start + field.ty.value_count();
                let field_value =
                    box_thunk_value(codegen, builder, &values[start..end], &field.ty)?;
                let (field_ptr, field_len) =
                    thunk_string_constant(codegen, builder, field.name.as_bytes())?;
                builder.ins().call(
                    instance_set_field_owned,
                    &[current, field_ptr, field_len, field_value],
                );
                start = end;
            }
            Ok(current)
        }
    }
}

fn unbox_thunk_value(
    codegen: &mut NativeCodegen<'_>,
    builder: &mut FunctionBuilder<'_>,
    raw: Value,
    ty: &DirectType,
) -> std::result::Result<Vec<Value>, String> {
    match ty {
        DirectType::Opaque(_) => Ok(vec![raw]),
        DirectType::Scalar(ScalarKind::Int32) => {
            let unbox_i64 = codegen
                .object
                .declare_func_in_func(codegen.unbox_i64, builder.func);
            let inst = builder.ins().call(unbox_i64, &[raw]);
            Ok(builder.inst_results(inst).to_vec())
        }
        DirectType::Scalar(ScalarKind::Int64) => {
            let unbox_int64 = codegen
                .object
                .declare_func_in_func(codegen.unbox_int64, builder.func);
            let inst = builder.ins().call(unbox_int64, &[raw]);
            Ok(builder.inst_results(inst).to_vec())
        }
        DirectType::Scalar(ScalarKind::Uint64) => {
            let unbox_u64 = codegen
                .object
                .declare_func_in_func(codegen.unbox_u64, builder.func);
            let inst = builder.ins().call(unbox_u64, &[raw]);
            Ok(builder.inst_results(inst).to_vec())
        }
        DirectType::Scalar(ScalarKind::Float32) | DirectType::Scalar(ScalarKind::Float64) => {
            let unbox_f64 = codegen
                .object
                .declare_func_in_func(codegen.unbox_f64, builder.func);
            let inst = builder.ins().call(unbox_f64, &[raw]);
            Ok(builder.inst_results(inst).to_vec())
        }
        DirectType::Scalar(ScalarKind::Bool) => {
            let unbox_bool = codegen
                .object
                .declare_func_in_func(codegen.unbox_bool, builder.func);
            let inst = builder.ins().call(unbox_bool, &[raw]);
            Ok(builder.inst_results(inst).to_vec())
        }
        DirectType::Scalar(ScalarKind::Unit) => Ok(vec![builder.ins().iconst(types::I64, 0)]),
        DirectType::PlainClass(class) => {
            let instance_get_field = codegen
                .object
                .declare_func_in_func(codegen.instance_get_field, builder.func);
            let mut values = Vec::new();
            for field in &class.fields {
                let (field_ptr, field_len) =
                    thunk_string_constant(codegen, builder, field.name.as_bytes())?;
                let inst = builder
                    .ins()
                    .call(instance_get_field, &[raw, field_ptr, field_len]);
                let field_raw = builder.inst_results(inst)[0];
                values.extend(unbox_thunk_value(codegen, builder, field_raw, &field.ty)?);
                if !matches!(field.ty, DirectType::Opaque(_)) {
                    let release_value = codegen
                        .object
                        .declare_func_in_func(codegen.release_value, builder.func);
                    builder.ins().call(release_value, &[field_raw]);
                }
            }
            Ok(values)
        }
    }
}

fn release_direct_call_results(
    codegen: &mut NativeCodegen<'_>,
    builder: &mut FunctionBuilder<'_>,
    function_name: &str,
    results: &[Value],
) -> std::result::Result<(), String> {
    let return_ty = codegen
        .function_return_types
        .get(function_name)
        .cloned()
        .ok_or({
            format!(
                "direct backend does not know return type for `{}`",
                function_name
            )
        })?;
    let return_count = return_ty.value_count();
    if results.len() < return_count {
        return Err(format!(
            "direct backend cleanup call `{}` returned too few values",
            function_name
        ));
    }
    release_direct_values(codegen, builder, &results[..return_count], &return_ty)?;
    let mut cursor = return_count;
    for writeback_ty in codegen
        .function_writeback_types
        .get(function_name)
        .cloned()
        .unwrap_or_default()
    {
        let count = writeback_ty.value_count();
        if results.len() < cursor + count {
            return Err(format!(
                "direct backend cleanup call `{}` returned incomplete writeback values",
                function_name
            ));
        }
        release_direct_values(
            codegen,
            builder,
            &results[cursor..cursor + count],
            &writeback_ty,
        )?;
        cursor += count;
    }
    Ok(())
}

fn release_direct_values(
    codegen: &mut NativeCodegen<'_>,
    builder: &mut FunctionBuilder<'_>,
    values: &[Value],
    ty: &DirectType,
) -> std::result::Result<(), String> {
    match ty {
        DirectType::Opaque(_) => {
            let Some(value) = values.first().copied() else {
                return Err(format!(
                    "direct backend cleanup expected an opaque `{}` result",
                    render_direct_type(ty)
                ));
            };
            let release_value = codegen
                .object
                .declare_func_in_func(codegen.release_value, builder.func);
            builder.ins().call(release_value, &[value]);
        }
        DirectType::PlainClass(class) => {
            let mut start = 0usize;
            for field in &class.fields {
                let end = start + field.ty.value_count();
                if values.len() < end {
                    return Err(format!(
                        "direct backend cleanup expected `{}` values for `{}`",
                        end,
                        render_direct_type(ty)
                    ));
                }
                release_direct_values(codegen, builder, &values[start..end], &field.ty)?;
                start = end;
            }
        }
        DirectType::Scalar(_) => {}
    }
    Ok(())
}

fn main_signature(call_conv: CallConv) -> Signature {
    let mut signature = Signature::new(call_conv);
    signature.returns.push(AbiParam::new(types::I32));
    signature
}

fn thunk_signature(call_conv: CallConv) -> Signature {
    let mut signature = Signature::new(call_conv);
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I64));
    signature
}

fn default_binder_signature(call_conv: CallConv) -> Signature {
    let mut signature = Signature::new(call_conv);
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    signature
}

fn mangle_symbol(name: &str) -> String {
    let mut mangled = String::from("aura_fn_");
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            mangled.push(ch);
        } else {
            mangled.push('_');
        }
    }
    mangled
}

fn public_direct_function_name(name: &str) -> String {
    name.split_once("::__default_")
        .map_or_else(|| name.to_string(), |(public, _)| public.to_string())
}

fn mangle_thunk_symbol(name: &str) -> String {
    let mut mangled = String::from("aura_thunk_");
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            mangled.push(ch);
        } else {
            mangled.push('_');
        }
    }
    mangled
}

fn mangle_default_binder_symbol(name: &str) -> String {
    let mut mangled = String::from("aura_default_binder_");
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            mangled.push(ch);
        } else {
            mangled.push('_');
        }
    }
    mangled
}

fn mangle_cleanup_thunk_symbol(function_name: &str, place: &str, index: usize) -> String {
    let mut mangled = format!("aura_cleanup_{}_", index);
    for ch in function_name
        .chars()
        .chain(std::iter::once('_'))
        .chain(place.chars())
    {
        if ch.is_ascii_alphanumeric() {
            mangled.push(ch);
        } else {
            mangled.push('_');
        }
    }
    mangled
}

fn direct_type_to_type(ty: &DirectType) -> Type {
    match ty {
        DirectType::Scalar(ScalarKind::Int32) => Type::named("int32"),
        DirectType::Scalar(ScalarKind::Int64) => Type::named("int64"),
        DirectType::Scalar(ScalarKind::Uint64) => Type::named("uint64"),
        DirectType::Scalar(ScalarKind::Float32) => Type::named("float32"),
        DirectType::Scalar(ScalarKind::Float64) => Type::named("float64"),
        DirectType::Scalar(ScalarKind::Bool) => Type::named("bool"),
        DirectType::Scalar(ScalarKind::Unit) => Type::Unit,
        DirectType::PlainClass(class) => Type::named(&class.class_name),
        DirectType::Opaque(ty) => ty.clone(),
    }
}

fn collect_direct_runtime_type_substitutions(
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
                collect_direct_runtime_type_substitutions(pattern_arg, actual_arg, substitutions);
            }
        }
        Type::Tuple(pattern_elements) => {
            let Type::Tuple(actual_elements) = actual else {
                return;
            };
            if pattern_elements.len() != actual_elements.len() {
                return;
            }
            for (pattern_element, actual_element) in
                pattern_elements.iter().zip(actual_elements.iter())
            {
                collect_direct_runtime_type_substitutions(
                    pattern_element,
                    actual_element,
                    substitutions,
                );
            }
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
                return;
            };
            if pattern_params.len() != actual_params.len() {
                return;
            }
            for (pattern_param, actual_param) in pattern_params.iter().zip(actual_params.iter()) {
                if pattern_param.passing != actual_param.passing {
                    return;
                }
                collect_direct_runtime_type_substitutions(
                    &pattern_param.ty,
                    &actual_param.ty,
                    substitutions,
                );
            }
            collect_direct_runtime_type_substitutions(pattern_return, actual_return, substitutions);
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
                collect_direct_runtime_type_substitutions(
                    &pattern_param.ty,
                    &actual_param.ty,
                    substitutions,
                );
            }
            for (pattern_capture, actual_capture) in
                pattern_captures.iter().zip(actual_captures.iter())
            {
                collect_direct_runtime_type_substitutions(
                    &pattern_capture.ty,
                    &actual_capture.ty,
                    substitutions,
                );
            }
            collect_direct_runtime_type_substitutions(pattern_return, actual_return, substitutions);
        }
        Type::Unit | Type::Module(_) => {}
    }
}

fn is_numeric_type_name(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) if args.is_empty() => {
            name == "float32"
                || name == "float64"
                || name.starts_with("int")
                || name.starts_with("uint")
        }
        _ => false,
    }
}

fn runtime_type_is_wildcard(ty: &Type) -> bool {
    match ty {
        Type::TypeParam(_) => true,
        Type::Named(name, _) if name == "Unknown" => true,
        Type::Named(_, args) => args.iter().any(runtime_type_is_wildcard),
        Type::Tuple(elements) => elements.iter().any(runtime_type_is_wildcard),
        Type::Function {
            params,
            return_type,
            ..
        } => {
            params
                .iter()
                .any(|param| runtime_type_is_wildcard(&param.ty))
                || runtime_type_is_wildcard(return_type)
        }
        Type::Closure {
            params,
            return_type,
            captures,
            ..
        } => {
            params
                .iter()
                .any(|param| runtime_type_is_wildcard(&param.ty))
                || captures
                    .iter()
                    .any(|capture| runtime_type_is_wildcard(&capture.ty))
                || runtime_type_is_wildcard(return_type)
        }
        Type::Unit | Type::Module(_) => false,
    }
}

fn collect_task_start_targets(
    module: &MirModule,
    reachable_by_function: &HashMap<String, HashSet<String>>,
) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for function in module.functions.iter().chain(module.top_level.iter()) {
        let Some(reachable) = reachable_by_function.get(&function.name) else {
            continue;
        };
        for block in &function.blocks {
            if !reachable.contains(&block.label) {
                continue;
            }
            for instruction in &block.instructions {
                if let Instruction::Assign {
                    value: Rvalue::StartTask { function, .. },
                    ..
                } = instruction
                {
                    targets.insert(match function {
                        Operand::Function { name, .. } => name.clone(),
                        _ => "<dynamic-function-value>".to_string(),
                    });
                }
            }
        }
    }
    targets
}

#[cfg(test)]
#[path = "native_codegen_tests.rs"]
mod tests;
