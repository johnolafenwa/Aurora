use super::{
    append_string_with_limit, cancel_current_lightweight_task_boundary, cast_numeric_value,
    claim_task_result_observations, create_dir_once, decode_process_restart_policy,
    decode_process_stdio, divmod_numeric_values, finalize_task_execution, float_floor_divmod,
    float_power, format_runtime_value, io_decode_utf8, io_error, lock_mutex,
    next_retry_runtime_backoff, non_unix_tls_listener_wait_timeout, option_none, option_some,
    parse_format_spec, process_error_cancelled, process_error_no_command, process_error_other,
    process_error_spawn, process_error_timed_out, process_supervisor_event_failed,
    process_supervisor_wait_cancelled, process_supervisor_wait_event,
    process_supervisor_wait_timed_out, process_wait_cancelled, process_wait_failed,
    process_wait_timed_out, queue_receive_cancelled, queue_receive_closed, queue_receive_item,
    queue_receive_timed_out, recv_for_task_group_iteration, remove_file_checked, render_float,
    render_float32, result_err, result_ok, round_numeric_value, run_blocking_io,
    run_lightweight_root_task, run_protocol_step, select_outcome_deadline, select_outcome_queue,
    select_outcome_task, select_runtime_values, send_error_cancelled, send_error_closed,
    send_error_full, send_error_timed_out, sleep_with_runtime_scheduler, slice_string_owned,
    slice_vec_owned, spawn_lightweight_task, spawn_lightweight_task_with_cancellation,
    spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup,
    spawn_lightweight_task_with_stack, task_group_cleanup_should_cancel, task_result_cancelled,
    task_result_error, task_result_ready, task_result_timed_out, validate_format_spec_for_type,
    validate_read_line_capacity, validate_requested_read_size, validate_retry_runtime_policy,
    wait_all_cancelled, wait_all_error, wait_all_ready, wait_all_timed_out, wait_any_cancelled,
    wait_any_error, wait_any_ready, wait_any_timed_out, wait_condvar, wait_for_runtime_scheduler,
    wait_timeout_condvar, ArrayBinaryOp, ArrayDType, ArrayReduction, ArrayStorage, ArrayValue,
    BlockingIoPool, CancellationContext, ChannelValue, ClosureCaptureValue, ClosureEnvironment,
    EnumVariantValue, FfiHandleValue, FileValue, FloatPowerWidth, FormatSpecErrorKind,
    FunctionValue, HttpListenerValue, HttpResponseValue, InstanceValue, IntegerArithmeticMode,
    LightweightTaskFailureSignal, MapValue, ModuleNamespaceValue, ProcessChildValue,
    ProcessChildWaitStatus, ProcessCompletedValue, ProcessRestartPolicy, ProcessStdioConfig,
    ProcessSupervisorValue, ProcessSupervisorWaitStatus, RangeValue, ReactorSubscription,
    RecvValueResult, RngValue, SetValue, TaskCancelledSignal, TaskExecutionResult, TaskGroupValue,
    TaskValue, TaskWaitStatus, TcpListenerValue, TcpStreamValue, TryRecvResult, TupleValue,
    UdpDatagramValue, UdpSocketValue, Value, VecValue, WebSocketListenerValue,
    MAX_FILESYSTEM_READ_BYTES, MAX_STREAM_READ_BYTES,
};
use super::{install_after_select_queue_commit_hook, install_after_select_source_validation_hook};
use crate::integer::IntegerKind;

fn math_call(name: &str, values: &[f64]) -> super::Result<Value> {
    super::evaluate_host_builtin(
        &format!("math::{name}"),
        values.iter().copied().map(Value::Float).collect(),
    )
}

fn expect_math_float(name: &str, values: &[f64]) -> f64 {
    let Value::Float(value) = math_call(name, values).expect("math call should succeed") else {
        panic!("math.{name} should return float64");
    };
    value
}

fn expect_math_int(name: &str, value: f64) -> i64 {
    let Value::Int(value) = math_call(name, &[value]).expect("math call should succeed") else {
        panic!("math.{name} should return int64");
    };
    i64::try_from(value.as_i128().expect("math integer should be signed"))
        .expect("math integer should fit int64")
}

#[test]
fn math_host_builtins_follow_the_ratified_finite_contract() {
    assert_eq!(expect_math_int("floor", -1.25), -2);
    assert_eq!(expect_math_int("ceil", -1.25), -1);
    assert_eq!(expect_math_int("trunc", -1.75), -1);
    assert_eq!(expect_math_float("pow", &[2.0, -3.0]), 0.125);
    assert_eq!(expect_math_float("exp", &[0.0]), 1.0);
    assert_eq!(expect_math_float("log", &[1.0]), 0.0);
    assert_eq!(expect_math_float("log2", &[8.0]), 3.0);
    assert_eq!(expect_math_float("log10", &[1000.0]), 3.0);
    assert_eq!(expect_math_float("sin", &[0.0]), 0.0);
    assert_eq!(expect_math_float("cos", &[0.0]), 1.0);
    assert_eq!(expect_math_float("tan", &[0.0]), 0.0);
}

#[test]
fn math_host_builtins_classify_every_exception_family() {
    for name in ["floor", "ceil", "trunc"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0e20] {
            let error = math_call(name, &[value]).expect_err("conversion must fail");
            assert_eq!(error.code, "AU4002", "{name}({value})");
        }
    }

    assert!(expect_math_float("exp", &[f64::NAN]).is_nan());
    assert_eq!(expect_math_float("exp", &[f64::INFINITY]), f64::INFINITY);
    assert_eq!(expect_math_float("exp", &[f64::NEG_INFINITY]), 0.0);
    assert_eq!(
        math_call("exp", &[1000.0])
            .expect_err("finite exponential overflow must trap")
            .code,
        "AU4002"
    );

    for name in ["log", "log2", "log10"] {
        assert!(expect_math_float(name, &[f64::NAN]).is_nan());
        assert_eq!(expect_math_float(name, &[f64::INFINITY]), f64::INFINITY);
        for value in [0.0, -0.0, -1.0] {
            let error = math_call(name, &[value]).expect_err("log domain must fail");
            assert_eq!(error.code, "AU4001", "{name}({value})");
        }
    }

    for name in ["sin", "cos", "tan"] {
        assert!(expect_math_float(name, &[f64::NAN]).is_nan());
        for value in [f64::INFINITY, f64::NEG_INFINITY] {
            let error = math_call(name, &[value]).expect_err("trig infinity must fail");
            assert_eq!(error.code, "AU4001", "{name}({value})");
        }
    }

    assert_eq!(expect_math_float("pow", &[f64::NAN, 0.0]), 1.0);
    assert_eq!(expect_math_float("pow", &[1.0, f64::NAN]), 1.0);
    assert!(expect_math_float("pow", &[f64::NAN, 2.0]).is_nan());
    for (base, exponent) in [(0.0, -1.0), (-2.0, 0.5)] {
        let error = math_call("pow", &[base, exponent]).expect_err("pow domain must fail");
        assert_eq!(error.code, "AU4001");
    }
    assert_eq!(
        math_call("pow", &[f64::MAX, 2.0])
            .expect_err("finite power overflow must trap")
            .code,
        "AU4002"
    );
}

#[test]
fn practical_format_specifications_are_unicode_aware_and_width_exact() {
    let integer = Value::Int(crate::integer::IntegerValue::from_i64(-1_234_567));
    assert_eq!(
        format_runtime_value(&integer, &Type::named("int64"), "+15,d").unwrap(),
        "     -1,234,567"
    );
    assert_eq!(
        format_runtime_value(&integer, &Type::named("int64"), "*>12X").unwrap(),
        "*****-12D687"
    );
    assert_eq!(
        format_runtime_value(
            &Value::String("aura✨".to_string()),
            &Type::named("str"),
            "·^9.4s",
        )
        .unwrap(),
        "··aura···"
    );
}

#[test]
fn format_spec_variants_preserve_exact_output_and_teaching_diagnostics() {
    let positive = Value::Int(crate::integer::IntegerValue::from_i64(12_345));
    assert_eq!(
        format_runtime_value(&positive, &Type::named("int64"), "-d").unwrap(),
        "12345"
    );
    assert_eq!(
        format_runtime_value(&positive, &Type::named("int64"), ",.2f").unwrap(),
        "12,345.00"
    );
    assert_eq!(
        format_runtime_value(
            &Value::Int(crate::integer::IntegerValue::from_i64(999)),
            &Type::named("int64"),
            ".1e",
        )
        .unwrap(),
        "1.0e+03"
    );
    assert_eq!(
        format_runtime_value(
            &Value::Int(crate::integer::IntegerValue::from_i64(0)),
            &Type::named("int64"),
            ".0e",
        )
        .unwrap(),
        "0e+00"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(12_345.5), &Type::named("float64"), ",.2f",).unwrap(),
        "12,345.50"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(0.0012), &Type::named("float64"), ".2e",).unwrap(),
        "1.20e-03"
    );
    assert_eq!(
        format_runtime_value(
            &Value::String("Aura".to_string()),
            &Type::named("str"),
            "<8s",
        )
        .unwrap(),
        "Aura    "
    );

    let tuple = Value::Tuple(TupleValue {
        element_types: vec![Type::named("int64"), Type::named("str")],
        elements: vec![
            Value::Int(crate::integer::IntegerValue::from_i64(1)),
            Value::String("one".to_string()),
        ],
    });
    assert_eq!(
        format_runtime_value(
            &tuple,
            &Type::Tuple(vec![Type::named("int64"), Type::named("str")]),
            "",
        )
        .unwrap(),
        "(1, one)"
    );

    let malformed = [
        (".s", "format precision requires decimal digits after `.`"),
        ("dd", "malformed format specification `dd`"),
    ];
    for (source, expected) in malformed {
        let error = parse_format_spec(source).expect_err("the specification must be rejected");
        assert_eq!(error.message, expected);
    }

    let incompatible = [
        (
            &positive,
            Type::named("int64"),
            "s",
            "format code `s` requires `str`, found integer",
        ),
        (
            &Value::Float(1.0),
            Type::named("float64"),
            "d",
            "integer format code requires an integer value, found float",
        ),
        (
            &tuple,
            Type::Tuple(vec![Type::named("int64"), Type::named("str")]),
            "f",
            "numeric format code requires an integer or floating value, found value",
        ),
        (
            &Value::String("Aura".to_string()),
            Type::named("str"),
            "+s",
            "a format sign is valid only for numeric values",
        ),
        (
            &positive,
            Type::named("int64"),
            ",x",
            "the thousands separator is valid only with d, f, and %",
        ),
        (
            &positive,
            Type::named("int64"),
            ".2d",
            "precision requires s, f, e, or %; integer precision is available only through f, e, and %",
        ),
    ];
    for (value, value_type, source, expected) in incompatible {
        let error = format_runtime_value(value, &value_type, source)
            .expect_err("the type-incompatible specification must be rejected");
        assert_eq!(error.code, "AU2002");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn floating_formatting_preserves_binary32_identity_and_ieee_signs() {
    let stored = Value::Float(1.234_567_890_123);
    assert_eq!(
        format_runtime_value(&stored, &Type::named("float32"), ".8f").unwrap(),
        format!("{:.8}", 1.234_567_890_123_f64 as f32)
    );
    assert_eq!(
        format_runtime_value(&Value::Float(-0.0), &Type::named("float64"), "+.2f").unwrap(),
        "-0.00"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(f64::INFINITY), &Type::named("float64"), "+f").unwrap(),
        "+inf"
    );
    assert_eq!(
        format_runtime_value(
            &Value::Float(f64::NEG_INFINITY),
            &Type::named("float64"),
            " f",
        )
        .unwrap(),
        "-inf"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(f64::NAN), &Type::named("float64"), " 8f").unwrap(),
        "     nan"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(2.5), &Type::named("float64"), ".0f").unwrap(),
        "2"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(3.5), &Type::named("float64"), ".0f").unwrap(),
        "4"
    );
}

#[test]
fn python_zero_pad_shorthand_places_padding_after_the_numeric_sign() {
    for (value, spec, expected) in [
        (-1.25, "09.3f", "-0001.250"),
        (1.25, "09.3f", "00001.250"),
        (1.25, "+09.3f", "+0001.250"),
        (1.25, " 09.3f", " 0001.250"),
    ] {
        assert_eq!(
            format_runtime_value(&Value::Float(value), &Type::named("float64"), spec).unwrap(),
            expected,
            "format {spec}"
        );
    }
}

#[test]
fn format_width_and_precision_enforce_the_ratified_boundaries() {
    assert_eq!(
        parse_format_spec("1000000.1000000s").unwrap().width,
        Some(1_000_000)
    );
    assert!(parse_format_spec("1000001s")
        .unwrap_err()
        .message
        .contains("cannot exceed 1000000"));
    assert!(parse_format_spec(".1000001s")
        .unwrap_err()
        .message
        .contains("cannot exceed 1000000"));
    assert!(parse_format_spec(",,d").is_err());
    assert!(parse_format_spec("{width}d").is_err());

    let widest = format_runtime_value(
        &Value::String("x".to_string()),
        &Type::named("str"),
        "1000000s",
    )
    .unwrap();
    assert_eq!(widest.chars().count(), 1_000_000);

    let mut bounded = "abcd".to_string();
    let error = append_string_with_limit(&mut bounded, "e", 4).unwrap_err();
    assert_eq!(error.code, "AU4005");
    assert_eq!(
        bounded, "abcd",
        "a failed preflight must not mutate the output"
    );
}

#[test]
fn format_components_reject_host_overflow_and_round_decimal_ties_to_even() {
    let oversized_component = "9".repeat(128);
    let width = parse_format_spec(&format!("{oversized_component}d"))
        .expect_err("a width outside usize must be rejected before the maintained limit check");
    assert_eq!(width.kind, FormatSpecErrorKind::Syntax);
    assert_eq!(width.message, "format width is not a valid decimal integer");

    let precision = parse_format_spec(&format!(".{oversized_component}e"))
        .expect_err("a precision outside usize must be rejected before formatting");
    assert_eq!(precision.kind, FormatSpecErrorKind::Syntax);
    assert_eq!(
        precision.message,
        "format precision is not a valid decimal integer"
    );

    for (value, expected) in [(1_251, "1.3e+03"), (1_350, "1.4e+03"), (1_250, "1.2e+03")] {
        assert_eq!(
            format_runtime_value(
                &Value::Int(IntegerValue::from_i64(value)),
                &Type::named("int64"),
                ".1e",
            )
            .expect("integer scientific formatting should round without binary64 conversion"),
            expected,
            "formatting {value} should use decimal ties-to-even"
        );
    }
}

#[test]
fn integer_float_style_formats_do_not_round_through_binary64() {
    let exact = Value::Int(
        crate::integer::IntegerValue::from_typed_unsigned(
            9_007_199_254_740_993,
            IntegerKind::Uint64,
        )
        .unwrap(),
    );
    assert_eq!(
        format_runtime_value(&exact, &Type::named("uint64"), ".0f").unwrap(),
        "9007199254740993"
    );
    assert_eq!(
        format_runtime_value(&exact, &Type::named("uint64"), ".2e").unwrap(),
        "9.01e+15"
    );
    assert_eq!(
        format_runtime_value(&exact, &Type::named("uint64"), ".0%").unwrap(),
        "900719925474099300%"
    );
}

#[test]
fn impossible_runtime_format_contract_mismatches_are_diagnostics() {
    let error = format_runtime_value(
        &Value::String("not an integer".to_string()),
        &Type::named("int64"),
        "d",
    )
    .unwrap_err();
    assert_eq!(error.code, "AU4001");
    assert!(error.message.contains("internal format contract mismatch"));
}

#[test]
fn format_matrix_pins_alignment_sign_radix_scientific_and_default_rendering() {
    let string = Value::String("Aura".to_string());
    for (spec, expected) in [
        ("8s", "Aura    "),
        (">8s", "    Aura"),
        ("·^9s", "··Aura···"),
        (".<7.3s", "Aur...."),
    ] {
        assert_eq!(
            format_runtime_value(&string, &Type::named("str"), spec).unwrap(),
            expected,
            "string format {spec}"
        );
    }

    let positive = Value::Int(crate::integer::IntegerValue::from_i64(42));
    let negative = Value::Int(crate::integer::IntegerValue::from_i64(-42));
    for (value, spec, expected) in [
        (&positive, "+d", "+42"),
        (&positive, " d", " 42"),
        (&positive, "-d", "42"),
        (&negative, "d", "-42"),
        (&positive, "x", "2a"),
        (&positive, "X", "2A"),
        (&positive, "b", "101010"),
        (&positive, "o", "52"),
        (&positive, "08d", "00000042"),
        (&positive, ".0e", "4e+01"),
    ] {
        assert_eq!(
            format_runtime_value(value, &Type::named("int64"), spec).unwrap(),
            expected,
            "integer format {spec}"
        );
    }

    let zero = Value::Int(crate::integer::IntegerValue::from_i64(0));
    assert_eq!(
        format_runtime_value(&zero, &Type::named("int64"), ".0e").unwrap(),
        "0e+00"
    );
    let carries = Value::Int(crate::integer::IntegerValue::from_i64(999));
    assert_eq!(
        format_runtime_value(&carries, &Type::named("int64"), ".1e").unwrap(),
        "1.0e+03"
    );
    let grouped = Value::Int(crate::integer::IntegerValue::from_i64(12_345));
    assert_eq!(
        format_runtime_value(&grouped, &Type::named("int64"), ",.2f").unwrap(),
        "12,345.00"
    );

    assert_eq!(
        format_runtime_value(&Value::Float(12_345.25), &Type::named("float64"), ",.2f").unwrap(),
        "12,345.25"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(12.5), &Type::named("float64"), ".2e").unwrap(),
        "1.25e+01"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(-0.0), &Type::named("float64"), "").unwrap(),
        "-0.0"
    );
    assert_eq!(
        format_runtime_value(&Value::Float(1.25), &Type::named("float32"), "").unwrap(),
        "1.25"
    );
    assert_eq!(
        format_runtime_value(&positive, &Type::named("int64"), "").unwrap(),
        "42"
    );
    assert_eq!(
        format_runtime_value(&Value::Bool(true), &Type::named("bool"), "").unwrap(),
        "true"
    );
}

#[test]
fn format_matrix_pins_each_public_syntax_and_type_error_class() {
    for (spec, message) in [
        (".f", "format precision requires decimal digits after `.`"),
        ("dd", "malformed format specification `dd`"),
        ("q", "unsupported format type `q`"),
        ("{width}d", "cannot contain nested replacement fields"),
    ] {
        let error = parse_format_spec(spec).expect_err("invalid format syntax must fail");
        assert_eq!(error.kind, FormatSpecErrorKind::Syntax, "format {spec}");
        assert!(error.message.contains(message), "{}", error.message);
    }

    for (spec, ty, message) in [
        ("s", "int64", "format code `s` requires `str`"),
        (
            "d",
            "float64",
            "integer format code requires an integer value",
        ),
        (
            "f",
            "str",
            "numeric format code requires an integer or floating value",
        ),
        (
            "+s",
            "str",
            "a format sign is valid only for numeric values",
        ),
        (
            ",x",
            "int64",
            "thousands separator is valid only with d, f, and %",
        ),
        (
            "05s",
            "str",
            "zero-padding shorthand is valid only for numeric values",
        ),
        (".2d", "int64", "precision requires s, f, e, or %"),
    ] {
        let parsed = parse_format_spec(spec).expect("the spelling is syntactically valid");
        let error = validate_format_spec_for_type(&parsed, &Type::named(ty))
            .expect_err("the format must be rejected for this static type");
        assert_eq!(error.kind, FormatSpecErrorKind::Type, "format {spec}");
        assert!(error.message.contains(message), "{}", error.message);
    }

    let syntax = format_runtime_value(
        &Value::Int(crate::integer::IntegerValue::from_i64(1)),
        &Type::named("int64"),
        "{width}d",
    )
    .expect_err("runtime formatting must preserve syntax diagnostics");
    assert_eq!(syntax.code, "AU1101");

    let wrong_type = format_runtime_value(
        &Value::Int(crate::integer::IntegerValue::from_i64(1)),
        &Type::named("int64"),
        "s",
    )
    .expect_err("runtime formatting must preserve static type diagnostics");
    assert_eq!(wrong_type.code, "AU2002");
}

#[test]
fn math_host_dispatch_rejects_wrong_runtime_types_and_arities_exactly() {
    for name in ["floor", "ceil", "trunc"] {
        let error = super::evaluate_host_builtin(
            &format!("math::{name}"),
            vec![Value::Int(crate::integer::IntegerValue::from_i64(1))],
        )
        .expect_err("integer arguments must not masquerade as float64");
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, format!("`math.{name}` expects `float64`"));
    }

    let power_type = super::evaluate_host_builtin(
        "math::pow",
        vec![Value::Float(2.0), Value::String("3".to_string())],
    )
    .expect_err("math.pow requires two float64 arguments");
    assert_eq!(power_type.code, "AU4001");
    assert_eq!(
        power_type.message,
        "`math.pow` expects two `float64` arguments"
    );

    let power_arity = super::evaluate_host_builtin("math::pow", vec![Value::Float(2.0)])
        .expect_err("math.pow requires two arguments");
    assert_eq!(power_arity.code, "AU2004");
    assert!(power_arity.message.contains("expects 2 argument"));

    for name in ["exp", "log", "log2", "log10", "sin", "cos", "tan"] {
        let error = super::evaluate_host_builtin(&format!("math::{name}"), vec![Value::Bool(true)])
            .expect_err("non-float arguments must be diagnosed");
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, format!("`math.{name}` expects `float64`"));
    }
}

#[test]
fn dense_arrays_validate_shape_storage_and_deep_clone() {
    let array = ArrayValue::new(
        vec![2, 2].into_boxed_slice(),
        ArrayStorage::Int32(vec![1, 2, 3, 4].into_boxed_slice()),
    )
    .expect("matching row-major shape and storage should be accepted");
    assert_eq!(array.dtype(), ArrayDType::Int32);
    assert_eq!(array.element_type(), Type::named("int32"));
    assert_eq!(array.rank(), 2);
    assert_eq!(array.len(), 4);
    assert_eq!(
        Value::Array(array.clone()).render(),
        "Array[int32](shape=[2, 2], values=[1, 2, 3, 4])"
    );

    let cloned = array.clone();
    let (ArrayStorage::Int32(source), ArrayStorage::Int32(copy)) =
        (&array.storage, &cloned.storage)
    else {
        panic!("test arrays should retain int32 storage");
    };
    assert_ne!(
        source.as_ptr(),
        copy.as_ptr(),
        "array clones must own independent contiguous buffers"
    );
    assert_eq!(array, cloned);

    let fallible_clone = array
        .try_clone()
        .expect("explicit Array clones should use fallible storage copies");
    let (ArrayStorage::Int32(source), ArrayStorage::Int32(copy)) =
        (&array.storage, &fallible_clone.storage)
    else {
        panic!("test arrays should retain int32 storage");
    };
    assert_ne!(source.as_ptr(), copy.as_ptr());
    assert_ne!(array.shape.as_ptr(), fallible_clone.shape.as_ptr());

    for allocation_budget in [0, 1] {
        let clone_error =
            super::with_array_allocation_budget(allocation_budget, || array.try_clone())
                .expect_err("shape and storage allocation failures must remain recoverable");
        assert_eq!(clone_error.code, "AU4005");
    }
    let shape_error = super::with_array_allocation_budget(0, || array.shape_value())
        .expect_err("shape materialization allocation failures must remain recoverable");
    assert_eq!(shape_error.code, "AU4005");

    let rank_error = ArrayValue::new(
        Vec::new().into_boxed_slice(),
        ArrayStorage::Float64(vec![1.0].into_boxed_slice()),
    )
    .expect_err("rank-zero arrays are outside the Phase 7.3 surface");
    assert_eq!(rank_error.code, "AU4007");

    let shape_error = ArrayValue::new(
        vec![2, 3].into_boxed_slice(),
        ArrayStorage::Int64(vec![1, 2].into_boxed_slice()),
    )
    .expect_err("shape products must exactly match storage");
    assert_eq!(shape_error.code, "AU4007");

    let product_error = ArrayValue::new(
        vec![i64::MAX as usize, 3].into_boxed_slice(),
        ArrayStorage::Float32(Vec::new().into_boxed_slice()),
    )
    .expect_err("shape products must be checked");
    assert_eq!(product_error.code, "AU4005");
    assert_eq!(
        product_error.message,
        format!(
            "array shape product overflows host allocation bounds: [{}, 3]",
            i64::MAX
        )
    );

    ArrayValue::new(
        vec![i64::MAX as usize, 2, 0].into_boxed_slice(),
        ArrayStorage::Float32(Vec::new().into_boxed_slice()),
    )
    .expect("a zero dimension makes the checked row-major product zero regardless of axis order");

    let dimension_error = ArrayValue::new(
        vec![i64::MAX as usize + 1, 0].into_boxed_slice(),
        ArrayStorage::Float32(Vec::new().into_boxed_slice()),
    )
    .expect_err("every public shape dimension must fit the int64 shape surface");
    assert_eq!(dimension_error.code, "AU4005");

    let allocation_error = ArrayValue::zeros(
        ArrayDType::Float64,
        vec![i64::MAX as usize].into_boxed_slice(),
    )
    .expect_err("impossible host capacities must return AU4005 without attempting an OOM");
    assert_eq!(allocation_error.code, "AU4005");
}

#[test]
fn dense_arrays_copy_vec_inputs_normalize_coordinates_and_slice_the_first_axis() {
    let source = VecValue {
        element_type: Type::named("int32"),
        elements: (0..6)
            .map(|value| Value::Int(IntegerValue::from_i32(value)))
            .collect(),
    };
    let array = ArrayValue::from_vec(&source, Some(&[2, 3]))
        .expect("typed Vec input should copy into dense storage");
    assert_eq!(
        array
            .get(&[0, -1])
            .expect("negative coordinate should normalize once"),
        Value::Int(IntegerValue::from_i32(2))
    );
    assert_eq!(
        array
            .get(&[-1, 0])
            .expect("negative first-axis coordinate should work"),
        Value::Int(IntegerValue::from_i32(3))
    );
    let coordinate_error = array
        .get(&[-3, 0])
        .expect_err("coordinates below -len must not clamp");
    assert_eq!(coordinate_error.code, "AU4003");
    assert_eq!(
        array
            .get_optional(&[-3, 0])
            .expect("optional lookup should retain a valid-rank query"),
        None,
        "Array.get maps coordinate bounds errors to None"
    );
    let rank_error = array
        .get(&[0])
        .expect_err("coordinate count must equal rank");
    assert_eq!(rank_error.code, "AU4007");
    assert_eq!(
        array
            .get_optional(&[0])
            .expect("Array.get must map a rank mismatch to an absent optional value"),
        None
    );
    let mut rejected_update = array.clone();
    assert_eq!(
        rejected_update
            .set(&[-3, 0], Value::Int(IntegerValue::from_i32(9)))
            .expect_err("Array.set must trap rather than clamp invalid coordinates")
            .code,
        "AU4003"
    );
    assert_eq!(
        rejected_update, array,
        "a failed Array.set must not expose a partial mutation"
    );

    let tail = array
        .slice_first_axis(Some(-1), None)
        .expect("first-axis slices should normalize like Vec slices");
    assert_eq!(tail.shape.as_ref(), &[1, 3]);
    assert_eq!(
        tail,
        ArrayValue::new(
            vec![1, 3].into_boxed_slice(),
            ArrayStorage::Int32(vec![3, 4, 5].into_boxed_slice()),
        )
        .unwrap()
    );
    let (ArrayStorage::Int32(source_storage), ArrayStorage::Int32(tail_storage)) =
        (&array.storage, &tail.storage)
    else {
        panic!("test arrays should retain int32 storage");
    };
    assert_ne!(source_storage.as_ptr(), tail_storage.as_ptr());

    let mut updated = array.clone();
    let previous = updated
        .set(&[0, 1], Value::Int(IntegerValue::from_i32(20)))
        .expect("set should retain dtype and return the previous scalar");
    assert_eq!(previous, Value::Int(IntegerValue::from_i32(1)));
    updated
        .fill(Value::Int(IntegerValue::from_i32(-4)))
        .expect("fill should update every scalar without changing shape");
    assert_eq!(
        updated,
        ArrayValue::new(
            vec![2, 3].into_boxed_slice(),
            ArrayStorage::Int32(vec![-4; 6].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        source.elements[1],
        Value::Int(IntegerValue::from_i32(1)),
        "constructing and mutating an Array must not alter its Vec source"
    );
}

#[test]
fn dense_arrays_all_dtypes_construct_clone_mutate_fill_and_slice_exactly() {
    fn assert_independent_clone(array: &ArrayValue) {
        let copy = array
            .try_clone()
            .expect("every supported Array dtype should clone fallibly");
        assert_eq!(copy, *array);
        assert_ne!(copy.shape.as_ptr(), array.shape.as_ptr());
        match (&array.storage, &copy.storage) {
            (ArrayStorage::Int32(source), ArrayStorage::Int32(copy)) => {
                assert_ne!(source.as_ptr(), copy.as_ptr());
            }
            (ArrayStorage::Int64(source), ArrayStorage::Int64(copy)) => {
                assert_ne!(source.as_ptr(), copy.as_ptr());
            }
            (ArrayStorage::Float32(source), ArrayStorage::Float32(copy)) => {
                assert_ne!(source.as_ptr(), copy.as_ptr());
            }
            (ArrayStorage::Float64(source), ArrayStorage::Float64(copy)) => {
                assert_ne!(source.as_ptr(), copy.as_ptr());
            }
            _ => panic!("an Array clone must retain its exact dtype"),
        }
    }

    let full_cases = [
        (
            ArrayDType::Int32,
            Value::Int(IntegerValue::from_i32(6)),
            ArrayStorage::Int32(vec![6, 6].into_boxed_slice()),
        ),
        (
            ArrayDType::Int64,
            Value::Int(IntegerValue::from_i64(7)),
            ArrayStorage::Int64(vec![7, 7].into_boxed_slice()),
        ),
        (
            ArrayDType::Float32,
            Value::Float(1.25),
            ArrayStorage::Float32(vec![1.25, 1.25].into_boxed_slice()),
        ),
        (
            ArrayDType::Float64,
            Value::Float(-2.5),
            ArrayStorage::Float64(vec![-2.5, -2.5].into_boxed_slice()),
        ),
    ];
    for (dtype, value, storage) in full_cases {
        assert_eq!(
            ArrayValue::full(dtype, vec![2].into_boxed_slice(), &value)
                .expect("Array.full should create every supported dtype"),
            ArrayValue::new(vec![2].into_boxed_slice(), storage).unwrap()
        );
    }

    let mut int64_array = ArrayValue::from_vec(
        &VecValue {
            element_type: Type::named("int64"),
            elements: [-3_i64, 5, 9]
                .into_iter()
                .map(|value| Value::Int(IntegerValue::from_i64(value)))
                .collect(),
        },
        None,
    )
    .expect("Array[int64].from_list should infer a one-dimensional shape");
    assert_eq!(int64_array.shape.as_ref(), &[3]);
    assert_eq!(
        int64_array
            .set(&[-1], Value::Int(IntegerValue::from_i64(11)))
            .unwrap(),
        Value::Int(IntegerValue::from_i64(9))
    );
    int64_array
        .fill(Value::Int(IntegerValue::from_i64(-7)))
        .unwrap();
    assert_eq!(
        int64_array
            .slice_first_axis(Some(1), None)
            .expect("int64 slicing should preserve dtype and normalized bounds"),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int64(vec![-7, -7].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        int64_array.render(),
        "Array[int64](shape=[3], values=[-7, -7, -7])"
    );
    assert_independent_clone(&int64_array);

    let mut float32_array = ArrayValue::from_vec(
        &VecValue {
            element_type: Type::named("float32"),
            elements: [1.25, -2.5, 4.0].into_iter().map(Value::Float).collect(),
        },
        Some(&[3]),
    )
    .expect("Array[float32].from_list should retain float32 storage");
    assert_eq!(
        float32_array.set(&[1], Value::Float(3.5)).unwrap(),
        Value::Float(-2.5)
    );
    float32_array.fill(Value::Float(2.25)).unwrap();
    assert_eq!(
        float32_array
            .slice_first_axis(None, Some(-1))
            .expect("float32 slicing should preserve its narrowed storage"),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float32(vec![2.25, 2.25].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        float32_array.render(),
        "Array[float32](shape=[3], values=[2.25, 2.25, 2.25])"
    );
    assert_independent_clone(&float32_array);

    let mut float64_array = ArrayValue::from_vec(
        &VecValue {
            element_type: Type::named("float64"),
            elements: [1.5, 2.5, 3.5].into_iter().map(Value::Float).collect(),
        },
        Some(&[3]),
    )
    .expect("Array[float64].from_list should retain float64 storage");
    assert_eq!(
        float64_array.set(&[0], Value::Float(-4.5)).unwrap(),
        Value::Float(1.5)
    );
    float64_array.fill(Value::Float(8.5)).unwrap();
    assert_eq!(
        float64_array
            .slice_first_axis(Some(-2), None)
            .expect("float64 slicing should normalize negative bounds"),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float64(vec![8.5, 8.5].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        float64_array.render(),
        "Array[float64](shape=[3], values=[8.5, 8.5, 8.5])"
    );
    assert_independent_clone(&float64_array);
}

#[test]
fn array_slices_report_storage_allocation_failure_for_every_dtype_without_mutation() {
    let arrays = [
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int32(vec![1, 2].into_boxed_slice()),
        )
        .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int64(vec![1, 2].into_boxed_slice()),
        )
        .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float32(vec![1.0, 2.0].into_boxed_slice()),
        )
        .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float64(vec![1.0, 2.0].into_boxed_slice()),
        )
        .unwrap(),
    ];

    for array in arrays {
        let before = array.clone();
        let error =
            super::with_array_allocation_budget(1, || array.slice_first_axis(Some(0), Some(1)))
                .expect_err("the storage copy after the shape copy must remain fallible");
        assert_eq!(error.code, "AU4005");
        assert_eq!(
            error.message,
            "array slice could not allocate storage for 1 array elements"
        );
        assert_eq!(array, before, "a failed slice must not mutate its source");
    }
}

#[test]
fn array_from_vec_validates_shape_and_count_before_allocation_or_conversion() {
    let malformed_source = VecValue {
        element_type: Type::named("int32"),
        elements: vec![Value::Int(IntegerValue::from_i64(7))],
    };

    let rank_error = super::with_array_allocation_budget(0, || {
        ArrayValue::from_vec(&malformed_source, Some(&[]))
    })
    .expect_err("rank-zero from_vec shapes must fail before any allocation");
    assert_eq!(rank_error.code, "AU4007");
    assert_eq!(rank_error.message, "array rank must be at least one");

    let count_error = super::with_array_allocation_budget(0, || {
        ArrayValue::from_vec(&malformed_source, Some(&[2]))
    })
    .expect_err("shape/count mismatches must precede allocation and element conversion");
    assert_eq!(count_error.code, "AU4007");
    assert_eq!(
        count_error.message,
        "array shape [2] requires 2 values, but storage contains 1"
    );

    let valid_allocation_error = super::with_array_allocation_budget(0, || {
        ArrayValue::from_vec(&malformed_source, Some(&[1]))
    })
    .expect_err("a valid shape should proceed to fallible allocation");
    assert_eq!(valid_allocation_error.code, "AU4005");
    assert_eq!(
        valid_allocation_error.message,
        "Array.from_list shape could not allocate storage for 1 array elements"
    );

    let conversion_error = ArrayValue::from_vec(&malformed_source, Some(&[1]))
        .expect_err("valid shapes should proceed to exact-dtype element conversion");
    assert_eq!(conversion_error.code, "AU4007");
    assert_eq!(
        conversion_error.message,
        "array int32 storage requires int32 scalar at flat index 0, found 7"
    );
}

#[test]
fn array_construction_rejects_unsupported_types_and_inexact_scalar_metadata() {
    let unsupported = VecValue {
        element_type: Type::named("str"),
        elements: vec![Value::String("one".to_string())],
    };
    let from_vec_error = ArrayValue::from_vec(&unsupported, None)
        .expect_err("Array.from_list must reject non-numeric element types");
    assert_eq!(from_vec_error.code, "AU4007");
    assert_eq!(
        from_vec_error.message,
        "Array values require int32, int64, float32, or float64 elements, found `str`"
    );

    let from_values_error = ArrayValue::from_values(
        &Type::Tuple(vec![Type::named("int32")]),
        vec![1].into_boxed_slice(),
        vec![Value::Int(IntegerValue::from_i32(1))],
    )
    .expect_err("the native Array constructor must enforce the same numeric type boundary");
    assert_eq!(from_values_error.code, "AU4007");
    assert_eq!(
        from_values_error.message,
        "Array values require int32, int64, float32, or float64 elements, found `(int32,)`"
    );

    assert_eq!(
        ArrayDType::from_type(&Type::Named("int32".to_string(), vec![Type::named("str")])),
        None,
        "numeric Array dtypes are exact non-generic scalar types"
    );
    assert_eq!(ArrayDType::from_type(&Type::Unit), None);

    for (element_type, value, expected) in [
        (
            Type::named("int32"),
            Value::Bool(true),
            "array int32 storage requires int32 scalar at flat index 0, found true",
        ),
        (
            Type::named("int64"),
            Value::Int(IntegerValue::from_i32(7)),
            "array int64 storage requires int64 scalar at flat index 0, found 7",
        ),
        (
            Type::named("float32"),
            Value::Int(IntegerValue::from_i32(3)),
            "array float32 storage requires float32 scalar at flat index 0, found 3",
        ),
        (
            Type::named("float64"),
            Value::String("3.0".to_string()),
            "array float64 storage requires float64 scalar at flat index 0, found 3.0",
        ),
    ] {
        let error = ArrayValue::from_values(&element_type, vec![1].into_boxed_slice(), vec![value])
            .expect_err("Array storage must preserve exact scalar runtime metadata");
        assert_eq!(error.code, "AU4007");
        assert_eq!(error.message, expected);
    }

    let mut int64_array = ArrayValue::zeros(ArrayDType::Int64, vec![1].into_boxed_slice())
        .expect("the dynamic-result diagnostic probe should construct its destination Array");
    let callback_result_error = int64_array
        .fill(Value::String("not an int64".to_string()))
        .expect_err("a dynamically produced non-integer cannot enter int64 Array storage");
    assert_eq!(callback_result_error.code, "AU4007");
    assert_eq!(
        callback_result_error.message,
        "array int64 storage requires int64 scalar at flat index 0, found not an int64"
    );
}

#[test]
fn array_containing_language_copies_preserve_reachable_structure_and_independence() {
    fn source_array(values: &[i32]) -> Value {
        Value::Array(
            ArrayValue::new(
                vec![values.len()].into_boxed_slice(),
                ArrayStorage::Int32(values.to_vec().into_boxed_slice()),
            )
            .unwrap(),
        )
    }

    fn nested_arrays(value: &Value) -> Vec<&ArrayValue> {
        let mut pending = vec![value];
        let mut arrays = Vec::new();
        while let Some(value) = pending.pop() {
            match value {
                Value::Array(array) => arrays.push(array),
                Value::Vec(vector) => pending.extend(vector.elements.iter().rev()),
                Value::Tuple(tuple) => pending.extend(tuple.elements.iter().rev()),
                Value::Map(map) => {
                    pending.extend(map.entries.iter().rev().map(|(_, value)| value));
                }
                Value::Instance(instance) => pending.extend(instance.fields.values().rev()),
                Value::EnumVariant(variant) => pending.extend(variant.payloads.iter().rev()),
                _ => {}
            }
        }
        arrays
    }

    let array_type = Type::Named("Array".to_string(), vec![Type::named("int32")]);
    let cases = vec![
        Value::Vec(VecValue {
            element_type: array_type.clone(),
            elements: vec![source_array(&[4, 9]), source_array(&[1, 7])],
        }),
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("str"), array_type.clone(), Type::named("int32")],
            elements: vec![
                Value::String("before".to_string()),
                source_array(&[2, 5]),
                Value::Int(IntegerValue::from_i32(8)),
            ],
        }),
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: array_type.clone(),
            entries: vec![
                (Value::String("second".to_string()), source_array(&[6, 3])),
                (Value::String("first".to_string()), source_array(&[5, 2])),
            ],
        }),
        Value::Instance(InstanceValue {
            class_name: "ArrayBox".to_string(),
            fields: BTreeMap::from([
                ("left".to_string(), source_array(&[11, 13])),
                ("right".to_string(), source_array(&[17, 19])),
            ]),
        }),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Option".to_string(),
            variant_name: "Some".to_string(),
            payloads: vec![source_array(&[23, 29])],
        }),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Result".to_string(),
            variant_name: "Ok".to_string(),
            payloads: vec![source_array(&[31, 37])],
        }),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "TaggedArray".to_string(),
            variant_name: "Tagged".to_string(),
            payloads: vec![
                Value::String("before".to_string()),
                source_array(&[41, 43]),
                Value::Int(IntegerValue::from_i32(47)),
            ],
        }),
    ];

    for source in cases {
        let mut copy = super::try_clone_array_containing_value(&source)
            .expect("reachable Array-containing language containers should clone recursively");
        assert_eq!(
            copy, source,
            "copying must preserve container metadata, child order, and scalar values"
        );

        let source_arrays = nested_arrays(&source);
        let copy_arrays = nested_arrays(&copy);
        assert_eq!(source_arrays.len(), copy_arrays.len());
        assert!(
            !source_arrays.is_empty(),
            "every case should contain at least one reachable Array"
        );
        for (source_array, copy_array) in source_arrays.into_iter().zip(copy_arrays) {
            assert_ne!(source_array.shape.as_ptr(), copy_array.shape.as_ptr());
            let (ArrayStorage::Int32(source_storage), ArrayStorage::Int32(copy_storage)) =
                (&source_array.storage, &copy_array.storage)
            else {
                panic!("test arrays should retain int32 storage");
            };
            assert_ne!(source_storage.as_ptr(), copy_storage.as_ptr());
        }

        if let Value::Vec(vector) = &mut copy {
            let Value::Array(array) = &mut vector.elements[0] else {
                panic!("the first Vec element should remain an Array");
            };
            array
                .set(&[0], Value::Int(IntegerValue::from_i32(101)))
                .expect("the copied nested Array should remain mutable");
            let Value::Vec(source_vector) = &source else {
                unreachable!("the copy retained the source Vec variant");
            };
            let Value::Array(source_array) = &source_vector.elements[0] else {
                unreachable!("the source Vec contains an Array");
            };
            assert_eq!(
                source_array.get(&[0]).unwrap(),
                Value::Int(IntegerValue::from_i32(4)),
                "mutating a copied child Array must not alter its source"
            );
        }
    }
}

#[test]
fn array_containing_language_copies_preserve_empty_containers() {
    let array_type = Type::Named("Array".to_string(), vec![Type::named("int32")]);
    let empty_values = vec![
        Value::Vec(VecValue {
            element_type: array_type.clone(),
            elements: Vec::new(),
        }),
        Value::Tuple(TupleValue {
            element_types: Vec::new(),
            elements: Vec::new(),
        }),
        Value::Set(SetValue {
            element_type: Type::named("str"),
            elements: Vec::new(),
        }),
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: array_type,
            entries: Vec::new(),
        }),
        Value::Instance(InstanceValue {
            class_name: "EmptyArrayBox".to_string(),
            fields: BTreeMap::new(),
        }),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Option".to_string(),
            variant_name: "None".to_string(),
            payloads: Vec::new(),
        }),
    ];

    for source in empty_values {
        let copy = super::try_clone_array_containing_value(&source)
            .expect("empty reachable containers should copy successfully");
        assert_eq!(
            copy, source,
            "empty copies must retain their runtime type metadata and variant identity"
        );
    }
}

#[test]
fn array_aware_language_copy_preserves_reachable_scalar_set_order() {
    let source = Value::Set(SetValue {
        element_type: Type::named("str"),
        elements: vec![
            Value::String("third".to_string()),
            Value::String("first".to_string()),
            Value::String("second".to_string()),
        ],
    });

    let copy = super::try_clone_array_containing_value(&source)
        .expect("a source-reachable scalar Set should copy successfully");
    let Value::Set(copy) = copy else {
        panic!("copying a Set must retain its runtime variant");
    };
    assert_eq!(copy.element_type, Type::named("str"));
    assert_eq!(
        copy.elements,
        vec![
            Value::String("third".to_string()),
            Value::String("first".to_string()),
            Value::String("second".to_string()),
        ],
        "Set storage order must remain stable while rebuilding a copy"
    );
}

#[test]
fn array_containing_language_copy_reports_outer_shape_and_storage_allocation_failures() {
    let array_type = Type::Named("Array".to_string(), vec![Type::named("int32")]);
    let vector = Value::Vec(VecValue {
        element_type: array_type,
        elements: vec![Value::Array(
            ArrayValue::new(
                vec![2].into_boxed_slice(),
                ArrayStorage::Int32(vec![4, 9].into_boxed_slice()),
            )
            .unwrap(),
        )],
    });

    let expected_failures = [
        (
            0,
            "Array-containing Vec copy could not allocate storage for 1 array elements",
        ),
        (
            1,
            "Array shape could not allocate storage for 1 array elements",
        ),
        (
            2,
            "Array.clone could not allocate storage for 2 array elements",
        ),
    ];
    for (successful_allocations, expected_message) in expected_failures {
        let error = super::with_array_allocation_budget(successful_allocations, || {
            super::try_clone_array_containing_value(&vector)
        })
        .expect_err("every Array-bearing copy allocation stage must fail recoverably");
        assert_eq!(error.code, "AU4005");
        assert_eq!(error.message, expected_message);
    }

    let Value::Vec(source_vector) = &vector else {
        unreachable!("the source is a Vec");
    };
    let slice = super::with_array_allocation_budget(3, || {
        super::slice_vec_owned(source_vector, None, None)
    })
    .expect("a sufficient allocation budget should produce an owned Vec slice");
    let (Value::Array(source_array), Value::Array(slice_array)) =
        (&source_vector.elements[0], &slice.elements[0])
    else {
        panic!("list[Array[T]] slices should retain their element values");
    };
    let (ArrayStorage::Int32(source_storage), ArrayStorage::Int32(slice_storage)) =
        (&source_array.storage, &slice_array.storage)
    else {
        panic!("test arrays should retain int32 storage");
    };
    assert_ne!(
        source_storage.as_ptr(),
        slice_storage.as_ptr(),
        "a successful Vec slice must own its nested Array storage"
    );
}

#[test]
fn adr0038_partial_capture_copies_release_every_projected_value_after_failure() {
    fn channel_value(channel: &ChannelValue) -> Value {
        Value::Channel(channel.clone())
    }

    fn cleanup_tree(channel: &ChannelValue) -> Value {
        Value::Tuple(TupleValue {
            element_types: vec![
                Type::Named("list".to_string(), vec![Type::named("CleanupNode")]),
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("Queue")],
                ),
            ],
            elements: vec![
                Value::Vec(VecValue {
                    element_type: Type::named("CleanupNode"),
                    elements: vec![Value::Set(SetValue {
                        element_type: Type::named("CleanupNode"),
                        elements: vec![Value::Instance(InstanceValue {
                            class_name: "CleanupNode".to_string(),
                            fields: BTreeMap::from([(
                                "payload".to_string(),
                                Value::EnumVariant(EnumVariantValue {
                                    enum_name: "CleanupPayload".to_string(),
                                    variant_name: "Queue".to_string(),
                                    payloads: vec![channel_value(channel)],
                                }),
                            )]),
                        })],
                    })],
                }),
                Value::Map(MapValue {
                    key_type: Type::named("str"),
                    value_type: Type::named("Queue"),
                    entries: vec![(Value::String("queue".to_string()), channel_value(channel))],
                }),
            ],
        })
    }

    fn failing_array() -> Value {
        Value::Array(
            ArrayValue::new(
                vec![1].into_boxed_slice(),
                ArrayStorage::Int32(vec![7].into_boxed_slice()),
            )
            .expect("the source Array should be valid before clone-failure injection"),
        )
    }

    fn assert_failure_releases_partial_copies(
        source: Value,
        channel: &ChannelValue,
        successful_allocations: usize,
        expected_message: &str,
    ) {
        let environment = ClosureEnvironment::new(
            vec![ClosureCaptureValue {
                name: "state".to_string(),
                ty: Type::named("CaptureState"),
                value: source,
                source_place: Some("state".to_string()),
                mutable: true,
            }],
            false,
        );
        let baseline = Arc::strong_count(&channel.inner);
        assert!(
            baseline > 1,
            "the source fixture must retain at least one observable Queue value"
        );

        let error = super::with_array_allocation_budget(successful_allocations + 1, || {
            environment.arguments("main::__lambda_projected_capture")
        })
        .expect_err("the selected nested allocation must fail recoverably");
        assert_eq!(error.code, "AU4005");
        assert_eq!(error.message, expected_message);
        assert_eq!(
            Arc::strong_count(&channel.inner),
            baseline,
            "all Queue aliases created before the failure must be released exactly once"
        );

        let recovered = environment
            .arguments("main::__lambda_projected_capture")
            .expect("a failed repeatable capture copy must leave its source environment live");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].source_place.as_deref(), Some("state"));
        assert!(recovered[0].mutable);
        drop(recovered);
        assert_eq!(
            Arc::strong_count(&channel.inner),
            baseline,
            "dropping a successful retry must release each projected Queue alias once"
        );
    }

    let tuple_channel = ChannelValue::new();
    assert_failure_releases_partial_copies(
        Value::Tuple(TupleValue {
            element_types: vec![
                Type::named("CaptureState"),
                Type::Named("Array".to_string(), vec![Type::named("int32")]),
            ],
            elements: vec![cleanup_tree(&tuple_channel), failing_array()],
        }),
        &tuple_channel,
        8,
        "Array shape could not allocate storage for 1 array elements",
    );

    let set_channel = ChannelValue::new();
    let tuple_capture = || {
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("Queue")],
            elements: vec![channel_value(&set_channel)],
        })
    };
    assert_failure_releases_partial_copies(
        Value::Set(SetValue {
            element_type: Type::Tuple(vec![Type::named("Queue")]),
            elements: vec![tuple_capture(), tuple_capture()],
        }),
        &set_channel,
        3,
        "Array-containing tuple type copy could not allocate storage for 1 array elements",
    );

    let map_key_channel = ChannelValue::new();
    let integer_key = |value| {
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("int32")],
            elements: vec![Value::Int(IntegerValue::from_i32(value))],
        })
    };
    assert_failure_releases_partial_copies(
        Value::Map(MapValue {
            key_type: Type::Tuple(vec![Type::named("int32")]),
            value_type: Type::named("Queue"),
            entries: vec![
                (integer_key(1), channel_value(&map_key_channel)),
                (integer_key(2), channel_value(&map_key_channel)),
            ],
        }),
        &map_key_channel,
        3,
        "Array-containing tuple type copy could not allocate storage for 1 array elements",
    );

    let map_value_channel = ChannelValue::new();
    let queue_tuple = || {
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("Queue")],
            elements: vec![channel_value(&map_value_channel)],
        })
    };
    assert_failure_releases_partial_copies(
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::Tuple(vec![Type::named("Queue")]),
            entries: vec![
                (Value::String("first".to_string()), queue_tuple()),
                (Value::String("second".to_string()), queue_tuple()),
            ],
        }),
        &map_value_channel,
        3,
        "Array-containing tuple type copy could not allocate storage for 1 array elements",
    );

    let instance_channel = ChannelValue::new();
    assert_failure_releases_partial_copies(
        Value::Instance(InstanceValue {
            class_name: "CaptureState".to_string(),
            fields: BTreeMap::from([
                ("a_state".to_string(), cleanup_tree(&instance_channel)),
                ("z_values".to_string(), failing_array()),
            ]),
        }),
        &instance_channel,
        6,
        "Array shape could not allocate storage for 1 array elements",
    );
}

#[test]
fn deeply_recursive_reachable_array_copy_completes_on_a_512_kib_stack() {
    const DEPTH: usize = 4_096;
    const TEST_STACK_BYTES: usize = 512 * 1024;

    std::thread::Builder::new()
        .name("deep-array-copy".to_string())
        .stack_size(TEST_STACK_BYTES)
        .spawn(|| {
            let mut source = Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "None".to_string(),
                payloads: Vec::new(),
            });
            for value in (0..DEPTH).rev() {
                source = Value::EnumVariant(EnumVariantValue {
                    enum_name: "Option".to_string(),
                    variant_name: "Some".to_string(),
                    payloads: vec![Value::Instance(InstanceValue {
                        class_name: "ArrayNode".to_string(),
                        fields: BTreeMap::from([
                            ("next".to_string(), source),
                            (
                                "values".to_string(),
                                Value::Array(
                                    ArrayValue::new(
                                        vec![1].into_boxed_slice(),
                                        ArrayStorage::Int32(vec![value as i32].into_boxed_slice()),
                                    )
                                    .unwrap(),
                                ),
                            ),
                        ]),
                    })],
                });
            }

            let source = std::mem::ManuallyDrop::new(source);
            let copy = std::mem::ManuallyDrop::new(
                super::try_clone_array_containing_value(&source)
                    .expect("recursive indirect class values should clone without recursion"),
            );
            let mut source_cursor: &Value = &source;
            let mut copy_cursor: &Value = &copy;
            for expected in 0..DEPTH {
                let (Value::EnumVariant(source_option), Value::EnumVariant(copy_option)) =
                    (source_cursor, copy_cursor)
                else {
                    panic!("each recursive link should remain an Option");
                };
                assert_eq!(source_option.variant_name, "Some");
                assert_eq!(copy_option.variant_name, "Some");
                let (Value::Instance(source_node), Value::Instance(copy_node)) =
                    (&source_option.payloads[0], &copy_option.payloads[0])
                else {
                    panic!("each present Option should retain its ArrayNode");
                };
                let (Some(Value::Array(source_array)), Some(Value::Array(copy_array))) = (
                    source_node.fields.get("values"),
                    copy_node.fields.get("values"),
                ) else {
                    panic!("each ArrayNode should retain its values field");
                };
                assert_eq!(
                    copy_array.get(&[0]).unwrap(),
                    Value::Int(IntegerValue::from_i32(expected as i32)),
                    "deep copies must preserve every node's payload order"
                );
                let (ArrayStorage::Int32(source_storage), ArrayStorage::Int32(copy_storage)) =
                    (&source_array.storage, &copy_array.storage)
                else {
                    panic!("test arrays should retain int32 storage");
                };
                assert_ne!(
                    source_storage.as_ptr(),
                    copy_storage.as_ptr(),
                    "every deeply nested Array must own independent storage"
                );
                source_cursor = source_node
                    .fields
                    .get("next")
                    .expect("each ArrayNode should retain its next field");
                copy_cursor = copy_node
                    .fields
                    .get("next")
                    .expect("each copied ArrayNode should retain its next field");
            }

            for cursor in [source_cursor, copy_cursor] {
                let Value::EnumVariant(option) = cursor else {
                    panic!("the recursive chain should terminate with Option.None");
                };
                assert_eq!(option.variant_name, "None");
                assert!(option.payloads.is_empty());
            }
        })
        .expect("the fixed-stack clone test thread should start")
        .join()
        .expect("deep iterative clone should not overflow a 512 KiB stack");
}

#[test]
fn array_clone_failure_drops_a_completed_deep_sibling_on_a_small_stack() {
    const HELPER_ENV: &str = "AURA_ARRAY_CLONE_FAILURE_DROP_HELPER";
    const DEPTH: usize = 20_000;

    if std::env::var_os(HELPER_ENV).is_some() {
        thread::Builder::new()
            .name("aura-array-clone-failure-drop".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut deep_sibling = Value::Unit;
                for _ in 0..DEPTH {
                    deep_sibling = Value::EnumVariant(EnumVariantValue {
                        enum_name: "CloneFailureProbe".to_string(),
                        variant_name: "Next".to_string(),
                        payloads: vec![deep_sibling],
                    });
                }
                let source = Value::EnumVariant(EnumVariantValue {
                    enum_name: "CloneFailureProbe".to_string(),
                    variant_name: "Pair".to_string(),
                    payloads: vec![
                        deep_sibling,
                        Value::Array(
                            ArrayValue::new(
                                vec![1].into_boxed_slice(),
                                ArrayStorage::Int32(vec![7].into_boxed_slice()),
                            )
                            .unwrap(),
                        ),
                    ],
                });

                let error = super::with_array_allocation_budget(DEPTH + 1, || {
                    super::try_clone_array_containing_value(&source)
                })
                .expect_err(
                    "the Array sibling should fail after the earlier deep sibling was rebuilt",
                );
                assert_eq!(error.code, "AU4005");

                // The clone-state destructor asserts every cleanup push fits its fallibly
                // pre-reserved stack. The source is retained because this regression exercises
                // that no-allocation cleanup, not Rust's recursive drop for the fixture.
                std::mem::forget(source);
            })
            .expect("small-stack clone-failure helper should spawn")
            .join()
            .expect("clone failure must clean partial deep values without overflowing");
        return;
    }

    let output = std::process::Command::new(
        std::env::current_exe().expect("current test binary should exist"),
    )
    .arg("--exact")
    .arg(
        "runtime_value::tests::array_clone_failure_drops_a_completed_deep_sibling_on_a_small_stack",
    )
    .arg("--nocapture")
    .env(HELPER_ENV, "1")
    .output()
    .expect("clone-failure helper process should run");

    assert!(
        output.status.success(),
        "clone-failure cleanup must be stack bounded and use only pre-reserved storage; child stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dense_array_kernels_cover_checked_arithmetic_broadcast_and_modes() {
    let left = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int32(vec![i32::MAX, -2].into_boxed_slice()),
    )
    .unwrap();
    let right = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int32(vec![1, 3].into_boxed_slice()),
    )
    .unwrap();
    let overflow = left
        .binary(&right, ArrayBinaryOp::Add, IntegerArithmeticMode::Checked)
        .expect_err("checked integer kernels must trap before exposing output");
    assert_eq!(overflow.code, "AU4002");
    assert!(overflow.message.contains("flat index 0"));

    assert_eq!(
        left.binary(&right, ArrayBinaryOp::Add, IntegerArithmeticMode::Wrapping)
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int32(vec![i32::MIN, 1].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        left.binary(
            &right,
            ArrayBinaryOp::Add,
            IntegerArithmeticMode::Saturating
        )
        .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int32(vec![i32::MAX, 1].into_boxed_slice()),
        )
        .unwrap()
    );

    let floats = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float32(vec![2.0, 4.0].into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        floats
            .scalar_binary(
                &Value::Float(2.0),
                false,
                ArrayBinaryOp::Div,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float32(vec![1.0, 2.0].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        floats
            .scalar_binary(
                &Value::Float(8.0),
                true,
                ArrayBinaryOp::Sub,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float32(vec![6.0, 4.0].into_boxed_slice()),
        )
        .unwrap()
    );
    let zero = floats
        .scalar_binary(
            &Value::Float(0.0),
            false,
            ArrayBinaryOp::Div,
            IntegerArithmeticMode::Checked,
        )
        .expect_err("float division by zero must retain scalar diagnostics");
    assert_eq!(zero.code, "AU4004");

    let invalid_mode = floats
        .scalar_binary(
            &Value::Float(1.0),
            false,
            ArrayBinaryOp::Add,
            IntegerArithmeticMode::Wrapping,
        )
        .expect_err("defensive runtime mode validation should reject wrapping floats");
    assert_eq!(invalid_mode.code, "AU4001");

    let invalid_division = left
        .scalar_binary(
            &Value::Int(IntegerValue::from_i32(1)),
            false,
            ArrayBinaryOp::Div,
            IntegerArithmeticMode::Checked,
        )
        .expect_err("defensive runtime operation validation should reject integer division");
    assert_eq!(invalid_division.code, "AU4001");

    let shape_error = left
        .binary(
            &ArrayValue::new(
                vec![1, 2].into_boxed_slice(),
                ArrayStorage::Int32(vec![1, 2].into_boxed_slice()),
            )
            .unwrap(),
            ArrayBinaryOp::Sub,
            IntegerArithmeticMode::Checked,
        )
        .expect_err("equal lengths do not make unequal shapes broadcast-compatible");
    assert_eq!(shape_error.code, "AU4007");
}

#[test]
fn dense_array_arithmetic_preserves_dtype_operand_order_and_integer_modes() {
    fn int32_array(values: &[i32]) -> ArrayValue {
        ArrayValue::new(
            vec![values.len()].into_boxed_slice(),
            ArrayStorage::Int32(values.to_vec().into_boxed_slice()),
        )
        .unwrap()
    }

    let integer_left = int32_array(&[i32::MIN, 4]);
    let integer_right = int32_array(&[1, 3]);
    let multiplication_left = int32_array(&[i32::MAX, 4]);
    let multiplication_right = int32_array(&[2, 3]);
    for (left, right, mode, operation, expected) in [
        (
            &integer_left,
            &integer_right,
            IntegerArithmeticMode::Wrapping,
            ArrayBinaryOp::Sub,
            int32_array(&[i32::MAX, 1]),
        ),
        (
            &multiplication_left,
            &multiplication_right,
            IntegerArithmeticMode::Wrapping,
            ArrayBinaryOp::Mul,
            int32_array(&[-2, 12]),
        ),
        (
            &integer_left,
            &integer_right,
            IntegerArithmeticMode::Saturating,
            ArrayBinaryOp::Sub,
            int32_array(&[i32::MIN, 1]),
        ),
        (
            &multiplication_left,
            &multiplication_right,
            IntegerArithmeticMode::Saturating,
            ArrayBinaryOp::Mul,
            int32_array(&[i32::MAX, 12]),
        ),
    ] {
        assert_eq!(
            left.binary(right, operation, mode)
                .expect("wrapping and saturating integer modes should return exact values"),
            expected
        );
    }

    for (operation, expected_message) in [
        (
            ArrayBinaryOp::Sub,
            "array subtraction overflowed at flat index 0",
        ),
        (
            ArrayBinaryOp::Mul,
            "array multiplication overflowed at flat index 0",
        ),
    ] {
        let right = if operation == ArrayBinaryOp::Sub {
            int32_array(&[1, 1])
        } else {
            int32_array(&[2, 1])
        };
        let left = if operation == ArrayBinaryOp::Sub {
            &integer_left
        } else {
            &multiplication_left
        };
        let error = left
            .binary(&right, operation, IntegerArithmeticMode::Checked)
            .expect_err("checked subtraction and multiplication must diagnose overflow");
        assert_eq!(error.code, "AU4002");
        assert_eq!(error.message, expected_message);
    }

    let int64_left = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int64(vec![6, -4].into_boxed_slice()),
    )
    .unwrap();
    let int64_right = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int64(vec![5, 3].into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        int64_left
            .binary(
                &int64_right,
                ArrayBinaryOp::Mul,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int64(vec![30, -12].into_boxed_slice()),
        )
        .unwrap()
    );
    assert_eq!(
        int64_left
            .scalar_binary(
                &Value::Int(IntegerValue::from_i64(10)),
                true,
                ArrayBinaryOp::Sub,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int64(vec![4, 14].into_boxed_slice()),
        )
        .unwrap(),
        "a scalar on the left must retain operand order"
    );
    assert_eq!(
        int64_left
            .scalar_binary(
                &Value::Int(IntegerValue::from_i64(2)),
                false,
                ArrayBinaryOp::Mul,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Int64(vec![12, -8].into_boxed_slice()),
        )
        .unwrap()
    );
    let int64_scalar_overflow = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int64(vec![3, i64::MAX].into_boxed_slice()),
    )
    .unwrap()
    .scalar_binary(
        &Value::Int(IntegerValue::from_i64(2)),
        false,
        ArrayBinaryOp::Mul,
        IntegerArithmeticMode::Checked,
    )
    .expect_err("checked int64 scalar arithmetic must report the first overflowing element");
    assert_eq!(int64_scalar_overflow.code, "AU4002");
    assert_eq!(
        int64_scalar_overflow.message,
        "array multiplication overflowed at flat index 1"
    );

    let float32_left = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float32(vec![8.0, 9.0].into_boxed_slice()),
    )
    .unwrap();
    let float32_right = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float32(vec![2.0, 3.0].into_boxed_slice()),
    )
    .unwrap();
    for (operation, expected) in [
        (ArrayBinaryOp::Add, [10.0_f32, 12.0]),
        (ArrayBinaryOp::Sub, [6.0_f32, 6.0]),
        (ArrayBinaryOp::Mul, [16.0_f32, 27.0]),
        (ArrayBinaryOp::Div, [4.0_f32, 3.0]),
    ] {
        assert_eq!(
            float32_left
                .binary(&float32_right, operation, IntegerArithmeticMode::Checked,)
                .unwrap(),
            ArrayValue::new(
                vec![2].into_boxed_slice(),
                ArrayStorage::Float32(expected.into()),
            )
            .unwrap()
        );
    }
    let float32_zero_divisor = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float32(vec![2.0, 0.0].into_boxed_slice()),
    )
    .unwrap();
    let float32_division_error = float32_left
        .binary(
            &float32_zero_divisor,
            ArrayBinaryOp::Div,
            IntegerArithmeticMode::Checked,
        )
        .expect_err("float32 Array division must report the first zero divisor");
    assert_eq!(float32_division_error.code, "AU4004");
    assert_eq!(
        float32_division_error.message,
        "array division has a zero divisor at flat index 1"
    );

    let float64_left = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float64(vec![12.0, 15.0].into_boxed_slice()),
    )
    .unwrap();
    let float64_right = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float64(vec![3.0, 5.0].into_boxed_slice()),
    )
    .unwrap();
    for (operation, expected) in [
        (ArrayBinaryOp::Add, [15.0_f64, 20.0]),
        (ArrayBinaryOp::Sub, [9.0_f64, 10.0]),
        (ArrayBinaryOp::Mul, [36.0_f64, 75.0]),
        (ArrayBinaryOp::Div, [4.0_f64, 3.0]),
    ] {
        assert_eq!(
            float64_left
                .binary(&float64_right, operation, IntegerArithmeticMode::Checked,)
                .unwrap(),
            ArrayValue::new(
                vec![2].into_boxed_slice(),
                ArrayStorage::Float64(expected.into()),
            )
            .unwrap()
        );
    }
    assert_eq!(
        float64_left
            .scalar_binary(
                &Value::Float(3.0),
                false,
                ArrayBinaryOp::Div,
                IntegerArithmeticMode::Checked,
            )
            .unwrap(),
        ArrayValue::new(
            vec![2].into_boxed_slice(),
            ArrayStorage::Float64(vec![4.0, 5.0].into_boxed_slice()),
        )
        .unwrap()
    );

    let zero_divisor = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Float64(vec![2.0, 0.0].into_boxed_slice()),
    )
    .unwrap();
    let error = float64_left
        .binary(
            &zero_divisor,
            ArrayBinaryOp::Div,
            IntegerArithmeticMode::Checked,
        )
        .expect_err("Array division by zero must report the first failing flat index");
    assert_eq!(error.code, "AU4004");
    assert_eq!(
        error.message,
        "array division has a zero divisor at flat index 1"
    );
}

#[test]
fn dense_array_reductions_define_empty_and_dtype_behavior() {
    let ints = ArrayValue::new(
        vec![2, 2].into_boxed_slice(),
        ArrayStorage::Int64(vec![4, -2, 7, 1].into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        ints.reduce(ArrayReduction::Sum).unwrap(),
        Value::Int(IntegerValue::from_i64(10))
    );
    assert_eq!(
        ints.reduce(ArrayReduction::Min).unwrap(),
        Value::Int(IntegerValue::from_i64(-2))
    );
    assert_eq!(
        ints.reduce(ArrayReduction::Max).unwrap(),
        Value::Int(IntegerValue::from_i64(7))
    );
    assert_eq!(
        ints.reduce(ArrayReduction::Mean).unwrap(),
        Value::Float(2.5)
    );

    let empty = ArrayValue::new(
        vec![0].into_boxed_slice(),
        ArrayStorage::Int32(Vec::new().into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        empty.reduce(ArrayReduction::Sum).unwrap(),
        Value::Int(IntegerValue::from_i32(0))
    );
    for reduction in [
        ArrayReduction::Min,
        ArrayReduction::Max,
        ArrayReduction::Mean,
    ] {
        let error = empty
            .reduce(reduction)
            .expect_err("empty non-identity reductions must be rejected");
        assert_eq!(error.code, "AU4007");
    }

    let overflow = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int32(vec![i32::MAX, 1].into_boxed_slice()),
    )
    .unwrap()
    .reduce(ArrayReduction::Sum)
    .expect_err("integer sum must retain checked arithmetic");
    assert_eq!(overflow.code, "AU4002");
    assert!(overflow.message.contains("flat index 1"));

    let int64_overflow = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int64(vec![i64::MAX, 1].into_boxed_slice()),
    )
    .unwrap()
    .reduce(ArrayReduction::Sum)
    .expect_err("int64 sums must retain checked arithmetic");
    assert_eq!(int64_overflow.code, "AU4002");
    assert_eq!(
        int64_overflow.message,
        "array addition overflowed at flat index 1"
    );

    let wide_mean = ArrayValue::new(
        vec![2].into_boxed_slice(),
        ArrayStorage::Int32(vec![i32::MAX, i32::MAX].into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        wide_mean.reduce(ArrayReduction::Mean).unwrap(),
        Value::Float(i32::MAX as f64),
        "mean must accumulate independently in f64 instead of overflowing a same-dtype sum"
    );
    assert_eq!(
        ArrayValue::new(
            vec![3].into_boxed_slice(),
            ArrayStorage::Int32(vec![7, -4, 2].into_boxed_slice()),
        )
        .unwrap()
        .reduce(ArrayReduction::Min)
        .unwrap(),
        Value::Int(IntegerValue::from_i32(-4))
    );
    assert_eq!(
        ArrayValue::new(
            vec![3].into_boxed_slice(),
            ArrayStorage::Int32(vec![7, -4, 2].into_boxed_slice()),
        )
        .unwrap()
        .reduce(ArrayReduction::Max)
        .unwrap(),
        Value::Int(IntegerValue::from_i32(7))
    );

    let rounded_sum = ArrayValue::new(
        vec![3].into_boxed_slice(),
        ArrayStorage::Float32(vec![16_777_216.0, 1.0, -16_777_216.0].into_boxed_slice()),
    )
    .unwrap();
    assert_eq!(
        rounded_sum.reduce(ArrayReduction::Sum).unwrap(),
        Value::Float(0.0),
        "float32 sum must preserve left-to-right float32 rounding"
    );

    for (storage, expected_sum, expected_min, expected_max, expected_mean) in [
        (
            ArrayStorage::Float32(vec![1.5, -2.0, 6.5].into_boxed_slice()),
            6.0,
            -2.0,
            6.5,
            2.0,
        ),
        (
            ArrayStorage::Float64(vec![1.5, -2.0, 6.5].into_boxed_slice()),
            6.0,
            -2.0,
            6.5,
            2.0,
        ),
    ] {
        let array = ArrayValue::new(vec![3].into_boxed_slice(), storage).unwrap();
        for (reduction, expected) in [
            (ArrayReduction::Sum, expected_sum),
            (ArrayReduction::Min, expected_min),
            (ArrayReduction::Max, expected_max),
            (ArrayReduction::Mean, expected_mean),
        ] {
            assert_eq!(
                array.reduce(reduction).unwrap(),
                Value::Float(expected),
                "{reduction:?} must preserve the finite floating reduction contract"
            );
        }
    }

    for storage in [
        ArrayStorage::Int64(Vec::new().into_boxed_slice()),
        ArrayStorage::Float32(Vec::new().into_boxed_slice()),
        ArrayStorage::Float64(Vec::new().into_boxed_slice()),
    ] {
        let array = ArrayValue::new(vec![0].into_boxed_slice(), storage).unwrap();
        let expected = match array.dtype() {
            ArrayDType::Int64 => Value::Int(IntegerValue::from_i64(0)),
            ArrayDType::Float32 | ArrayDType::Float64 => Value::Float(0.0),
            ArrayDType::Int32 => unreachable!("the int32 empty identity is covered above"),
        };
        assert_eq!(
            array.reduce(ArrayReduction::Sum).unwrap(),
            expected,
            "every dtype must expose its exact empty-sum identity"
        );
    }

    for storage in [
        ArrayStorage::Float32(vec![1.0, f32::NAN, -2.0].into_boxed_slice()),
        ArrayStorage::Float64(vec![1.0, f64::NAN, -2.0].into_boxed_slice()),
    ] {
        let array = ArrayValue::new(vec![3].into_boxed_slice(), storage).unwrap();
        for reduction in [
            ArrayReduction::Sum,
            ArrayReduction::Min,
            ArrayReduction::Max,
            ArrayReduction::Mean,
        ] {
            let Value::Float(result) = array.reduce(reduction).unwrap() else {
                panic!("floating reductions must return floating scalars");
            };
            assert!(
                result.is_nan(),
                "{reduction:?} must propagate NaN explicitly"
            );
        }
    }
}

#[test]
fn owned_slice_helpers_normalize_once_reject_without_clamping_and_copy_values() {
    let vector = VecValue {
        element_type: Type::named("str"),
        elements: ["zero", "one", "two", "three"]
            .into_iter()
            .map(|value| Value::String(value.to_string()))
            .collect(),
    };

    let middle = slice_vec_owned(&vector, Some(-3), Some(-1))
        .expect("negative bounds should normalize once");
    assert_eq!(middle.element_type, Type::named("str"));
    assert_eq!(
        middle.elements,
        vec![
            Value::String("one".to_string()),
            Value::String("two".to_string())
        ]
    );
    assert_ne!(
        middle.elements.as_ptr(),
        vector.elements.as_ptr(),
        "a Vec slice must own fresh element storage"
    );

    let full = slice_vec_owned(&vector, None, None).expect("omitted bounds should select all");
    assert_eq!(full, vector);
    assert_ne!(
        full.elements.as_ptr(),
        vector.elements.as_ptr(),
        "even a full Vec slice must be a fresh owned value"
    );

    let unicode = "aé🎉e\u{301}";
    assert_eq!(
        slice_string_owned(unicode, Some(1), Some(-1))
            .expect("str bounds should count Unicode scalar values"),
        "é🎉e"
    );
    assert_eq!(
        slice_string_owned(unicode, None, None).expect("full str slice should succeed"),
        unicode
    );

    for (start, end, message) in [
        (
            Some(-6),
            None,
            "slice start `-6` normalizes to `-1`, outside `0..=5`",
        ),
        (None, Some(6), "slice end `6` is outside `0..=5`"),
        (
            Some(4),
            Some(2),
            "slice start `4` is greater than slice end `2`",
        ),
    ] {
        let error = slice_string_owned(unicode, start, end)
            .expect_err("invalid slice bounds must trap instead of clamping");
        assert_eq!(error.code, "AU4003");
        assert_eq!(error.message, message);
    }
}

#[test]
fn retry_runtime_policy_validates_host_limits_and_checked_doubling() {
    for (attempts, backoff, code) in [(0, 0, "AU4003"), (1, -1, "AU4001")] {
        let error = validate_retry_runtime_policy(attempts, backoff)
            .expect_err("invalid retry policy should be rejected");
        assert_eq!(error.code, code);
    }
    let unrepresentable = validate_retry_runtime_policy(1, i128::MAX)
        .expect_err("initial backoff must fit the host timer");
    assert_eq!(unrepresentable.code, "AU4001");
    validate_retry_runtime_policy(1, 0).expect("zero backoff is valid");

    assert_eq!(
        next_retry_runtime_backoff(10).expect("small backoff should double"),
        20
    );
    let duration_overflow = next_retry_runtime_backoff(i128::MAX)
        .expect_err("Duration arithmetic overflow must be diagnosed");
    assert_eq!(duration_overflow.code, "AU4002");
    let host_overflow = next_retry_runtime_backoff(i128::MAX / 2)
        .expect_err("host timer overflow must be diagnosed");
    assert_eq!(host_overflow.code, "AU4002");
}

#[test]
fn retry_host_dispatch_rejects_inexact_runtime_argument_types() {
    for (name, args, expected) in [
        (
            "control::__retry_validate",
            vec![Value::Bool(true), Value::Duration(0)],
            "control.retry max_attempts must be an int32",
        ),
        (
            "control::__retry_validate",
            vec![
                Value::Int(IntegerValue::from_i64(i64::MAX)),
                Value::Duration(0),
            ],
            "control.retry max_attempts must be an int32",
        ),
        (
            "control::__retry_validate",
            vec![
                Value::Int(IntegerValue::from_i32(2)),
                Value::Int(IntegerValue::from_i32(0)),
            ],
            "control.retry initial_backoff must be a Duration",
        ),
        (
            "control::__retry_next_backoff",
            vec![Value::String("1s".to_string())],
            "control.retry backoff must be a Duration",
        ),
    ] {
        let error = super::evaluate_host_builtin(name, args)
            .expect_err("retry host helpers must validate their erased runtime inputs");
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, expected);
    }

    assert_eq!(
        super::evaluate_host_builtin(
            "control::__retry_validate",
            vec![Value::Int(IntegerValue::from_i32(2)), Value::Duration(1)],
        )
        .expect("exact retry argument metadata should pass host validation"),
        Value::Unit
    );
    assert_eq!(
        super::evaluate_host_builtin("control::__retry_next_backoff", vec![Value::Duration(7)],)
            .expect("a valid retry backoff should double through host dispatch"),
        Value::Duration(14)
    );
}
use crate::ast::ReceiverKind;
use crate::diag::{Diagnostic, Span};
use crate::integer::IntegerValue;
use crate::runtime_config::BlockingIoPoolConfig;
use crate::runtime_reactor::{RuntimeReactor, WaitKey};
use crate::sema::{FunctionParamContract, Type};
use rcgen::generate_simple_self_signed;
use std::collections::BTreeMap;
use std::ffi::{c_void, OsStr};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use super::{
    read_all_with_fd_deadline, read_exact_with_fd_deadline, read_line_with_fd_deadline,
    read_some_with_fd_deadline, write_all_with_fd_deadline, TlsListenerValue, TlsStreamValue,
    UnixListenerValue, UnixStreamValue,
};
#[cfg(unix)]
use std::os::fd::AsRawFd;

#[test]
fn ffi_handles_keep_addresses_opaque_in_runtime_rendering_and_identity_operations() {
    assert!(
        FfiHandleValue::new("ffi.Token".to_string(), std::ptr::null_mut()).is_none(),
        "a null foreign pointer is never a valid opaque handle"
    );
    let pointer = 0x1234usize as *mut c_void;
    let handle =
        FfiHandleValue::new("ffi.Token".to_string(), pointer).expect("pointer is non-null");
    assert_eq!(handle.type_name(), "ffi.Token");
    assert_eq!(handle.as_ptr(), pointer);

    let value = Value::FfiHandle(handle);
    assert_eq!(value.render(), "<opaque ffi.Token>");
    let debug = format!("{value:?}");
    assert!(debug.contains("ffi.Token"));
    assert!(!debug.contains("1234"));
    assert!(!debug.contains("4660"));
    assert_ne!(
        value,
        value.clone(),
        "opaque handles have no source equality operation"
    );
}

struct TempDir {
    path: PathBuf,
}

struct AtomicReleaseGuard(Arc<AtomicBool>);

impl Drop for AtomicReleaseGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn wait_task_ready(task: &TaskValue) -> Result<Value, Diagnostic> {
    match task
        .wait_result_with_cancellation_observed(None, None)
        .expect("an omitted task timeout cannot overflow")
    {
        TaskWaitStatus::Ready(result) => result,
        TaskWaitStatus::Cancelled => Err(Diagnostic::new("task was cancelled")),
        TaskWaitStatus::TimedOut => Err(Diagnostic::new("task wait timed out")),
    }
}

#[cfg(target_pointer_width = "64")]
#[test]
fn lightweight_stack_allocator_reports_real_address_space_exhaustion() {
    let error = match super::allocate_lightweight_task_stack(usize::MAX / 2) {
        Ok(_) => panic!("an address-space-sized guarded stack reservation must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "AU4005");
    assert!(
        error
            .message
            .starts_with("failed to allocate Aura task stack:"),
        "the allocator failure must retain its resource context: {error}"
    );
}

fn assert_cast_source_type(value: Value, expected_source: &str) {
    let error = cast_numeric_value(value, &Type::named("int32"), None)
        .expect_err("non-numeric runtime values should not cast to integers");
    assert!(
        error
            .message
            .contains(&format!("found `{expected_source}` and `int32`")),
        "unexpected diagnostic for {expected_source}: {}",
        error.message
    );
}

fn assert_value_equals_clone(value: Value) {
    assert_eq!(value, value.clone());
}

fn function_signature(parameter_name: &str, has_default: bool, default_erased: bool) -> Type {
    Type::Function {
        params: vec![
            FunctionParamContract {
                name: parameter_name.to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Borrow,
                has_default,
                default_erased,
            },
            FunctionParamContract {
                name: "items".to_string(),
                ty: Type::Named("list".to_string(), vec![Type::named("int32")]),
                passing: ReceiverKind::BorrowMut,
                has_default: false,
                default_erased: false,
            },
            FunctionParamContract {
                name: "predicate".to_string(),
                ty: Type::Function {
                    params: vec![FunctionParamContract {
                        name: String::new(),
                        ty: Type::named("bool"),
                        passing: ReceiverKind::Value,
                        has_default: false,
                        default_erased: false,
                    }],
                    return_type: Box::new(Type::Unit),
                },
                passing: ReceiverKind::Value,
                has_default: false,
                default_erased: false,
            },
        ],
        return_type: Box::new(Type::Tuple(vec![Type::named("int64")])),
    }
}

#[test]
fn function_values_expose_structural_identity_rendering_cloning_and_cast_diagnostics() {
    let signature = function_signature("label", true, false);
    assert_eq!(
        signature.to_string(),
        "def(str, mut list[int32], own def(own bool) -> None) -> (int64,)"
    );

    let function = Value::Function(Box::new(FunctionValue {
        name: "support::transform".to_string(),
        signature: signature.clone(),
        source_path: Some("/workspace/support.au".to_string()),
        entry_span: Span::new(7, 3),
        direct_thunk: Some(11),
        direct_default_binder: Some(12),
        closure_environment: None,
    }));
    assert_eq!(function.render(), "<function support::transform>");

    let cloned = function.clone();
    let Value::Function(cloned_function) = &cloned else {
        panic!("cloning a function value must retain its runtime variant");
    };
    assert_eq!(
        (
            cloned_function.source_path.as_deref(),
            cloned_function.entry_span,
            cloned_function.direct_thunk,
            cloned_function.direct_default_binder,
        ),
        (
            Some("/workspace/support.au"),
            Span::new(7, 3),
            Some(11),
            Some(12),
        )
    );
    assert_eq!(function, cloned);

    let same_callable_contract_with_different_execution_metadata =
        Value::Function(Box::new(FunctionValue {
            name: "support::transform".to_string(),
            signature: function_signature("renamed", false, true),
            source_path: Some("/installed/support.au".to_string()),
            entry_span: Span::new(70, 30),
            direct_thunk: Some(101),
            direct_default_binder: None,
            closure_environment: None,
        }));
    assert_eq!(
        function, same_callable_contract_with_different_execution_metadata,
        "function identity ignores declaration-only parameter metadata and backend addresses"
    );

    let different_name = Value::Function(Box::new(FunctionValue {
        name: "support::other".to_string(),
        signature: signature.clone(),
        source_path: None,
        entry_span: Span::new(1, 1),
        direct_thunk: None,
        direct_default_binder: None,
        closure_environment: None,
    }));
    assert_ne!(function, different_name);

    let different_signature = Value::Function(Box::new(FunctionValue {
        name: "support::transform".to_string(),
        signature: Type::Function {
            params: Vec::new(),
            return_type: Box::new(Type::named("int64")),
        },
        source_path: None,
        entry_span: Span::new(1, 1),
        direct_thunk: None,
        direct_default_binder: None,
        closure_environment: None,
    }));
    assert_ne!(function, different_signature);
    assert_ne!(function, Value::Unit);

    let cast_error = cast_numeric_value(function, &Type::named("int32"), Some(Span::new(19, 8)))
        .expect_err("function values are not numeric cast sources");
    assert_eq!(
        cast_error.message,
        "casts are only supported between numeric types, found `def(str, mut list[int32], own def(own bool) -> None) -> (int64,)` and `int32`"
    );
    assert_eq!(cast_error.span, Some(Span::new(19, 8)));
}

#[test]
fn function_values_remain_observable_inside_structural_runtime_aggregates() {
    let signature = function_signature("value", false, false);
    let callbacks = Value::Vec(VecValue {
        element_type: signature.clone(),
        elements: ["first", "second"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                Value::Function(Box::new(FunctionValue {
                    name: format!("support::{name}"),
                    signature: signature.clone(),
                    source_path: Some("/workspace/support.au".to_string()),
                    entry_span: Span::new(index + 2, 5),
                    direct_thunk: Some(20 + index as i64),
                    direct_default_binder: None,
                    closure_environment: None,
                }))
            })
            .collect(),
    });

    assert_eq!(
        callbacks.render(),
        "[<function support::first>, <function support::second>]"
    );
    let cloned = callbacks.clone();
    assert_eq!(callbacks, cloned);
    let Value::Vec(cloned_callbacks) = cloned else {
        panic!("function collection should retain its Vec runtime shape");
    };
    assert_eq!(cloned_callbacks.element_type, signature);
    let [Value::Function(first), Value::Function(second)] = cloned_callbacks.elements.as_slice()
    else {
        panic!("function collection should retain both callable values");
    };
    assert_eq!(
        (
            first.entry_span,
            first.direct_thunk,
            second.entry_span,
            second.direct_thunk,
        ),
        (Span::new(2, 5), Some(20), Span::new(3, 5), Some(21))
    );
}

#[test]
fn closure_environments_share_identity_and_enforce_capture_ownership() {
    let capture_type = Type::named("int64");
    let captured = || ClosureCaptureValue {
        name: "offset".to_string(),
        ty: capture_type.clone(),
        value: Value::Int(IntegerValue::from_signed(9)),
        source_place: None,
        mutable: false,
    };

    let repeatable = Arc::new(ClosureEnvironment::new(vec![captured()], false));
    for invocation in 1..=2 {
        let arguments = repeatable
            .arguments("main::__lambda_repeatable")
            .unwrap_or_else(|error| panic!("repeatable invocation {invocation} failed: {error}"));
        let [argument] = arguments.as_slice() else {
            panic!("repeatable closure should expose its one captured argument");
        };
        assert_eq!(argument.name, "offset");
        assert_eq!(argument.ty, capture_type);
        assert_eq!(argument.value, Value::Int(IntegerValue::from_signed(9)));
    }

    let poisoned = Arc::new(ClosureEnvironment::new(vec![captured()], false));
    let poison_target = Arc::clone(&poisoned);
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = poison_target
            .captures
            .lock()
            .expect("fresh closure environment lock should be healthy");
        panic!("poison closure environment for recovery test");
    }));
    assert!(poison.is_err());
    let recovered = poisoned
        .arguments("main::__lambda_poisoned")
        .expect("a poisoned bookkeeping lock must not discard the captured environment");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].value, Value::Int(IntegerValue::from_signed(9)));

    let consuming = Arc::new(ClosureEnvironment::new(vec![captured()], true));
    let consuming_alias = Arc::clone(&consuming);
    let first = consuming
        .arguments("main::__lambda_consuming")
        .expect("the first consuming invocation owns the captures");
    assert_eq!(first[0].value, Value::Int(IntegerValue::from_signed(9)));
    let second = consuming_alias
        .arguments("main::__lambda_consuming")
        .expect_err("all aliases must observe that the environment was consumed");
    assert_eq!(
        second.message,
        "closure `main::__lambda_consuming` has already consumed its captured environment"
    );

    let signature = Type::Closure {
        params: Box::new(Vec::new()),
        return_type: Box::new(capture_type.clone()),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "offset".to_string(),
            ty: capture_type.clone(),
            mode: crate::sema::ClosureCaptureMode::Copy,
            span: Span::new(2, 41),
        }]),
        call_kind: crate::sema::ClosureCallKind::Repeatable,
    };
    let function = |environment: Option<Arc<ClosureEnvironment>>| {
        Value::Function(Box::new(FunctionValue {
            name: "main::__lambda_repeatable".to_string(),
            signature: signature.clone(),
            source_path: Some("/workspace/main.au".to_string()),
            entry_span: Span::new(2, 41),
            direct_thunk: Some(17),
            direct_default_binder: None,
            closure_environment: environment,
        }))
    };
    let shared = function(Some(Arc::clone(&repeatable)));
    let same_environment = function(Some(Arc::clone(&repeatable)));
    let distinct_environment = function(Some(Arc::new(ClosureEnvironment::new(
        vec![captured()],
        false,
    ))));
    let capture_free_storage = function(None);

    assert_eq!(
        shared, same_environment,
        "function clones that share one environment are the same runtime closure"
    );
    assert_eq!(shared, shared.clone());
    assert_ne!(
        shared, distinct_environment,
        "identical capture contents do not make distinct closure instances equal"
    );
    assert_ne!(
        shared, capture_free_storage,
        "closure identity must distinguish the presence of an environment"
    );
    assert_eq!(
        shared.render(),
        "<function main::__lambda_repeatable>",
        "rendering remains stable without exposing captured values"
    );
}

#[test]
fn adr0038_closure_environment_mutable_capture_helpers_cover_success_and_errors() {
    let environment = ClosureEnvironment::new(
        vec![
            ClosureCaptureValue {
                name: "items".to_string(),
                ty: Type::named("list[int64]"),
                value: Value::Vec(VecValue {
                    element_type: Type::named("int64"),
                    elements: vec![Value::Int(IntegerValue::from_signed(1))],
                }),
                source_place: Some("items".to_string()),
                mutable: true,
            },
            ClosureCaptureValue {
                name: "label".to_string(),
                ty: Type::named("str"),
                value: Value::String("fixed".to_string()),
                source_place: Some("label".to_string()),
                mutable: false,
            },
        ],
        false,
    );

    let arguments = environment
        .arguments("main::__lambda_mutable")
        .expect("repeatable capture arguments should clone their values");
    assert_eq!(arguments[0].source_place.as_deref(), Some("items"));
    assert!(arguments[0].mutable);
    assert!(!arguments[1].mutable);

    environment
        .write_back_mutable(
            0,
            Value::Vec(VecValue {
                element_type: Type::named("int64"),
                elements: vec![Value::Int(IntegerValue::from_signed(2))],
            }),
        )
        .expect("mutable captures accept writeback");
    assert_eq!(
        environment
            .capture_value(0)
            .expect("a live capture can be observed"),
        Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements: vec![Value::Int(IntegerValue::from_signed(2))],
        })
    );

    let immutable = environment
        .write_back_mutable(1, Value::String("changed".to_string()))
        .expect_err("shared captures reject mutable writeback");
    assert_eq!(immutable.message, "closure capture `label` is not mutable");
    assert_eq!(
        environment
            .write_back_mutable(9, Value::Unit)
            .expect_err("an unknown capture index must be diagnosed")
            .message,
        "closure has no capture at index 9"
    );
    assert_eq!(
        environment
            .capture_value(9)
            .expect_err("an unknown live capture must be diagnosed")
            .message,
        "closure has no live capture at index 9"
    );

    let unavailable = ClosureEnvironment::new(Vec::new(), true);
    *unavailable
        .captures
        .lock()
        .expect("fresh capture lock should be healthy") = None;
    assert_eq!(
        unavailable
            .arguments("main::__lambda_unavailable")
            .expect_err("a missing consuming environment must be diagnosed")
            .message,
        "closure `main::__lambda_unavailable` has already consumed its captured environment"
    );
    assert_eq!(
        unavailable
            .write_back_mutable(0, Value::Unit)
            .expect_err("a missing environment cannot accept writeback")
            .message,
        "cannot write back into a consumed closure environment"
    );
    assert_eq!(
        unavailable
            .capture_value(0)
            .expect_err("a missing environment has no live captures")
            .message,
        "closure has no live capture at index 0"
    );
}

#[test]
fn adr0038_closure_capture_access_recovers_poisoned_loan_bookkeeping() {
    fn mutable_capture(value: i64) -> ClosureCaptureValue {
        ClosureCaptureValue {
            name: "counter".to_string(),
            ty: Type::named("int64"),
            value: Value::Int(IntegerValue::from_i64(value)),
            source_place: Some("state.counter".to_string()),
            mutable: true,
        }
    }

    let repeatable = Arc::new(ClosureEnvironment::new(vec![mutable_capture(3)], false));
    let poison_target = Arc::clone(&repeatable);
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = poison_target
            .captures
            .lock()
            .expect("fresh capture bookkeeping should be healthy");
        panic!("poison capture bookkeeping");
    }));
    assert!(poison.is_err());

    repeatable
        .write_back_mutable(0, Value::Int(IntegerValue::from_i64(4)))
        .expect("a poisoned mutex must not discard mutable-capture writeback");
    assert_eq!(
        repeatable
            .capture_value(0)
            .expect("a poisoned mutex must not hide a live borrowed capture"),
        Value::Int(IntegerValue::from_i64(4))
    );
    let borrowed = repeatable
        .arguments("main::__lambda_borrowed")
        .expect("repeatable capture reads must recover the same live environment");
    assert_eq!(borrowed[0].source_place.as_deref(), Some("state.counter"));
    assert!(borrowed[0].mutable);
    assert_eq!(borrowed[0].value, Value::Int(IntegerValue::from_i64(4)));

    let consuming = Arc::new(ClosureEnvironment::new(vec![mutable_capture(7)], true));
    let poison_target = Arc::clone(&consuming);
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = poison_target
            .consumed
            .lock()
            .expect("fresh consumption bookkeeping should be healthy");
        panic!("poison consumption bookkeeping");
    }));
    assert!(poison.is_err());

    let owned = consuming
        .arguments("main::__lambda_once")
        .expect("poison recovery must preserve the first consuming capture read");
    assert_eq!(owned[0].value, Value::Int(IntegerValue::from_i64(7)));
    assert_eq!(
        consuming
            .capture_value(0)
            .expect("the consumed slot stays addressable for native mutable writeback"),
        Value::Unit,
        "moving a consuming capture must clear the environment before its writeback"
    );
    consuming
        .write_back_mutable(0, Value::Int(IntegerValue::from_i64(8)))
        .expect("the closure body may publish its final mutable capture value");
    assert_eq!(
        consuming
            .capture_value(0)
            .expect("the final mutable capture value remains observable for runtime cleanup"),
        Value::Int(IntegerValue::from_i64(8))
    );
    assert_eq!(
        consuming
            .arguments("main::__lambda_once")
            .expect_err("poison recovery must not permit a second consuming read")
            .message,
        "closure `main::__lambda_once` has already consumed its captured environment"
    );
}

#[test]
fn p63_embedded_nominal_types_preserve_base_value_identity_and_rendering() {
    let instance = |class_name: &str, runtime_type: &str| {
        Value::Instance(super::InstanceValue {
            class_name: class_name.to_string(),
            fields: BTreeMap::from([
                (
                    super::DIRECT_RUNTIME_TYPE_FIELD.to_string(),
                    Value::String(runtime_type.to_string()),
                ),
                (
                    "value".to_string(),
                    Value::Int(IntegerValue::from_signed(7)),
                ),
            ]),
        })
    };

    let integer = instance("Packet\0pkg.Packet[int64]", "pkg.Packet[int64]");
    let string = instance("Packet\0pkg.Packet[str]", "pkg.Packet[str]");
    let other = instance("Envelope\0pkg.Envelope[int64]", "pkg.Envelope[int64]");

    assert_eq!(
        integer, string,
        "generic runtime metadata is not part of structural instance equality"
    );
    assert_ne!(
        integer, other,
        "different nominal base types must never compare equal"
    );
    assert_eq!(integer.render(), "Packet(value=7)");
    assert_eq!(string.render(), "Packet(value=7)");
    assert_eq!(
        super::embedded_nominal_runtime_type_name("Packet\0pkg.Packet[int64]"),
        Some("pkg.Packet[int64]")
    );
    assert_eq!(super::embedded_nominal_runtime_type_name("Packet"), None);
}

#[test]
fn tuple_values_preserve_type_metadata_equality_and_rendering() {
    let pair = Value::Tuple(TupleValue {
        element_types: vec![Type::named("int64"), Type::named("str")],
        elements: vec![
            Value::Int(IntegerValue::from_signed(7)),
            Value::String("seven".to_string()),
        ],
    });
    assert_value_equals_clone(pair.clone());
    assert_eq!(pair.render(), "(7, seven)");

    let singleton = Value::Tuple(TupleValue {
        element_types: vec![Type::named("bool")],
        elements: vec![Value::Bool(true)],
    });
    assert_eq!(singleton.render(), "(true,)");

    let empty = Value::Tuple(TupleValue {
        element_types: Vec::new(),
        elements: Vec::new(),
    });
    assert_eq!(empty.render(), "()");
    assert_ne!(pair, singleton);
}

#[test]
fn tuple_value_equality_uses_elements_not_runtime_type_metadata() {
    let left = Value::Tuple(TupleValue {
        element_types: vec![Type::Tuple(vec![Type::named("int64")])],
        elements: vec![Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(7))],
        })],
    });
    let same_elements_different_metadata = Value::Tuple(TupleValue {
        element_types: vec![Type::Tuple(vec![Type::named("uint64")])],
        elements: vec![Value::Tuple(TupleValue {
            element_types: vec![Type::named("uint64")],
            elements: vec![Value::Int(IntegerValue::from_signed(7))],
        })],
    });
    let different_elements = Value::Tuple(TupleValue {
        element_types: vec![Type::Tuple(vec![Type::named("int64")])],
        elements: vec![Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(8))],
        })],
    });

    assert_eq!(
        left, same_elements_different_metadata,
        "tuple runtime metadata is not part of structural value equality"
    );
    assert_ne!(left, different_elements);
}

fn runtime_bytes(bytes: &[u8]) -> Value {
    Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: bytes
            .iter()
            .map(|byte| {
                Value::Int(
                    IntegerValue::from_typed_unsigned(
                        u128::from(*byte),
                        crate::integer::IntegerKind::Uint8,
                    )
                    .expect("every byte fits uint8"),
                )
            })
            .collect(),
    })
}

fn expect_runtime_bytes(value: &Value) -> Vec<u8> {
    super::host_bytes_from_runtime(value, "test byte value")
        .expect("runtime byte value should carry exact list[uint8] metadata")
}

#[test]
fn bytes_runtime_materialization_is_fallible_exact_and_non_consuming() {
    let original = runtime_bytes(&[0, 1, 127, 128, 255]);
    let snapshot = original.clone();
    assert_eq!(
        super::host_bytes_from_runtime(&original, "bytes::hex_encode")
            .expect("exact runtime bytes should materialize"),
        vec![0, 1, 127, 128, 255]
    );
    assert_eq!(original, snapshot);

    let materialized = super::runtime_bytes_from_host(&[0, 1, 127, 128, 255])
        .expect("host bytes should materialize");
    let Value::Vec(materialized_vec) = &materialized else {
        panic!("host bytes should materialize as list[uint8]");
    };
    assert_eq!(materialized_vec.element_type, Type::named("uint8"));
    assert_eq!(
        expect_runtime_bytes(&materialized),
        vec![0, 1, 127, 128, 255]
    );
    assert!(materialized_vec.elements.iter().all(|value| {
        matches!(
            value,
            Value::Int(value)
                if value.runtime_kind() == Some(crate::integer::IntegerKind::Uint8)
        )
    }));

    for operation in ["host", "runtime"] {
        let error = super::with_bytes_runtime_allocation_budget(0, || match operation {
            "host" => {
                super::host_bytes_from_runtime(&original, "bytes::sha256").map(|_| Value::Unit)
            }
            "runtime" => super::runtime_bytes_from_host(&[1]),
            _ => unreachable!(),
        })
        .expect_err("byte-buffer materialization allocation failure must trap");
        assert_eq!(error.code, "AU4005", "{operation}");
        assert_eq!(
            error.message, "memory allocation failed while materializing byte data",
            "{operation}"
        );
    }
}

#[test]
fn bytes_runtime_conversion_rejects_malformed_vec_uint8_values() {
    let not_a_vector = Value::String("not bytes".to_string());
    let wrong_type = Value::Vec(VecValue {
        element_type: Type::named("int32"),
        elements: Vec::new(),
    });
    let wrong_metadata = Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: vec![Value::Int(IntegerValue::from_literal(1))],
    });
    let wrong_element = Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: vec![Value::Bool(true)],
    });

    let error = super::host_bytes_from_runtime(&not_a_vector, "bytes::hex_encode")
        .expect_err("non-vector runtime values must be rejected");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "`bytes::hex_encode` expects a runtime `list[uint8]` value"
    );

    for value in [&wrong_type, &wrong_metadata, &wrong_element] {
        let error = super::host_bytes_from_runtime(value, "bytes::hex_encode")
            .expect_err("malformed list[uint8] runtime values must be rejected");
        assert_eq!(error.code, "AU4001");
        assert!(error.message.contains("`bytes::hex_encode`"));
        assert!(error.message.contains("`list[uint8]`"));
    }
}

#[test]
fn bytes_adapter_rejects_wrong_runtime_types_without_consuming_inputs() {
    let wrong = Value::Bool(true);
    for name in [
        "bytes::hex_decode",
        "bytes::base64_decode",
        "bytes::sha256_string",
        "str.to_bytes",
    ] {
        let error = super::evaluate_bytes_host_builtin_ref(name, &wrong)
            .expect("the byte builtin should be recognized")
            .expect_err("str-taking byte builtins must reject non-str values");
        assert_eq!(error.code, "AU2004", "{name}");
        assert_eq!(
            error.message,
            format!("`{name}` expects argument 1 to be `str`, found `true`"),
            "{name}"
        );
    }

    let error = super::evaluate_bytes_host_builtin_ref("str.from_bytes", &wrong)
        .expect("str.from_bytes should be recognized")
        .expect_err("str.from_bytes must reject non-byte-vector runtime values");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "`str.from_bytes` expects a runtime `list[uint8]` value"
    );
    assert_eq!(wrong, Value::Bool(true));
}

#[test]
fn bytes_data_errors_materialize_exact_typed_result_payloads() {
    use crate::bytes_codec::{BytesCodecError, BytesDataError};

    let cases = [
        (
            BytesDataError::InvalidUtf8 { index: 7 },
            "InvalidUtf8",
            vec![(7, crate::integer::IntegerKind::Int32)],
        ),
        (
            BytesDataError::InvalidHexLength { length: 9 },
            "InvalidHexLength",
            vec![(9, crate::integer::IntegerKind::Int32)],
        ),
        (
            BytesDataError::InvalidHexDigit {
                index: 3,
                byte: 0xfe,
            },
            "InvalidHexDigit",
            vec![
                (3, crate::integer::IntegerKind::Int32),
                (0xfe, crate::integer::IntegerKind::Uint8),
            ],
        ),
        (
            BytesDataError::InvalidBase64 { index: 4 },
            "InvalidBase64",
            vec![(4, crate::integer::IntegerKind::Int32)],
        ),
    ];

    for (error, expected_variant, expected_payloads) in cases {
        let value = super::bytes_codec_error_to_result(BytesCodecError::Data(error))
            .expect("data errors should be recoverable typed results");
        let Value::EnumVariant(result) = value else {
            panic!("codec data error should produce Result.Err");
        };
        assert_eq!(
            (result.enum_name.as_str(), result.variant_name.as_str()),
            ("Result", "Err")
        );
        let [Value::EnumVariant(error)] = result.payloads.as_slice() else {
            panic!("Result.Err should contain bytes.Error");
        };
        assert_eq!(
            (error.enum_name.as_str(), error.variant_name.as_str()),
            ("bytes.Error", expected_variant)
        );
        assert_eq!(error.payloads.len(), expected_payloads.len());
        for (payload, (expected_value, expected_kind)) in
            error.payloads.iter().zip(expected_payloads)
        {
            assert!(matches!(
                payload,
                Value::Int(value)
                    if value.as_i128() == Some(expected_value)
                        && value.runtime_kind() == Some(expected_kind)
            ));
        }
    }
}

#[test]
fn bytes_error_index_retains_the_int32_bytes_error_payload_boundary() {
    let payload_ceiling = i32::MAX as usize;
    let boundary = super::bytes_error_index(payload_ceiling)
        .expect("the maximum `bytes.Error` int32 payload must remain representable");
    assert!(matches!(
        boundary,
        Value::Int(value)
            if value.as_i128() == Some(i128::from(i32::MAX))
                && value.runtime_kind() == Some(crate::integer::IntegerKind::Int32)
    ));

    let diagnostic = super::bytes_error_index(payload_ceiling + 1)
        .expect_err("metadata above the `bytes.Error` int32 payload range must trap");
    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(
        diagnostic.message,
        "byte-codec error metadata exceeds the `bytes.Error` int32 payload range"
    );
}

#[test]
fn bytes_host_builtin_adapter_covers_codecs_hashes_and_strict_utf8() {
    fn call(name: &str, args: &[&Value]) -> Value {
        let [value] = args else {
            panic!("{name} test call should contain exactly one argument");
        };
        super::evaluate_bytes_host_builtin_ref(name, value)
            .unwrap_or_else(|| panic!("{name} should be a recognized bytes builtin"))
            .unwrap_or_else(|error| panic!("{name} should succeed: {error}"))
    }

    let binary = runtime_bytes(&[0, 1, 0xfe, 0xff]);
    let binary_snapshot = binary.clone();
    assert_eq!(
        call("bytes::hex_encode", &[&binary]),
        Value::String("0001feff".to_string())
    );
    assert_eq!(
        call("bytes::base64_encode", &[&binary]),
        Value::String("AAH+/w==".to_string())
    );
    assert_eq!(binary, binary_snapshot);

    let hex = Value::String("00AaFf".to_string());
    let base64 = Value::String("AAH+/w==".to_string());
    for (name, input, expected) in [
        ("bytes::hex_decode", &hex, vec![0, 0xaa, 0xff]),
        ("bytes::base64_decode", &base64, vec![0, 1, 0xfe, 0xff]),
    ] {
        let Value::EnumVariant(result) = call(name, &[input]) else {
            panic!("{name} should return Result");
        };
        assert_eq!(result.variant_name, "Ok");
        assert_eq!(expect_runtime_bytes(&result.payloads[0]), expected);
    }

    let abc = runtime_bytes(b"abc");
    let digest = call("bytes::sha256", &[&abc]);
    assert_eq!(expect_runtime_bytes(&digest).len(), 32);
    let text = Value::String("abc".to_string());
    assert_eq!(call("bytes::sha256_string", &[&text]), digest);
    let encoded = call("str.to_bytes", &[&text]);
    assert_eq!(expect_runtime_bytes(&encoded), b"abc");
    let Value::EnumVariant(decoded) = call("str.from_bytes", &[&encoded]) else {
        panic!("str.from_bytes should return Result");
    };
    assert_eq!(decoded.variant_name, "Ok");
    assert_eq!(decoded.payloads, vec![Value::String("abc".to_string())]);
    assert_eq!(text, Value::String("abc".to_string()));

    assert_eq!(
        super::evaluate_host_builtin("bytes::hex_encode", vec![binary])
            .expect("the owned host dispatcher should delegate to the shared byte adapter"),
        Value::String("0001feff".to_string())
    );
    let arity = super::evaluate_host_builtin("bytes::sha256", Vec::new())
        .expect_err("the owned host dispatcher should retain ordinary arity diagnostics");
    assert_eq!(
        arity.message,
        "`bytes::sha256` expects 1 arguments, found 0"
    );

    assert!(super::evaluate_bytes_host_builtin_ref("json::parse", &text).is_none());
}

#[test]
fn bytes_host_builtin_adapter_returns_typed_data_errors_and_au4005_resources() {
    fn error_variant(name: &str, input: &Value) -> String {
        let value = super::evaluate_bytes_host_builtin_ref(name, input)
            .expect("bytes builtin should be recognized")
            .expect("malformed byte data should be returned, not trapped");
        let Value::EnumVariant(result) = value else {
            panic!("{name} should return Result");
        };
        assert_eq!(result.variant_name, "Err");
        let [Value::EnumVariant(error)] = result.payloads.as_slice() else {
            panic!("{name} Result.Err should contain bytes.Error");
        };
        error.variant_name.clone()
    }

    assert_eq!(
        error_variant("str.from_bytes", &runtime_bytes(&[b'a', 0xff, b'b'])),
        "InvalidUtf8"
    );
    assert_eq!(
        error_variant("bytes::hex_decode", &Value::String("0".to_string())),
        "InvalidHexLength"
    );
    assert_eq!(
        error_variant("bytes::base64_decode", &Value::String("YQ".to_string())),
        "InvalidBase64"
    );

    for (resource, expected_message) in [
        (
            crate::bytes_codec::BytesResourceError::OutputTooLarge {
                maximum: i32::MAX as usize,
            },
            "byte-codec safety ceiling",
        ),
        (
            crate::bytes_codec::BytesResourceError::AllocationFailed,
            "memory allocation failed",
        ),
    ] {
        let diagnostic = super::bytes_resource_error_to_diagnostic(resource.clone());
        assert_eq!(diagnostic.code, "AU4005");
        assert!(diagnostic.message.contains(expected_message));

        let diagnostic = super::bytes_codec_error_to_result(
            crate::bytes_codec::BytesCodecError::Resource(resource),
        )
        .expect_err("codec resource errors must trap instead of becoming bytes.Error");
        assert_eq!(diagnostic.code, "AU4005");
        assert!(diagnostic.message.contains(expected_message));
    }
}

#[test]
fn bytes_string_from_bytes_classifies_invalid_utf8_before_runtime_materialization() {
    let malformed = runtime_bytes(&[b'a', 0xf0, 0x9f, 0x8c]);

    // InvalidUtf8 needs exactly six runtime allocations: one payload and two
    // names for bytes.Error, then one payload and two names for Result.Err.
    // Any eager copy of the Vec[uint8] would consume a seventh checkpoint and
    // incorrectly replace this typed data error with AU4005.
    let value = super::with_bytes_runtime_allocation_budget(6, || {
        super::evaluate_bytes_host_builtin_ref("str.from_bytes", &malformed)
            .expect("str.from_bytes should be recognized")
    })
    .expect("the allocation budget needed for bytes.Error must not be spent copying the input");

    let Value::EnumVariant(result) = value else {
        panic!("str.from_bytes should return Result");
    };
    assert_eq!(result.variant_name, "Err");
    let [Value::EnumVariant(error)] = result.payloads.as_slice() else {
        panic!("Result.Err should contain bytes.Error");
    };
    assert_eq!(
        (error.enum_name.as_str(), error.variant_name.as_str()),
        ("bytes.Error", "InvalidUtf8")
    );
    assert!(matches!(
        error.payloads.as_slice(),
        [Value::Int(index)]
            if index.as_i128() == Some(1)
                && index.runtime_kind() == Some(crate::integer::IntegerKind::Int32)
    ));
}

#[test]
fn bytes_string_from_bytes_reports_materialization_failure_without_consuming_input() {
    let source = runtime_bytes(b"valid UTF-8");
    let snapshot = source.clone();

    let error = super::with_bytes_runtime_allocation_budget(0, || {
        super::evaluate_bytes_host_builtin_ref("str.from_bytes", &source)
            .expect("str.from_bytes should be recognized")
    })
    .expect_err("runtime byte materialization failure must remain an AU4005 trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing byte data"
    );
    assert_eq!(source, snapshot);
}

#[test]
fn bytes_runtime_utf8_validation_matches_std_first_error_offsets_without_allocating() {
    fn expected(bytes: &[u8]) -> Option<usize> {
        std::str::from_utf8(bytes)
            .err()
            .map(|error| error.valid_up_to())
    }

    fn assert_matches_std(bytes: &[u8]) {
        assert_eq!(
            super::runtime_utf8_error_index(bytes.iter().copied()),
            expected(bytes),
            "UTF-8 validation diverged for {bytes:02x?}"
        );
    }

    assert_matches_std(&[]);
    for first in u8::MIN..=u8::MAX {
        assert_matches_std(&[first]);
        for second in u8::MIN..=u8::MAX {
            assert_matches_std(&[first, second]);
        }
    }

    for text in [
        "ASCII",
        "café",
        "Aura\0",
        "Καλημέρα",
        "こんにちは",
        "🌌🦀",
        "\u{80}\u{7ff}\u{800}\u{ffff}\u{10000}\u{10ffff}",
    ] {
        assert_matches_std(text.as_bytes());
    }

    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    for length in 0..=24 {
        for _ in 0..4_096 {
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                bytes.push((state >> 56) as u8);
            }
            assert_matches_std(&bytes);
        }
    }
}

#[test]
fn bytes_encoder_expansion_is_preflighted_against_the_codec_safety_ceiling() {
    let bytes = runtime_bytes(&[0xab]);
    let codec_safety_ceiling = i32::MAX as usize;
    for (name, logical_input_len) in [
        ("bytes::hex_encode", codec_safety_ceiling / 2 + 1),
        ("bytes::base64_encode", (codec_safety_ceiling / 4) * 3 + 1),
    ] {
        let error = super::with_bytes_runtime_encoded_input_len_for_test(logical_input_len, || {
            super::with_bytes_runtime_allocation_budget(0, || {
                super::evaluate_bytes_host_builtin_ref(name, &bytes)
                    .expect("encoder should be recognized")
            })
        })
        .expect_err("output above the codec safety ceiling must trap before input allocation");
        assert_eq!(error.code, "AU4005", "{name}");
        assert_eq!(
            error.message,
            format!(
                "byte-codec output exceeds Aura's byte-codec safety ceiling of {} bytes",
                codec_safety_ceiling
            ),
            "{name}"
        );
    }
}

#[test]
fn bounded_read_helpers_reject_zero_and_oversized_requests_without_allocation() {
    let error = validate_requested_read_size("read_bytes(...)", 0)
        .expect_err("zero-byte bounded reads should be rejected before reading");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let error = validate_requested_read_size("read_exact(...)", MAX_STREAM_READ_BYTES + 1)
        .expect_err("oversized read_exact requests should fail before allocation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let error = validate_read_line_capacity(MAX_STREAM_READ_BYTES)
        .expect_err("line reads should enforce the shared read limit");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let error = super::validate_udp_datagram_limit(super::MAX_UDP_DATAGRAM_BYTES + 1)
        .expect_err("oversized UDP reads should fail before allocation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let error = super::validate_udp_datagram_limit(0)
        .expect_err("zero-byte UDP reads should be rejected before receiving a datagram");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn fd_reads_check_deadline_and_size_before_ready_reads() {
    let expired = Instant::now() - StdDuration::from_millis(1);
    let mut line_reader = io::Cursor::new(b"ready\n".to_vec());
    let error = read_line_with_fd_deadline(&mut line_reader, 0, libc::POLLIN, Some(expired), None)
        .expect_err("expired read_line deadline should fail before consuming ready bytes");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);

    let mut exact_reader = io::empty();
    let error = read_exact_with_fd_deadline(
        &mut exact_reader,
        0,
        MAX_STREAM_READ_BYTES + 1,
        libc::POLLIN,
        None,
        None,
    )
    .expect_err("oversized read_exact should fail before allocating");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let mut some_reader = io::empty();
    let error = read_some_with_fd_deadline(
        &mut some_reader,
        0,
        MAX_STREAM_READ_BYTES + 1,
        libc::POLLIN,
        None,
        None,
    )
    .expect_err("oversized read_bytes should fail before allocating");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let mut zero_exact_reader = io::Cursor::new(b"ready".to_vec());
    let error =
        read_exact_with_fd_deadline(&mut zero_exact_reader, -1, 0, libc::POLLIN, None, None)
            .expect_err("zero-byte exact reads should be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(zero_exact_reader.position(), 0);

    let mut zero_some_reader = io::Cursor::new(b"ready".to_vec());
    let error = read_some_with_fd_deadline(&mut zero_some_reader, -1, 0, libc::POLLIN, None, None)
        .expect_err("zero-byte bounded reads should be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(zero_some_reader.position(), 0);

    let mut empty_line_reader = io::Cursor::new(Vec::<u8>::new());
    assert_eq!(
        read_line_with_fd_deadline(&mut empty_line_reader, -1, libc::POLLIN, None, None)
            .expect("empty streams should report EOF without polling"),
        None
    );

    let mut trimmed_line_reader = io::Cursor::new(b"ready\r\n".to_vec());
    assert_eq!(
        read_line_with_fd_deadline(&mut trimmed_line_reader, -1, libc::POLLIN, None, None)
            .expect("ready line should decode before polling"),
        Some("ready".to_string())
    );

    let mut short_exact_reader = io::Cursor::new(b"x".to_vec());
    let error =
        read_exact_with_fd_deadline(&mut short_exact_reader, -1, 2, libc::POLLIN, None, None)
            .expect_err("short streams should report unexpected EOF");
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

    let mut ready_some_reader = io::Cursor::new(b"abc".to_vec());
    assert_eq!(
        read_some_with_fd_deadline(&mut ready_some_reader, -1, 8, libc::POLLIN, None, None)
            .expect("ready bytes should be returned without polling"),
        Some(b"abc".to_vec())
    );

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "test read failure",
            ))
        }
    }

    let mut failing_line_reader = FailingReader;
    let error = read_line_with_fd_deadline(&mut failing_line_reader, -1, libc::POLLIN, None, None)
        .expect_err("non-retryable read_line failures should be returned");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let mut failing_exact_reader = FailingReader;
    let error =
        read_exact_with_fd_deadline(&mut failing_exact_reader, -1, 1, libc::POLLIN, None, None)
            .expect_err("non-retryable read_exact failures should be returned");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let mut failing_some_reader = FailingReader;
    let error =
        read_some_with_fd_deadline(&mut failing_some_reader, -1, 1, libc::POLLIN, None, None)
            .expect_err("non-retryable read_bytes failures should be returned");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let mut failing_all_reader = FailingReader;
    let error = read_all_with_fd_deadline(&mut failing_all_reader, -1, libc::POLLIN, None, None)
        .expect_err("non-retryable read_all failures should be returned");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut zero_writer = ZeroWriter;
    let error = write_all_with_fd_deadline(&mut zero_writer, -1, b"x", libc::POLLOUT, None, None)
        .expect_err("zero-byte writes should report WriteZero");
    assert_eq!(error.kind(), io::ErrorKind::WriteZero);

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "test write failure",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut failing_writer = FailingWriter;
    let error =
        write_all_with_fd_deadline(&mut failing_writer, -1, b"x", libc::POLLOUT, None, None)
            .expect_err("non-retryable write failures should be returned");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
static UNIX_SOCKET_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
fn unique_unix_socket_path(prefix: &str) -> PathBuf {
    let nonce = UNIX_SOCKET_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let path = PathBuf::from(format!(
        "/tmp/{prefix}-{}-{nonce}-{nanos}.sock",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    path
}

#[cfg(unix)]
fn fd_is_nonblocking(fd: i32) -> bool {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "fcntl(F_GETFL) should succeed");
    flags & libc::O_NONBLOCK != 0
}

#[test]
fn runtime_io_wait_helpers_cover_deadlines_cancellation_and_poll_edges() {
    assert!(super::deadline_from_timeout(None)
        .expect("an omitted timeout should be valid")
        .is_none());
    assert!(
        super::deadline_from_timeout(Some(StdDuration::from_millis(1)))
            .expect("a short timeout should fit the host deadline")
            .is_some()
    );
    assert_eq!(super::duration_to_poll_timeout(StdDuration::ZERO), 0);
    assert_eq!(
        super::duration_to_poll_timeout(StdDuration::from_millis(i32::MAX as u64 + 1)),
        i32::MAX
    );
    assert!(super::tls_handshake_deadline(None)
        .expect("the TLS handshake cap should fit")
        .is_some());
    let requested_tls_deadline = Instant::now() + StdDuration::from_millis(1);
    assert_eq!(
        super::tls_handshake_deadline(Some(requested_tls_deadline))
            .expect("the TLS handshake cap should fit"),
        Some(requested_tls_deadline)
    );

    let cancel_flag = Arc::new(super::RuntimeWakeSignal::new(true));
    let cancelled = CancellationContext {
        flags: vec![cancel_flag],
    };
    let error = super::check_deadline_and_cancellation(None, Some(&cancelled))
        .expect_err("cancelled context should abort before deadline checks");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let error = super::next_wait_slice(None, Some(&cancelled))
        .expect_err("cancelled context should abort wait-slice calculation");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);

    let active_cancellation = CancellationContext::default();
    let slice = super::next_wait_slice(None, Some(&active_cancellation))
        .expect("active cancellation handle should produce a polling slice")
        .expect("cancellable waits should not block forever");
    assert!(slice <= StdDuration::from_millis(50));
    assert!(super::next_wait_slice(None, None)
        .expect("non-cancellable wait should be valid")
        .is_none());
    assert!(
        super::next_wait_slice(Some(Instant::now() + StdDuration::from_millis(5)), None)
            .expect("future deadlines should produce a finite wait")
            .is_some()
    );
    let expired = Instant::now() - StdDuration::from_millis(1);
    let error = super::next_wait_slice(Some(expired), None)
        .expect_err("expired deadlines should fail immediately");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let error = non_unix_tls_listener_wait_timeout(true, Some(expired), None)
        .expect_err("empty TLS listener waits should honor expired deadlines");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    #[cfg(unix)]
    {
        let (_writer, reader) =
            std::os::unix::net::UnixStream::pair().expect("unix stream pair should be available");
        super::wait_for_tls_listener_progress(
            reader.as_raw_fd(),
            false,
            Some(Instant::now() + StdDuration::from_millis(10)),
            None,
        )
        .expect("non-empty TLS handshake queues should ignore short listener poll timeouts");
    }

    assert!(super::is_retryable_network_error(&io::Error::new(
        io::ErrorKind::WouldBlock,
        "retry",
    )));
    assert!(super::is_retryable_network_error(&io::Error::new(
        io::ErrorKind::TimedOut,
        "retry",
    )));
    assert!(super::is_retryable_network_error(&io::Error::new(
        io::ErrorKind::Interrupted,
        "retry",
    )));
    assert!(!super::is_retryable_network_error(&io::Error::new(
        io::ErrorKind::InvalidInput,
        "stop",
    )));

    let recv_ready = ChannelValue::new();
    recv_ready
        .send(Value::Unit)
        .expect("queued channel values should make receives ready");
    assert_eq!(
        wait_for_runtime_scheduler(vec![recv_ready], false, Vec::new(), Vec::new(), None, None),
        super::RuntimeSchedulerWakeReason::Ready
    );
    assert_eq!(
        wait_for_runtime_scheduler(
            Vec::new(),
            false,
            vec![ChannelValue::with_capacity(1)],
            Vec::new(),
            None,
            None,
        ),
        super::RuntimeSchedulerWakeReason::Ready
    );
    let completed_task = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    assert_eq!(
        wait_task_ready(&completed_task).expect("test task should complete"),
        Value::Unit
    );
    assert_eq!(
        wait_for_runtime_scheduler(
            Vec::new(),
            false,
            Vec::new(),
            vec![completed_task],
            None,
            None
        ),
        super::RuntimeSchedulerWakeReason::Ready
    );

    let scheduler = super::runtime_scheduler().clone();
    let first_deadline = Instant::now() + StdDuration::from_millis(10);
    let second_deadline = Instant::now() + StdDuration::from_millis(50);
    let first = scheduler.register(
        Vec::new(),
        false,
        Vec::new(),
        Vec::new(),
        Some(first_deadline),
        None,
    );
    let second = scheduler.register(
        Vec::new(),
        false,
        Vec::new(),
        Vec::new(),
        Some(second_deadline),
        None,
    );
    assert_eq!(first.wait(), super::RuntimeSchedulerWakeReason::TimedOut);
    drop(second);
    scheduler.notify();
}

#[test]
fn injected_deadline_overflow_is_invalid_input_instead_of_a_sentinel() {
    let now = Instant::now();
    let error = super::deadline_from_timeout_with(
        Some(StdDuration::from_millis(1)),
        "queue timeout",
        now,
        |_, _| None,
    )
    .expect_err("a failed host deadline addition must remain an error");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "queue timeout exceeds the host deadline range"
    );

    assert_eq!(
        super::deadline_from_timeout_with(None, "unused", now, |_, _| {
            panic!("omitted timeouts must not attempt deadline construction")
        })
        .expect("an omitted timeout remains distinct from an invalid deadline"),
        None
    );
}

#[test]
fn injected_tls_handshake_cap_overflow_fails_closed() {
    let error = super::tls_handshake_deadline_with(None, Instant::now(), |_, _| None)
        .expect_err("the TLS handshake cap must never fail open to an unlimited deadline");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "TLS handshake timeout exceeds the host deadline range"
    );
}

#[test]
fn injected_supervisor_backoff_overflow_becomes_a_failed_event() {
    let schedule = super::supervisor_restart_schedule_with(
        "worker",
        3,
        Instant::now(),
        StdDuration::from_millis(10),
        |_, _| None,
    );
    let super::SupervisorRestartSchedule::Failed(Value::EnumVariant(event)) = schedule else {
        panic!("restart deadline overflow should produce SupervisorEvent.Failed")
    };
    assert_eq!(event.enum_name, "SupervisorEvent");
    assert_eq!(event.variant_name, "Failed");
    assert_eq!(event.payloads[0], Value::String("worker".to_string()));
    assert_eq!(
        event.payloads[1].render(),
        "Error.Io(io.Error.InvalidInput)"
    );
    assert_eq!(event.payloads[2], Value::Int(IntegerValue::from_signed(3)));
}

#[test]
fn supervisor_wait_or_none_deadline_overflow_is_a_typed_error() {
    let supervisor = ProcessSupervisorValue::new();
    let error = supervisor
        .wait_or_none(Some(StdDuration::MAX), None)
        .expect_err("wait_or_none must not reclassify a deadline error as a successful event");
    let Value::EnumVariant(process_error) = error else {
        panic!("wait_or_none deadline overflow should return process.Error.Io")
    };
    assert_eq!(process_error.enum_name, "Error");
    assert_eq!(process_error.variant_name, "Io");
    assert_eq!(process_error.payloads.len(), 1);
    assert_eq!(process_error.payloads[0].render(), "io.Error.InvalidInput");

    let ProcessSupervisorWaitStatus::Event(Value::EnumVariant(event)) =
        supervisor.wait(Some(StdDuration::MAX), None)
    else {
        panic!("wait deadline overflow should return SupervisorEvent.Failed")
    };
    assert_eq!(event.enum_name, "SupervisorEvent");
    assert_eq!(event.variant_name, "Failed");
    assert_eq!(event.payloads[0], Value::String("<supervisor>".to_string()));
    assert_eq!(
        event.payloads[1].render(),
        "Error.Io(io.Error.InvalidInput)"
    );
    assert_eq!(event.payloads[2], Value::Int(IntegerValue::from_signed(0)));
}

fn reactor_wait(task_id: u64) -> (RuntimeReactor, WaitKey) {
    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let key = WaitKey(task_id, 1);
    reactor
        .begin_wait(key)
        .expect("test wait registration should succeed");
    (reactor, key)
}

fn expect_reactor_wake(reactor: &mut RuntimeReactor, key: WaitKey) {
    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(500)))
            .expect("reactor polling should succeed"),
        vec![key]
    );
}

#[test]
fn channel_send_directly_wakes_reactor_receive_subscription() {
    let channel = ChannelValue::new();
    let (mut reactor, key) = reactor_wait(101);
    channel.subscribe_reactor_recv(&ReactorSubscription::new(key, reactor.handle()), false);

    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    expect_reactor_wake(&mut reactor, key);
}

#[test]
fn queue_empty_to_nonempty_transition_wakes_all_receivers_once() {
    let channel = ChannelValue::new();
    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let first = WaitKey(108, 1);
    let second = WaitKey(109, 1);
    reactor
        .begin_wait(first)
        .expect("first receive wait should register");
    reactor
        .begin_wait(second)
        .expect("second receive wait should register");
    let handle = reactor.handle();
    channel.subscribe_reactor_recv(&ReactorSubscription::new(first, handle.clone()), false);
    channel.subscribe_reactor_recv(&ReactorSubscription::new(second, handle), false);

    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(500)))
            .expect("first bounded reactor poll should succeed"),
        vec![first, second],
        "the empty-to-nonempty transition must wake every receiver so select losers can rearm"
    );

    assert_eq!(
        channel.try_send(Value::Bool(true)),
        super::TrySendResult::Sent
    );
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("nonempty queue poll should succeed")
            .is_empty(),
        "additional sends while the queue remains nonempty must not rebroadcast stale waits"
    );
}

#[test]
fn channel_close_wakes_ordinary_but_not_ignore_closed_receive_subscription() {
    let channel = ChannelValue::new();
    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let ordinary = WaitKey(102, 1);
    let ignore_closed = WaitKey(103, 1);
    reactor
        .begin_wait(ordinary)
        .expect("ordinary receive wait should register");
    reactor
        .begin_wait(ignore_closed)
        .expect("ignore-closed receive wait should register");
    let handle = reactor.handle();
    channel.subscribe_reactor_recv(&ReactorSubscription::new(ordinary, handle.clone()), false);
    channel.subscribe_reactor_recv(&ReactorSubscription::new(ignore_closed, handle), true);

    channel.close();

    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(500)))
            .expect("reactor polling should succeed"),
        vec![ordinary]
    );
    assert!(
        reactor.is_waiting(ignore_closed),
        "ignoring queue closure must leave the receive wait pending"
    );
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("bounded no-wake poll should succeed")
            .is_empty(),
        "queue closure must not wake an ignore-closed receive subscription"
    );
}

#[test]
fn channel_close_wakes_all_ordinary_receivers_and_senders_but_not_ignore_closed_receivers() {
    let channel = ChannelValue::with_capacity(1);
    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);

    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let ordinary_first = WaitKey(110, 1);
    let ordinary_second = WaitKey(111, 1);
    let ignore_closed_first = WaitKey(112, 1);
    let ignore_closed_second = WaitKey(113, 1);
    let sender_first = WaitKey(114, 1);
    let sender_second = WaitKey(115, 1);
    for key in [
        ordinary_first,
        ordinary_second,
        ignore_closed_first,
        ignore_closed_second,
        sender_first,
        sender_second,
    ] {
        reactor
            .begin_wait(key)
            .expect("queue close wait should register");
    }

    let handle = reactor.handle();
    channel.subscribe_reactor_recv(
        &ReactorSubscription::new(ordinary_first, handle.clone()),
        false,
    );
    channel.subscribe_reactor_recv(
        &ReactorSubscription::new(ordinary_second, handle.clone()),
        false,
    );
    channel.subscribe_reactor_recv(
        &ReactorSubscription::new(ignore_closed_first, handle.clone()),
        true,
    );
    channel.subscribe_reactor_recv(
        &ReactorSubscription::new(ignore_closed_second, handle.clone()),
        true,
    );
    channel.subscribe_reactor_send(&ReactorSubscription::new(sender_first, handle.clone()));
    channel.subscribe_reactor_send(&ReactorSubscription::new(sender_second, handle));

    channel.close();

    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(500)))
            .expect("bounded queue-close poll should succeed"),
        vec![ordinary_first, ordinary_second, sender_first, sender_second],
        "queue close must broadcast to ordinary receivers and blocked senders"
    );
    assert!(reactor.is_waiting(ignore_closed_first));
    assert!(reactor.is_waiting(ignore_closed_second));
    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("bounded ignore-closed poll should succeed"),
        Vec::<WaitKey>::new(),
        "queue close must leave every ignore-closed receiver asleep"
    );
}

#[test]
fn bounded_channel_receive_directly_wakes_reactor_send_subscription() {
    let channel = ChannelValue::with_capacity(1);
    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    let (mut reactor, key) = reactor_wait(104);
    channel.subscribe_reactor_send(&ReactorSubscription::new(key, reactor.handle()));

    assert_eq!(channel.try_recv(), TryRecvResult::Value(Value::Unit));
    expect_reactor_wake(&mut reactor, key);
}

#[test]
fn bounded_queue_full_to_available_transition_wakes_all_senders_once() {
    let channel = ChannelValue::with_capacity(1);
    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);

    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let first = WaitKey(116, 1);
    let second = WaitKey(117, 1);
    reactor
        .begin_wait(first)
        .expect("first blocked sender wait should register");
    reactor
        .begin_wait(second)
        .expect("second blocked sender wait should register");
    let handle = reactor.handle();
    channel.subscribe_reactor_send(&ReactorSubscription::new(first, handle.clone()));
    channel.subscribe_reactor_send(&ReactorSubscription::new(second, handle));

    assert_eq!(channel.try_recv(), TryRecvResult::Value(Value::Unit));
    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(500)))
            .expect("bounded sender-wake poll should succeed"),
        vec![first, second],
        "the full-to-available transition must wake every sender so select losers can rearm"
    );
    assert_eq!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("bounded post-receive poll should succeed"),
        Vec::<WaitKey>::new(),
        "one capacity transition must not queue duplicate sender wakeups"
    );
}

#[test]
fn real_task_completion_directly_wakes_reactor_result_subscription() {
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = TaskValue::from_handle(thread::spawn(move || {
        release_rx
            .recv()
            .expect("test should release the task after subscribing");
        Ok(Value::Unit)
    }));
    let (mut reactor, key) = reactor_wait(105);
    task.subscribe_reactor_completion(&ReactorSubscription::new(key, reactor.handle()));

    release_tx
        .send(())
        .expect("test task release should be delivered");
    expect_reactor_wake(&mut reactor, key);
    assert_eq!(
        wait_task_ready(&task).expect("released task should complete"),
        Value::Unit
    );
}

#[test]
fn cancellation_directly_wakes_unbounded_reactor_subscription() {
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let (mut reactor, key) = reactor_wait(106);
    cancellation.subscribe_reactor(&ReactorSubscription::new(key, reactor.handle()));

    group.cancel();

    expect_reactor_wake(&mut reactor, key);
    assert!(cancellation.is_cancelled());
}

#[test]
fn phase51_runtime_wake_signal_notifies_once_per_false_to_true_transition() {
    let signal = super::RuntimeWakeSignal::new(false);
    let mut reactor = RuntimeReactor::new().expect("test reactor should initialize");
    let first = WaitKey(118, 1);
    reactor
        .begin_wait(first)
        .expect("first signal wait should register");
    signal.subscribe(&ReactorSubscription::new(first, reactor.handle()));

    signal.store(false, Ordering::SeqCst);
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("unchanged false signal poll should succeed")
            .is_empty(),
        "storing false must not wake a signal subscriber"
    );

    signal.store(true, Ordering::SeqCst);
    expect_reactor_wake(&mut reactor, first);

    let second = WaitKey(119, 1);
    reactor
        .begin_wait(second)
        .expect("second signal wait should register");
    signal.subscribe(&ReactorSubscription::new(second, reactor.handle()));
    signal.store(true, Ordering::SeqCst);
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("unchanged true signal poll should succeed")
            .is_empty(),
        "repeated true stores must not rebroadcast an already-raised signal"
    );

    signal.store(false, Ordering::SeqCst);
    signal.store(true, Ordering::SeqCst);
    expect_reactor_wake(&mut reactor, second);

    let unsubscribed = WaitKey(120, 1);
    reactor
        .begin_wait(unsubscribed)
        .expect("unsubscription probe wait should register");
    let subscription = ReactorSubscription::new(unsubscribed, reactor.handle());
    signal.subscribe(&subscription);
    signal.unsubscribe(&subscription);
    signal.store(false, Ordering::SeqCst);
    signal.store(true, Ordering::SeqCst);
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("unsubscribed signal poll should succeed")
            .is_empty(),
        "an unsubscribed wait must remain asleep across later signal transitions"
    );
}

#[test]
fn duplicate_reactor_subscriptions_and_wakes_produce_one_ready_key() {
    let channel = ChannelValue::new();
    let (mut reactor, key) = reactor_wait(107);
    let handle = reactor.handle();
    channel.subscribe_reactor_recv(&ReactorSubscription::new(key, handle.clone()), false);
    channel.subscribe_reactor_recv(&ReactorSubscription::new(key, handle), false);

    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    assert_eq!(
        channel.try_send(Value::Bool(true)),
        super::TrySendResult::Sent
    );

    expect_reactor_wake(&mut reactor, key);
    assert!(
        reactor
            .poll(Some(StdDuration::from_millis(25)))
            .expect("bounded post-wake poll should succeed")
            .is_empty(),
        "duplicate registrations and source notifications must not queue duplicate wakes"
    );
}

#[test]
fn phase51_scheduler_rearms_after_a_queue_wake_loses_the_readiness_race() {
    let queue = ChannelValue::new();
    let mut scheduler = super::LightweightTaskScheduler::new();
    scheduler.arm_wait(
        121,
        super::TaskWaitRegistration {
            recv_channels: vec![queue.clone()],
            ignore_closed_recv_channels: false,
            send_channels: Vec::new(),
            task_waits: Vec::new(),
            deadline: None,
            cancellation: None,
            fd_wait: None,
        },
    );
    let first_key = scheduler
        .waiting
        .get(&121)
        .expect("queue wait should be armed")
        .key;

    assert_eq!(queue.try_send(Value::Unit), super::TrySendResult::Sent);
    assert_eq!(queue.try_recv(), TryRecvResult::Value(Value::Unit));
    scheduler
        .wait_for_external_events()
        .expect("the stale queue notification should be admitted safely");

    let rearmed_key = scheduler
        .waiting
        .get(&121)
        .expect("a consumed queue notification must leave the task waiting")
        .key;
    assert_ne!(
        rearmed_key, first_key,
        "rearming must allocate a fresh epoch so late events cannot resume the new wait"
    );
    assert!(
        scheduler.ready.is_empty(),
        "a queue item consumed before admission must not spuriously resume the waiter"
    );

    scheduler.admit_reactor_keys(vec![WaitKey(999, 1), first_key]);
    assert_eq!(
        scheduler
            .waiting
            .get(&121)
            .expect("unknown and stale keys must leave the active wait intact")
            .key,
        rearmed_key
    );
    assert!(
        scheduler.ready.is_empty(),
        "unknown task ids and prior wait epochs must not resume a task"
    );

    assert_eq!(
        queue.try_send(Value::Bool(true)),
        super::TrySendResult::Sent
    );
    scheduler
        .wait_for_external_events()
        .expect("a fresh queue transition should wake the rearmed wait");
    assert_eq!(
        scheduler.ready.pop_front(),
        Some((121, super::RuntimeSchedulerWakeReason::Ready))
    );
}

#[cfg(unix)]
#[test]
fn phase51_unsupported_fd_interest_surfaces_the_reactor_registration_diagnostic() {
    let error = run_lightweight_root_task(|| {
        let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
            recv_channels: Vec::new(),
            ignore_closed_recv_channels: false,
            send_channels: Vec::new(),
            task_waits: Vec::new(),
            deadline: None,
            cancellation: None,
            fd_wait: Some(super::FdWaitRegistration { fd: -1, events: 0 }),
        });
        Ok(Value::Unit)
    })
    .expect_err("a descriptor wait without readable or writable interest must fail");

    assert!(
        error
            .message
            .contains("Aura runtime reactor failed while registering a task wait"),
        "the scheduler should identify the failed reactor operation: {error:?}"
    );
    assert!(
        error
            .message
            .contains("descriptor wait has no supported interest"),
        "the diagnostic should explain the invalid descriptor interest: {error:?}"
    );
}

#[test]
fn task_wait_ready_reason_preserves_cancellation_source_deadline_fd_precedence() {
    let ready_channel = ChannelValue::new();
    assert_eq!(
        ready_channel.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let completed_task = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    assert_eq!(
        wait_task_ready(&completed_task).expect("test task should complete"),
        Value::Unit
    );
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancelled = group.child_cancellation();
    group.cancel();
    let expired = Instant::now() - StdDuration::from_millis(1);

    let all_ready = super::TaskWaitRegistration {
        recv_channels: vec![ready_channel.clone()],
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: vec![completed_task.clone()],
        deadline: Some(expired),
        cancellation: Some(cancelled),
        fd_wait: None,
    };
    assert_eq!(
        all_ready.ready_reason(true),
        Some(super::RuntimeSchedulerWakeReason::Cancelled),
        "cancellation must win over every other ready source"
    );

    let queue_ready = super::TaskWaitRegistration {
        recv_channels: vec![ready_channel],
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: Some(expired),
        cancellation: None,
        fd_wait: None,
    };
    assert_eq!(
        queue_ready.ready_reason(true),
        Some(super::RuntimeSchedulerWakeReason::Ready),
        "queue readiness must win over an expired deadline and fd readiness"
    );

    let task_ready = super::TaskWaitRegistration {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: vec![completed_task],
        deadline: Some(expired),
        cancellation: None,
        fd_wait: None,
    };
    assert_eq!(
        task_ready.ready_reason(true),
        Some(super::RuntimeSchedulerWakeReason::Ready),
        "task completion must win over an expired deadline and fd readiness"
    );

    let deadline_ready = super::TaskWaitRegistration {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: Some(expired),
        cancellation: None,
        fd_wait: None,
    };
    assert_eq!(
        deadline_ready.ready_reason(true),
        Some(super::RuntimeSchedulerWakeReason::TimedOut),
        "an expired deadline must win over fd readiness"
    );

    let fd_ready = super::TaskWaitRegistration {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: None,
        cancellation: None,
        fd_wait: None,
    };
    assert_eq!(
        fd_ready.ready_reason(true),
        Some(super::RuntimeSchedulerWakeReason::Ready)
    );
    assert_eq!(fd_ready.ready_reason(false), None);
}

#[test]
fn resolving_a_scheduler_wait_removes_all_source_subscriptions() {
    let queue = ChannelValue::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = TaskValue::from_handle(thread::spawn(move || {
        release_rx
            .recv()
            .expect("test should release the task after cleanup");
        Ok(Value::Unit)
    }));
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let mut scheduler = super::LightweightTaskScheduler::new();
    scheduler.arm_wait(
        108,
        super::TaskWaitRegistration {
            recv_channels: vec![queue.clone()],
            ignore_closed_recv_channels: false,
            send_channels: Vec::new(),
            task_waits: vec![task.clone()],
            deadline: None,
            cancellation: Some(cancellation.clone()),
            fd_wait: None,
        },
    );

    assert_eq!(lock_mutex(&queue.inner.recv_reactor_subscribers).len(), 1);
    assert_eq!(
        lock_mutex(&task.inner.completion_reactor_subscribers).len(),
        1
    );
    assert!(cancellation
        .flags
        .iter()
        .all(|flag| lock_mutex(&flag.reactor_subscribers).len() == 1));

    assert_eq!(queue.try_send(Value::Unit), super::TrySendResult::Sent);
    scheduler
        .wait_for_external_events()
        .expect("queue readiness should resolve the scheduler wait");

    assert!(!scheduler.waiting.contains_key(&108));
    assert_eq!(
        scheduler.ready.pop_front(),
        Some((108, super::RuntimeSchedulerWakeReason::Ready))
    );
    assert!(lock_mutex(&queue.inner.recv_reactor_subscribers).is_empty());
    assert!(lock_mutex(&task.inner.completion_reactor_subscribers).is_empty());
    assert!(cancellation
        .flags
        .iter()
        .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()));

    release_tx
        .send(())
        .expect("test task release should be delivered");
    assert_eq!(
        wait_task_ready(&task).expect("released task should complete"),
        Value::Unit
    );
}

#[test]
fn dropping_scheduler_removes_outstanding_source_subscriptions() {
    let queue = ChannelValue::new();
    let cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = cancellation_group.child_cancellation();
    {
        let mut scheduler = super::LightweightTaskScheduler::new();
        scheduler.arm_wait(
            109,
            super::TaskWaitRegistration {
                recv_channels: vec![queue.clone()],
                ignore_closed_recv_channels: false,
                send_channels: Vec::new(),
                task_waits: Vec::new(),
                deadline: Some(Instant::now() + StdDuration::from_secs(60)),
                cancellation: Some(cancellation.clone()),
                fd_wait: None,
            },
        );
        assert_eq!(lock_mutex(&queue.inner.recv_reactor_subscribers).len(), 1);
        assert!(cancellation
            .flags
            .iter()
            .all(|flag| lock_mutex(&flag.reactor_subscribers).len() == 1));
    }

    assert!(lock_mutex(&queue.inner.recv_reactor_subscribers).is_empty());
    assert!(cancellation
        .flags
        .iter()
        .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()));
}

#[test]
fn continuously_yielding_task_does_not_starve_reactor_wakeups() {
    let wake_queue = ChannelValue::new();
    let sender = wake_queue.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let sender_stop = stop.clone();
    let sender_thread = thread::spawn(move || {
        thread::sleep(StdDuration::from_millis(10));
        assert_eq!(sender.try_send(Value::Unit), super::TrySendResult::Sent);
        thread::sleep(crate::hosted_ci_timing_limit(StdDuration::from_millis(240)));
        sender_stop.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let received_after = super::run_lightweight_root_task_with_worker_count(1, move || {
        let hot_stop = stop.clone();
        let hot = super::spawn_lightweight_task(move || {
            while !hot_stop.load(Ordering::SeqCst) {
                super::yield_now_current_lightweight_task();
            }
            Ok(Value::Unit)
        })?;

        let received_after = match wake_queue
            .recv_result_with_cancellation(None, None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            RecvValueResult::Value(_) => started.elapsed(),
            other => panic!("reactor wake should deliver the queued value, got {other:?}"),
        };
        match hot
            .wait_result_with_cancellation(None, None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            super::TaskWaitStatus::Ready(result) => assert_eq!(result?, Value::Unit),
            other => panic!("yielding task should complete normally, got {other:?}"),
        }
        Ok(Value::Int(IntegerValue::from_signed(
            received_after.as_millis() as i128,
        )))
    })
    .expect("reactor fairness probe should complete");
    sender_thread.join().expect("sender thread should complete");

    let Value::Int(received_after) = received_after else {
        panic!("fairness probe should return its observed latency")
    };
    assert!(
        received_after.as_i128().expect("latency should fit i128")
            < crate::hosted_ci_timing_limit(StdDuration::from_millis(100)).as_millis() as i128,
        "a continually yielding task delayed the reactor wake for {received_after:?} ms"
    );
}

#[test]
fn lightweight_scheduler_helpers_cover_unbounded_waits_and_defensive_exit() {
    let mut scheduler = super::LightweightTaskScheduler::new();

    let waiting_task = scheduler
        .spawn_task(None, || {
            let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                recv_channels: Vec::new(),
                ignore_closed_recv_channels: false,
                send_channels: Vec::new(),
                task_waits: Vec::new(),
                deadline: None,
                cancellation: None,
                fd_wait: None,
            });
            Ok(Value::Unit)
        })
        .expect("lightweight task should spawn");
    scheduler.resume_task(1, super::RuntimeSchedulerWakeReason::Ready);
    assert!(waiting_task.waits_without_deadline());
    scheduler.resume_task(1, super::RuntimeSchedulerWakeReason::Ready);
    assert!(!waiting_task.waits_without_deadline());
    match waiting_task.completed_result() {
        Some(TaskExecutionResult::Ready(Ok(Value::Unit))) => {}
        other => panic!("expected completed unit task, got {other:?}"),
    }

    let timed_wait_task = scheduler
        .spawn_task(None, || {
            let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                recv_channels: Vec::new(),
                ignore_closed_recv_channels: false,
                send_channels: Vec::new(),
                task_waits: Vec::new(),
                deadline: Some(Instant::now() + StdDuration::from_secs(1)),
                cancellation: None,
                fd_wait: None,
            });
            Ok(Value::Unit)
        })
        .expect("lightweight task should spawn");
    scheduler.resume_task(2, super::RuntimeSchedulerWakeReason::Ready);
    assert!(!timed_wait_task.waits_without_deadline());
    scheduler.resume_task(2, super::RuntimeSchedulerWakeReason::Ready);
    match timed_wait_task.completed_result() {
        Some(TaskExecutionResult::Ready(Ok(Value::Unit))) => {}
        other => panic!("expected completed timed-wait task, got {other:?}"),
    }

    let exit_without_result = scheduler
        .spawn_task(None, || {
            let _ = super::yield_current_lightweight_task(super::TaskYield::Exit);
            Ok(Value::Unit)
        })
        .expect("lightweight task should spawn");
    scheduler.resume_task(3, super::RuntimeSchedulerWakeReason::Ready);
    match exit_without_result.completed_result() {
        Some(TaskExecutionResult::Ready(Err(error))) => assert!(error
            .message
            .contains("lightweight task exited without a result")),
        other => panic!("expected defensive missing-result error, got {other:?}"),
    }

    let ready_queue = ChannelValue::new();
    assert_eq!(
        ready_queue.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let immediately_ready = scheduler
        .spawn_task(None, move || {
            let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                recv_channels: vec![ready_queue],
                ignore_closed_recv_channels: false,
                send_channels: Vec::new(),
                task_waits: Vec::new(),
                deadline: None,
                cancellation: None,
                fd_wait: None,
            });
            Ok(Value::Unit)
        })
        .expect("immediately-ready task should spawn");
    scheduler.resume_task(4, super::RuntimeSchedulerWakeReason::Ready);
    assert!(
        !immediately_ready.waits_without_deadline(),
        "a source that wins the final readiness check must not report an armed unbounded wait"
    );

    #[cfg(unix)]
    {
        let failed_registration = scheduler
            .spawn_task(None, || {
                let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                    recv_channels: Vec::new(),
                    ignore_closed_recv_channels: false,
                    send_channels: Vec::new(),
                    task_waits: Vec::new(),
                    deadline: None,
                    cancellation: None,
                    fd_wait: Some(super::FdWaitRegistration { fd: -1, events: 0 }),
                });
                Ok(Value::Unit)
            })
            .expect("registration-failure task should spawn");
        scheduler.resume_task(5, super::RuntimeSchedulerWakeReason::Ready);
        assert!(
            !failed_registration.waits_without_deadline(),
            "a failed reactor registration must not leave stale unbounded-wait state"
        );
    }
}

#[test]
fn lightweight_scheduler_teardown_cancels_abandoned_tasks_and_runs_cleanup_once() {
    let cleanup_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cleanup_probe = cleanup_count.clone();
    let task_slot = Arc::new(Mutex::new(None));
    let task_probe = task_slot.clone();
    let cleanup_child_slot = Arc::new(Mutex::new(None));
    let cleanup_child_probe = cleanup_child_slot.clone();

    assert_eq!(
        run_lightweight_root_task(move || {
            let task = unsafe {
                spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup(
                    CancellationContext::default(),
                    || {
                        let _ =
                            super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                                recv_channels: Vec::new(),
                                ignore_closed_recv_channels: false,
                                send_channels: Vec::new(),
                                task_waits: Vec::new(),
                                deadline: None,
                                cancellation: None,
                                fd_wait: None,
                            });
                        Ok(Value::Unit)
                    },
                    move || {
                        cleanup_probe.fetch_add(1, Ordering::SeqCst);
                        let child = spawn_lightweight_task(|| Ok(Value::Unit))
                            .expect("teardown cleanup should retain its task context");
                        *lock_mutex(&cleanup_child_probe) = Some(child);
                    },
                )?
            };
            *lock_mutex(&task_probe) = Some(task);
            let _ = super::yield_now_current_lightweight_task();
            Ok(Value::Unit)
        })
        .expect("root completion should tear down abandoned children safely"),
        Value::Unit
    );

    assert_eq!(
        cleanup_count.load(Ordering::SeqCst),
        1,
        "a direct task's externalized cleanup must run exactly once"
    );
    let task = lock_mutex(&task_slot)
        .take()
        .expect("the root should publish its child handle");
    assert!(
        matches!(
            task.completed_result(),
            Some(TaskExecutionResult::Cancelled)
        ),
        "scheduler teardown must not leave abandoned task handles running"
    );
    let cleanup_child = lock_mutex(&cleanup_child_slot)
        .take()
        .expect("teardown should drain a child prepared by direct cleanup");
    assert!(
        matches!(
            cleanup_child.completed_result(),
            Some(TaskExecutionResult::Cancelled)
        ),
        "a cleanup-spawned child must also reach a terminal state during teardown"
    );
}

#[test]
fn force_reset_releases_the_task_context_and_spawn_buffer() {
    let spawn_buffer = {
        let mut scheduler = super::LightweightTaskScheduler::new();
        let spawn_buffer = std::rc::Rc::downgrade(&scheduler.spawn_requests);
        scheduler
            .spawn_task_with_forced_exit_cleanup(
                Some(CancellationContext::default()),
                None,
                || {
                    let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                        recv_channels: Vec::new(),
                        ignore_closed_recv_channels: false,
                        send_channels: Vec::new(),
                        task_waits: Vec::new(),
                        deadline: None,
                        cancellation: None,
                        fd_wait: None,
                    });
                    Ok(Value::Unit)
                },
                Some(Box::new(|| {})),
            )
            .expect("direct cleanup task should spawn");
        scheduler.resume_task(1, super::RuntimeSchedulerWakeReason::Ready);
        assert!(
            spawn_buffer.upgrade().is_some(),
            "the scheduler should own its live spawn buffer"
        );
        spawn_buffer
    };

    assert!(
        spawn_buffer.upgrade().is_none(),
        "force-reset must not abandon a task-context Rc on the coroutine stack"
    );
}

#[test]
fn single_worker_nested_spawns_preserve_admission_fifo_and_immediate_child_waits() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let root_order = order.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let first_order = root_order.clone();
        let first = spawn_lightweight_task(move || {
            lock_mutex(&first_order).push("first");
            let nested_order = first_order.clone();
            let nested = spawn_lightweight_task(move || {
                lock_mutex(&nested_order).push("nested");
                Ok(Value::Int(IntegerValue::from_signed(7)))
            })?;
            let nested_result = wait_task_ready(&nested)?;
            lock_mutex(&first_order).push("first-after-wait");
            Ok(nested_result)
        })?;

        let second_order = root_order.clone();
        let second = spawn_lightweight_task(move || {
            lock_mutex(&second_order).push("second");
            Ok(Value::Unit)
        })?;

        assert_eq!(
            wait_task_ready(&first)?,
            Value::Int(IntegerValue::from_signed(7))
        );
        wait_task_ready(&second)?;
        Ok(Value::Unit)
    });

    assert_eq!(
        result.expect("nested spawn probe should complete"),
        Value::Unit
    );
    assert_eq!(
        &*lock_mutex(&order),
        &["first", "second", "nested", "first-after-wait"],
        "spawn requests must join the ready queue in FIFO order before their parent resumes"
    );
}

#[test]
fn nested_stack_allocation_failure_is_synchronous_and_does_not_enqueue_a_task() {
    let result = run_lightweight_root_task(|| {
        super::fail_next_lightweight_task_stack_allocation();
        let error = spawn_lightweight_task(|| Ok(Value::Unit))
            .expect_err("the injected stack allocation failure must be returned by spawn");
        assert_eq!(error.code, "AU4005");

        let healthy = spawn_lightweight_task(|| Ok(Value::Bool(true)))?;
        wait_task_ready(&healthy)
    });
    assert_eq!(
        result.expect("the scheduler should remain usable after rejected admission"),
        Value::Bool(true)
    );
}

#[test]
fn root_scheduler_stack_allocation_failure_is_synchronous_and_preserves_admission_order() {
    let mut scheduler = super::LightweightTaskScheduler::new();
    super::fail_next_lightweight_task_stack_allocation();
    let error = scheduler
        .spawn_task(None, || Ok(Value::Unit))
        .expect_err("a root scheduler must report stack-allocation failure before admission");
    assert_eq!(error.code, "AU4005");
    assert_eq!(error.message, "injected Aura task stack allocation failure");

    let healthy = scheduler
        .spawn_task(None, || Ok(Value::Bool(true)))
        .expect("the rejected task must not consume the first scheduler admission");
    scheduler.resume_task(1, super::RuntimeSchedulerWakeReason::Ready);
    match healthy.completed_result() {
        Some(TaskExecutionResult::Ready(Ok(Value::Bool(true)))) => {}
        other => panic!("expected the first admitted task to complete, found {other:?}"),
    }
}

#[test]
fn pure_rust_abandoned_task_unwinds_owned_values_once_at_teardown() {
    struct DropProbe(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_drops = drops.clone();
    super::run_lightweight_root_task_with_worker_count(1, move || {
        spawn_lightweight_task(move || {
            let _probe = DropProbe(task_drops);
            let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                recv_channels: Vec::new(),
                ignore_closed_recv_channels: false,
                send_channels: Vec::new(),
                task_waits: Vec::new(),
                deadline: None,
                cancellation: None,
                fd_wait: None,
            });
            Ok(Value::Unit)
        })?;
        let _ = super::yield_now_current_lightweight_task();
        Ok(Value::Unit)
    })
    .expect("root completion should unwind an abandoned pure-Rust child");
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "an abandoned pure-Rust task's owned values must be dropped exactly once"
    );
}

#[test]
fn direct_cleanup_can_spawn_a_child_before_the_parent_is_retired() {
    let spawned = Arc::new(Mutex::new(None));
    let spawned_probe = spawned.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        unsafe {
            spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup(
                CancellationContext::default(),
                || {
                    super::exit_current_lightweight_task(TaskExecutionResult::Cancelled);
                },
                move || {
                    let child = spawn_lightweight_task(|| Ok(Value::Bool(true)))
                        .expect("cleanup should retain the active spawn context");
                    *lock_mutex(&spawned_probe) = Some(child);
                },
            )?;
        }
        let deadline = Instant::now() + StdDuration::from_secs(1);
        let task = loop {
            if let Some(task) = lock_mutex(&spawned).take() {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "direct cleanup should publish its child before the test deadline"
            );
            let _ = super::yield_now_current_lightweight_task();
        };
        wait_task_ready(&task)
    });
    assert_eq!(
        result.expect("a cleanup-spawned child should be drained safely"),
        Value::Bool(true)
    );
}

#[test]
fn explicit_stack_tasks_preserve_single_consumer_results_through_forced_cleanup() {
    let forced_cleanups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cleanup_probe = forced_cleanups.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let completed = super::spawn_lightweight_task_with_stack_and_result_repeatability(
            512 * 1024,
            false,
            || Ok(Value::String("owned result".to_string())),
        )?;
        completed
            .claim_result_observation()
            .expect("a stacked task should expose one owned-result observation");
        assert_eq!(
            wait_task_ready(&completed)?,
            Value::String("owned result".to_string())
        );
        let error = completed
            .claim_result_observation()
            .expect_err("the explicit-stack path must preserve single-consumer result metadata");
        assert_eq!(error.code, "AU4001");

        let cancelled = unsafe {
            super::spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup_and_stack_and_result_repeatability(
                CancellationContext::default(),
                Some(512 * 1024),
                false,
                || {
                    super::exit_current_lightweight_task(TaskExecutionResult::Cancelled);
                },
                move || {
                    cleanup_probe.fetch_add(1, Ordering::SeqCst);
                },
            )?
        };
        let deadline = Instant::now() + StdDuration::from_secs(1);
        while cancelled.completed_result().is_none() {
            assert!(
                Instant::now() < deadline,
                "the force-cleaned task should complete before the test deadline"
            );
            let _ = super::yield_now_current_lightweight_task();
        }
        assert!(matches!(
            cancelled.completed_result(),
            Some(TaskExecutionResult::Cancelled)
        ));
        cancelled
            .claim_result_observation()
            .expect("a force-cleaned task should expose its one cancelled observation");
        let error = cancelled
            .claim_result_observation()
            .expect_err("forced cleanup must retain the single-consumer observation state");
        assert_eq!(error.code, "AU4001");
        Ok(Value::Unit)
    });

    assert_eq!(
        result.expect("explicit-stack result ownership should survive both completion paths"),
        Value::Unit
    );
    assert_eq!(
        forced_cleanups.load(Ordering::SeqCst),
        1,
        "forced exit must run the externalized cleanup exactly once"
    );
}

#[test]
fn generated_root_cleanup_runs_once_on_forced_exit_and_not_on_normal_return() {
    let normal_cleanups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let normal_probe = normal_cleanups.clone();
    let normal = unsafe {
        super::run_lightweight_root_task_with_forced_exit_cleanup(
            || Ok(Value::Bool(true)),
            move || {
                normal_probe.fetch_add(1, Ordering::SeqCst);
            },
        )
    };
    assert_eq!(
        normal.expect("a generated root should return normally"),
        Value::Bool(true)
    );
    assert_eq!(
        normal_cleanups.load(Ordering::SeqCst),
        0,
        "forced cleanup must not run after a normal generated-root return"
    );

    let forced_cleanups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let forced_probe = forced_cleanups.clone();
    let forced = unsafe {
        super::run_lightweight_root_task_with_forced_exit_cleanup(
            || {
                super::exit_current_lightweight_task(TaskExecutionResult::Ready(Err(
                    Diagnostic::new("generated root failed"),
                )));
            },
            move || {
                forced_probe.fetch_add(1, Ordering::SeqCst);
            },
        )
    }
    .expect_err("the generated root should preserve its forced failure");
    assert_eq!(forced.message, "generated root failed");
    assert_eq!(
        forced_cleanups.load(Ordering::SeqCst),
        1,
        "generated-root forced cleanup must run exactly once"
    );

    #[cfg(unix)]
    {
        let abandoned_cleanups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let abandoned_probe = abandoned_cleanups.clone();
        let abandoned = unsafe {
            super::run_lightweight_root_task_with_forced_exit_cleanup(
                || {
                    let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                        recv_channels: Vec::new(),
                        ignore_closed_recv_channels: false,
                        send_channels: Vec::new(),
                        task_waits: Vec::new(),
                        deadline: None,
                        cancellation: None,
                        fd_wait: Some(super::FdWaitRegistration { fd: -1, events: 0 }),
                    });
                    Ok(Value::Unit)
                },
                move || {
                    abandoned_probe.fetch_add(1, Ordering::SeqCst);
                },
            )
        }
        .expect_err("a reactor registration failure should abandon the generated root");
        assert!(abandoned
            .message
            .contains("descriptor wait has no supported interest"));
        assert_eq!(
            abandoned_cleanups.load(Ordering::SeqCst),
            1,
            "generated-root cleanup must run exactly once on scheduler abandonment"
        );
    }
}

#[cfg(unix)]
#[test]
fn unix_fd_nonblocking_helper_toggles_socket_flags_and_reports_bad_fds() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    super::set_fd_nonblocking(listener.as_raw_fd(), true)
        .expect("socket should become nonblocking");
    assert!(fd_is_nonblocking(listener.as_raw_fd()));
    super::set_fd_nonblocking(listener.as_raw_fd(), false)
        .expect("socket should return to blocking mode");
    assert!(!fd_is_nonblocking(listener.as_raw_fd()));

    let (mut writer, reader) =
        std::os::unix::net::UnixStream::pair().expect("unix stream pair should be available");
    writer
        .write_all(b"x")
        .expect("ready peer byte should be written");
    super::wait_for_fd_event(reader.as_raw_fd(), libc::POLLIN, None, None)
        .expect("ready fd should not need a deadline to wake");

    let cancelled_poll = run_lightweight_root_task(|| {
        let group = TaskGroupValue::new(&CancellationContext::default());
        let cancellation = group.child_cancellation();
        group.cancel();
        let (_writer, reader) =
            std::os::unix::net::UnixStream::pair().expect("unix stream pair should be available");
        let error =
            super::wait_for_fd_event(reader.as_raw_fd(), libc::POLLIN, None, Some(&cancellation))
                .expect_err("cancelled lightweight fd waits should return promptly");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        Ok(Value::Bool(true))
    });
    assert_eq!(
        cancelled_poll.expect("cancelled fd poll root should complete"),
        Value::Bool(true)
    );

    super::set_fd_nonblocking(-1, true).expect_err("bad fds should report fcntl errors");
}

#[test]
fn render_float_formats_current_surface() {
    assert_eq!(render_float(42.0), "42.0");
    assert_eq!(render_float(3.5), "3.5");
    assert_eq!(render_float(f64::INFINITY), "inf");
    assert_eq!(render_float(9_007_199_254_740_992.0), "9007199254740992.0");
    assert_eq!(render_float(1e300), "1e300");
    assert_eq!(render_float(1e-300), "1e-300");
    assert_eq!(render_float(-0.0), "-0.0");
    assert_eq!(render_float(0.1 + 0.2), "0.30000000000000004");

    assert_eq!(render_float32(3.14), "3.14");
    assert_eq!(render_float32(-0.0), "-0.0");
}

#[test]
fn duration_helpers_preserve_nanoseconds_rendering_conversions_and_host_limits() {
    assert_eq!(super::NANOS_PER_MILLISECOND, 1_000_000);
    assert_eq!(super::NANOS_PER_SECOND, 1_000_000_000);
    assert_eq!(super::NANOS_PER_MINUTE, 60_000_000_000);

    for (nanoseconds, rendered) in [
        (0, "0ms"),
        (1, "0.000001ms"),
        (-1, "-0.000001ms"),
        (1_000_000, "1ms"),
        (1_100_000, "1.1ms"),
        (1_010_000, "1.01ms"),
        (1_001_000, "1.001ms"),
        (1_000_100, "1.0001ms"),
        (1_000_010, "1.00001ms"),
        (1_000_001, "1.000001ms"),
        (1_500_000, "1.5ms"),
        (-1_500_001, "-1.500001ms"),
        (1_000_000_000, "1000ms"),
        (i128::MIN, "-170141183460469231731687303715884.105728ms"),
    ] {
        assert_eq!(super::render_duration(nanoseconds), rendered);
        assert_eq!(Value::Duration(nanoseconds).render(), rendered);
    }

    assert_eq!(super::duration_to_milliseconds(1_500_000), 1.5);
    assert_eq!(super::duration_to_seconds(1_500_000_000), 1.5);
    assert_eq!(
        super::duration_to_milliseconds(0).to_bits(),
        0.0_f64.to_bits()
    );
    assert_eq!(super::duration_to_seconds(0).to_bits(), 0.0_f64.to_bits());
    for (nanoseconds, milliseconds_bits, seconds_bits) in [
        (1, 0x3eb0_c6f7_a0b5_ed8d, 0x3e11_2e0b_e826_d695),
        (-1, 0xbeb0_c6f7_a0b5_ed8d, 0xbe11_2e0b_e826_d695),
    ] {
        assert_eq!(
            super::duration_to_milliseconds(nanoseconds).to_bits(),
            milliseconds_bits,
            "Duration.to_ms must round signed fractional milliseconds",
        );
        assert_eq!(
            super::duration_to_seconds(nanoseconds).to_bits(),
            seconds_bits,
            "Duration.to_seconds must round signed fractional seconds",
        );
    }
    for (nanoseconds, milliseconds_bits, seconds_bits) in [
        (i128::MIN, 0xc6a0_c6f7_a0b5_ed8d, 0xc601_2e0b_e826_d695),
        (i128::MAX, 0x46a0_c6f7_a0b5_ed8d, 0x4601_2e0b_e826_d695),
    ] {
        assert_eq!(
            super::duration_to_milliseconds(nanoseconds).to_bits(),
            milliseconds_bits,
            "Duration.to_ms must support the complete signed i128 domain",
        );
        assert_eq!(
            super::duration_to_seconds(nanoseconds).to_bits(),
            seconds_bits,
            "Duration.to_seconds must support the complete signed i128 domain",
        );
    }
    assert_eq!(
        super::duration_to_milliseconds(-159_731_949_442_795_623_374_604_248_498_268_565_729)
            .to_bits(),
        0xc69f_8067_0f23_34c1,
        "Duration.to_ms must round the exact rational once, not double-round via an intermediate float",
    );
    assert_eq!(
        super::duration_to_seconds(36_342_747_987_658_862_963_516_616_932_497_802_072)
            .to_bits(),
        0x45dd_5b81_0f07_66c1,
        "Duration.to_seconds must round the exact rational once, not double-round via an intermediate float",
    );
    for (nanoseconds, expected_bits) in [
        (140_737_488_355_328_015_625, 0x42e0_0000_0000_0000),
        (140_737_488_355_328_046_875, 0x42e0_0000_0000_0002),
    ] {
        assert_eq!(
            super::duration_to_milliseconds(nanoseconds).to_bits(),
            expected_bits,
            "Duration.to_ms must resolve exact binary64 midpoints toward an even significand",
        );
    }
    for (nanoseconds, expected_bits) in [
        (17_592_186_044_416_001_953_125, 0x42b0_0000_0000_0000),
        (17_592_186_044_416_005_859_375, 0x42b0_0000_0000_0002),
    ] {
        assert_eq!(
            super::duration_to_seconds(nanoseconds).to_bits(),
            expected_bits,
            "Duration.to_seconds must resolve exact binary64 midpoints toward an even significand",
        );
    }
    assert_eq!(
        super::duration_to_seconds(33_554_431_999_999_999).to_bits(),
        0x4180_0000_0000_0000,
        "Duration.to_seconds must carry a rounded significand into the next binary64 exponent",
    );

    let timer = super::duration_to_host_timer(1_500_001, "test timeout")
        .expect("a small positive duration should fit the host timer");
    assert_eq!(timer.as_secs(), 0);
    assert_eq!(timer.subsec_nanos(), 1_500_001);

    let negative = super::duration_to_host_timer(-1, "test timeout")
        .expect_err("negative durations cannot become host timers");
    assert_eq!(negative.kind(), io::ErrorKind::InvalidInput);
    assert!(negative.to_string().contains("non-negative"));

    let too_wide = super::duration_to_host_timer(i128::MAX, "test timeout")
        .expect_err("a duration beyond u64 seconds cannot become a host timer");
    assert_eq!(too_wide.kind(), io::ErrorKind::InvalidInput);
    assert!(too_wide.to_string().contains("host timer range"));

    let deadline_overflow =
        super::duration_to_host_timer((u64::MAX as i128) * super::NANOS_PER_SECOND, "test timeout")
            .expect_err(
                "a representable host duration must not silently become an unlimited deadline",
            );
    assert_eq!(deadline_overflow.kind(), io::ErrorKind::InvalidInput);
    assert!(deadline_overflow.to_string().contains("deadline range"));

    assert_eq!(
        super::deadline_from_timeout(None).expect("an omitted timeout should be accepted"),
        None
    );
    let deadline_error = super::deadline_from_timeout(Some(StdDuration::MAX))
        .expect_err("an overflowing deadline must remain an error, never become unlimited");
    assert_eq!(deadline_error.kind(), io::ErrorKind::InvalidInput);
    assert!(deadline_error.to_string().contains("deadline range"));
}

#[test]
fn float_floor_divmod_matches_python_sign_precision_and_zero_rules() {
    for (left, right, quotient, remainder) in [
        (7.5, 2.0, 3.0, 1.5),
        (-7.5, 2.0, -4.0, 0.5),
        (7.5, -2.0, -4.0, -0.5),
        (-7.5, -2.0, 3.0, -1.5),
        (1.0, 0.1, 9.0, 0.099_999_999_999_999_95),
        (
            5e-300,
            1.300_000_000_000_000_1e-300,
            3.0,
            1.099_999_999_999_999_5e-300,
        ),
        (1e308, 3.0, 3.333_333_333_333_333e307, 2.0),
    ] {
        let (actual_quotient, actual_remainder) = float_floor_divmod(left, right);
        assert_eq!(actual_quotient, quotient);
        assert_eq!(actual_remainder, remainder);
    }

    let (negative_zero_quotient, negative_zero_remainder) = float_floor_divmod(0.0, -3.0);
    assert_eq!(negative_zero_quotient.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(negative_zero_remainder.to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn float_power_uses_destination_width_and_shared_exception_rules() {
    assert_eq!(
        float_power(1.5, 2.0, FloatPowerWidth::Float32).expect("float32 power should succeed"),
        (1.5_f32.powf(2.0)) as f64
    );
    assert_eq!(
        float_power(1.5, 2.0, FloatPowerWidth::Float64).expect("float64 power should succeed"),
        1.5_f64.powf(2.0)
    );

    let float32_overflow = float_power(100.0, 100.0, FloatPowerWidth::Float32)
        .expect_err("destination-width overflow must be diagnosed");
    assert_eq!(float32_overflow.code, "AU4002");
    assert!(float_power(100.0, 100.0, FloatPowerWidth::Float64).is_ok());

    assert_eq!(
        float_power(f64::NAN, 0.0, FloatPowerWidth::Float64).unwrap(),
        1.0
    );
    assert!(
        float_power(-2.0, 0.5, FloatPowerWidth::Float64)
            .expect_err("fractional power of a negative base is a domain error")
            .code
            == "AU4001"
    );
}

#[test]
fn numeric_round_preserves_integers_and_uses_checked_ties_to_even_int64_results() {
    let mut integer_values = [
        IntegerKind::Int8,
        IntegerKind::Int16,
        IntegerKind::Int32,
        IntegerKind::Int64,
        IntegerKind::Int128,
        IntegerKind::IntSize,
    ]
    .map(|kind| IntegerValue::from_typed_signed(-7, kind).unwrap())
    .to_vec();
    integer_values.extend(
        [
            IntegerKind::Uint8,
            IntegerKind::Uint16,
            IntegerKind::Uint32,
            IntegerKind::Uint64,
            IntegerKind::Uint128,
            IntegerKind::UintSize,
        ]
        .map(|kind| IntegerValue::from_typed_unsigned(7, kind).unwrap()),
    );
    integer_values.push(IntegerValue::from_i64(i64::MAX));
    for value in integer_values {
        assert_eq!(
            round_numeric_value(&Value::Int(value)).expect("integer round is identity"),
            Value::Int(value)
        );
    }

    for (value, expected) in [
        (0.0, 0),
        (-0.0, 0),
        (1.5, 2),
        (2.5, 2),
        (-1.5, -2),
        (-2.5, -2),
        (3.499_999_999, 3),
    ] {
        assert_eq!(
            round_numeric_value(&Value::Float(value)).expect("finite round should succeed"),
            Value::Int(IntegerValue::from_i64(expected)),
        );
    }

    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_223_372_036_854_775_808.0,
        -9_223_372_036_854_777_856.0,
    ] {
        let error = round_numeric_value(&Value::Float(value))
            .expect_err("invalid float-to-int64 round must trap");
        assert_eq!(error.code, "AU4002");
        assert!(error.message.contains("`round(...)`"));
    }

    let unsupported = round_numeric_value(&Value::String("1".to_string()))
        .expect_err("round's private runtime helper must validate its domain");
    assert_eq!(unsupported.code, "AU4001");
}

#[test]
fn numeric_divmod_returns_matching_typed_pairs_and_classifies_failures() {
    for kind in [
        IntegerKind::Int8,
        IntegerKind::Int16,
        IntegerKind::Int32,
        IntegerKind::Int64,
        IntegerKind::Int128,
        IntegerKind::IntSize,
    ] {
        let ty = Type::named(kind.runtime_type_name());
        let result = divmod_numeric_values(
            &Value::Int(IntegerValue::from_typed_signed(-7, kind).unwrap()),
            &Value::Int(IntegerValue::from_typed_signed(3, kind).unwrap()),
            &ty,
        )
        .expect("every signed integer width should use floor divmod");
        assert_eq!(
            result,
            Value::Tuple(TupleValue {
                element_types: vec![ty.clone(), ty],
                elements: vec![
                    Value::Int(IntegerValue::from_typed_signed(-3, kind).unwrap()),
                    Value::Int(IntegerValue::from_typed_signed(2, kind).unwrap()),
                ],
            })
        );
    }
    for kind in [
        IntegerKind::Uint8,
        IntegerKind::Uint16,
        IntegerKind::Uint32,
        IntegerKind::Uint64,
        IntegerKind::Uint128,
        IntegerKind::UintSize,
    ] {
        let ty = Type::named(kind.runtime_type_name());
        let result = divmod_numeric_values(
            &Value::Int(IntegerValue::from_typed_unsigned(7, kind).unwrap()),
            &Value::Int(IntegerValue::from_typed_unsigned(3, kind).unwrap()),
            &ty,
        )
        .expect("every unsigned integer width should preserve its exact kind");
        assert_eq!(
            result,
            Value::Tuple(TupleValue {
                element_types: vec![ty.clone(), ty],
                elements: vec![
                    Value::Int(IntegerValue::from_typed_unsigned(2, kind).unwrap()),
                    Value::Int(IntegerValue::from_typed_unsigned(1, kind).unwrap()),
                ],
            })
        );
    }

    for (left, right, quotient, remainder) in [
        (-7, 3, -3, 2),
        (7, -3, -3, -2),
        (-7, -3, 2, -1),
        (7, 3, 2, 1),
    ] {
        let ty = Type::named("int32");
        let result = divmod_numeric_values(
            &Value::Int(IntegerValue::from_typed_signed(left, IntegerKind::Int32).unwrap()),
            &Value::Int(IntegerValue::from_typed_signed(right, IntegerKind::Int32).unwrap()),
            &ty,
        )
        .expect("matching non-zero integers should produce a pair");
        assert_eq!(
            result,
            Value::Tuple(TupleValue {
                element_types: vec![ty.clone(), ty],
                elements: vec![
                    Value::Int(
                        IntegerValue::from_typed_signed(quotient, IntegerKind::Int32).unwrap()
                    ),
                    Value::Int(
                        IntegerValue::from_typed_signed(remainder, IntegerKind::Int32).unwrap()
                    ),
                ],
            })
        );
    }

    let float_ty = Type::named("float32");
    let floating = divmod_numeric_values(&Value::Float(-7.0), &Value::Float(3.0), &float_ty)
        .expect("matching floats should produce the corrected floor-divmod pair");
    assert_eq!(
        floating,
        Value::Tuple(TupleValue {
            element_types: vec![float_ty.clone(), float_ty],
            elements: vec![Value::Float(-3.0), Value::Float(2.0)],
        })
    );

    for (left, right, ty) in [
        (
            Value::Int(IntegerValue::from_i64(1)),
            Value::Int(IntegerValue::from_i64(0)),
            Type::named("int64"),
        ),
        (
            Value::Float(1.0),
            Value::Float(-0.0),
            Type::named("float64"),
        ),
    ] {
        let error = divmod_numeric_values(&left, &right, &ty)
            .expect_err("zero divisor must be classified uniformly");
        assert_eq!(error.code, "AU4004");
    }

    let min_overflow = divmod_numeric_values(
        &Value::Int(IntegerValue::from_i64(i64::MIN)),
        &Value::Int(IntegerValue::from_i64(-1)),
        &Type::named("int64"),
    )
    .expect_err("minimum divided by negative one must not panic");
    assert_eq!(min_overflow.code, "AU4002");
    assert_eq!(
        min_overflow.message,
        "`divmod(...)` integer quotient overflow"
    );

    let mathematical_quotient_overflow = divmod_numeric_values(
        &Value::Int(IntegerValue::from_literal(u128::MAX)),
        &Value::Int(IntegerValue::from_signed(-1)),
        &Type::named("int128"),
    )
    .expect_err("an unrepresentable negative uint128 quotient must be diagnosed");
    assert_eq!(mathematical_quotient_overflow.code, "AU4002");
    assert_eq!(
        mathematical_quotient_overflow.message,
        "`divmod(...)` integer quotient overflow"
    );

    let narrowed_remainder_overflow = divmod_numeric_values(
        &Value::Int(IntegerValue::from_signed(1_000)),
        &Value::Int(IntegerValue::from_signed(700)),
        &Type::named("int8"),
    )
    .expect_err("a remainder outside the statically supplied integer width must be diagnosed");
    assert_eq!(narrowed_remainder_overflow.code, "AU4002");
    assert_eq!(
        narrowed_remainder_overflow.message,
        "`divmod(...)` integer remainder overflow"
    );

    let mismatch = divmod_numeric_values(
        &Value::Int(IntegerValue::from_i64(1)),
        &Value::Float(1.0),
        &Type::named("int64"),
    )
    .expect_err("the private helper must reject mismatched runtime domains");
    assert_eq!(mismatch.code, "AU4001");
}

#[test]
fn option_and_result_helpers_render_expected_variants() {
    assert_eq!(
        option_some(Value::Int(IntegerValue::from_signed(7))).render(),
        "Option.Some(7)"
    );
    assert_eq!(option_none().render(), "Option.None");
    assert_eq!(result_ok(Value::Bool(true)).render(), "Result.Ok(true)");
    assert_eq!(
        result_err(Value::String("oops".to_string())).render(),
        "Result.Err(oops)"
    );
    assert_eq!(
        send_error_closed(Value::Int(IntegerValue::from_signed(3))).render(),
        "SendError.Closed(3)"
    );
    assert_eq!(
        send_error_cancelled(Value::Int(IntegerValue::from_signed(4))).render(),
        "SendError.Cancelled(4)"
    );
}

#[test]
fn async_and_process_result_helpers_render_expected_variants() {
    fn assert_variant(
        value: Value,
        enum_name: &str,
        variant_name: &str,
        expected_payloads: Vec<Value>,
    ) {
        let Value::EnumVariant(variant) = value else {
            panic!("expected {enum_name}.{variant_name} to render as an enum variant");
        };
        assert_eq!(variant.enum_name, enum_name);
        assert_eq!(variant.variant_name, variant_name);
        assert_eq!(variant.payloads, expected_payloads);
    }

    let payload = Value::Int(IntegerValue::from_signed(5));
    assert_variant(
        send_error_timed_out(payload.clone()),
        "SendError",
        "TimedOut",
        vec![payload.clone()],
    );
    assert_variant(
        send_error_full(payload.clone()),
        "SendError",
        "Full",
        vec![payload.clone()],
    );

    assert_variant(
        queue_receive_item(payload.clone()),
        "QueueReceive",
        "Item",
        vec![payload.clone()],
    );
    assert_variant(queue_receive_closed(), "QueueReceive", "Closed", Vec::new());
    assert_variant(
        queue_receive_timed_out(),
        "QueueReceive",
        "TimedOut",
        Vec::new(),
    );
    assert_variant(
        queue_receive_cancelled(),
        "QueueReceive",
        "Cancelled",
        Vec::new(),
    );

    let ready = Value::String("done".to_string());
    assert_variant(
        task_result_ready(ready.clone()),
        "TaskResult",
        "Ready",
        vec![ready.clone()],
    );
    assert_variant(
        task_result_error("boom".to_string()),
        "TaskResult",
        "Error",
        vec![Value::String("boom".to_string())],
    );
    assert_variant(
        task_result_timed_out(),
        "TaskResult",
        "TimedOut",
        Vec::new(),
    );
    assert_variant(
        task_result_cancelled(),
        "TaskResult",
        "Cancelled",
        Vec::new(),
    );

    assert_variant(
        wait_any_ready(2, ready.clone()),
        "WaitAny",
        "Ready",
        vec![Value::Int(IntegerValue::from_i64(2)), ready.clone()],
    );
    assert_variant(
        wait_any_error(3, "failed".to_string()),
        "WaitAny",
        "Error",
        vec![
            Value::Int(IntegerValue::from_i64(3)),
            Value::String("failed".to_string()),
        ],
    );
    assert_variant(wait_any_timed_out(), "WaitAny", "TimedOut", Vec::new());
    assert_variant(wait_any_cancelled(), "WaitAny", "Cancelled", Vec::new());

    let all_ready = vec![payload.clone(), ready.clone()];
    assert_variant(
        wait_all_ready(all_ready.clone()),
        "WaitAll",
        "Ready",
        vec![Value::Vec(VecValue {
            element_type: Type::named("Unknown"),
            elements: all_ready,
        })],
    );
    assert_variant(
        wait_all_error(4, "bad".to_string()),
        "WaitAll",
        "Error",
        vec![
            Value::Int(IntegerValue::from_i64(4)),
            Value::String("bad".to_string()),
        ],
    );
    assert_variant(wait_all_timed_out(), "WaitAll", "TimedOut", Vec::new());
    assert_variant(wait_all_cancelled(), "WaitAll", "Cancelled", Vec::new());

    for outcome in [
        wait_any_ready(0, Value::Unit),
        wait_any_error(0, "failed".to_string()),
        wait_all_error(0, "failed".to_string()),
        select_outcome_queue(0, queue_receive_closed()),
        select_outcome_task(0, task_result_cancelled()),
        select_outcome_deadline(0),
    ] {
        let Value::EnumVariant(variant) = outcome else {
            panic!("expected indexed concurrency outcome")
        };
        let Some(Value::Int(index)) = variant.payloads.first() else {
            panic!("expected indexed concurrency outcome payload")
        };
        assert_eq!(index.runtime_type_name(), Some("int64"));
    }

    assert_variant(process_wait_timed_out(), "Wait", "TimedOut", Vec::new());
    assert_variant(process_wait_cancelled(), "Wait", "Cancelled", Vec::new());

    let spawn_error = process_error_spawn("missing executable".to_string());
    assert_variant(
        spawn_error.clone(),
        "Error",
        "Spawn",
        vec![Value::String("missing executable".to_string())],
    );
    assert_variant(
        process_wait_failed(spawn_error.clone()),
        "Wait",
        "Failed",
        vec![spawn_error],
    );

    let event_error = process_error_other("crash".to_string());
    let supervisor_event = process_supervisor_event_failed(
        "worker".to_string(),
        event_error.clone(),
        IntegerValue::from_signed(2),
    );
    assert_variant(
        supervisor_event.clone(),
        "SupervisorEvent",
        "Failed",
        vec![
            Value::String("worker".to_string()),
            event_error,
            Value::Int(IntegerValue::from_signed(2)),
        ],
    );
    assert_variant(
        process_supervisor_wait_event(supervisor_event.clone()),
        "SupervisorWait",
        "Event",
        vec![supervisor_event],
    );
    assert_variant(
        process_supervisor_wait_timed_out(),
        "SupervisorWait",
        "TimedOut",
        Vec::new(),
    );
    assert_variant(
        process_supervisor_wait_cancelled(),
        "SupervisorWait",
        "Cancelled",
        Vec::new(),
    );

    assert_variant(process_error_no_command(), "Error", "NoCommand", Vec::new());
    assert_variant(process_error_timed_out(), "Error", "TimedOut", Vec::new());
    assert_variant(process_error_cancelled(), "Error", "Cancelled", Vec::new());
}

#[test]
fn process_config_decoders_report_unknown_and_wrong_variants() {
    let stdio_variant = |variant_name: &str| {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.Stdio".to_string(),
            variant_name: variant_name.to_string(),
            payloads: Vec::new(),
        })
    };
    assert!(matches!(
        decode_process_stdio(&stdio_variant("Inherit"), "stdin").expect("Inherit should decode"),
        ProcessStdioConfig::Inherit
    ));
    assert!(matches!(
        decode_process_stdio(&stdio_variant("Null"), "stdin").expect("Null should decode"),
        ProcessStdioConfig::Null
    ));
    assert!(matches!(
        decode_process_stdio(&stdio_variant("Pipe"), "stdout").expect("Pipe should decode"),
        ProcessStdioConfig::Pipe
    ));
    let unknown_stdio = match decode_process_stdio(&stdio_variant("Bogus"), "stdin") {
        Ok(_) => panic!("unknown stdio variants should fail"),
        Err(error) => error,
    };
    assert!(unknown_stdio
        .message
        .contains("unknown `process.Stdio` variant"));
    let wrong_stdio = match decode_process_stdio(&Value::Bool(true), "stdin") {
        Ok(_) => panic!("wrong stdio values should fail"),
        Err(error) => error,
    };
    assert!(wrong_stdio.message.contains("expects `process.Stdio`"));

    let restart_variant = |variant_name: &str| {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.RestartPolicy".to_string(),
            variant_name: variant_name.to_string(),
            payloads: Vec::new(),
        })
    };
    assert_eq!(
        decode_process_restart_policy(&restart_variant("Never"), "restart")
            .expect("Never should decode"),
        ProcessRestartPolicy::Never
    );
    assert_eq!(
        decode_process_restart_policy(&restart_variant("OnFailure"), "restart")
            .expect("OnFailure should decode"),
        ProcessRestartPolicy::OnFailure
    );
    assert_eq!(
        decode_process_restart_policy(&restart_variant("Always"), "restart")
            .expect("Always should decode"),
        ProcessRestartPolicy::Always
    );
    assert!(
        decode_process_restart_policy(&restart_variant("Bogus"), "restart")
            .expect_err("unknown restart variants should fail")
            .message
            .contains("unknown `process.RestartPolicy` variant")
    );
    assert!(
        decode_process_restart_policy(&Value::String("Always".to_string()), "restart")
            .expect_err("wrong restart values should fail")
            .message
            .contains("expects `process.RestartPolicy`")
    );
}

#[test]
fn cast_numeric_value_covers_success_and_failure_paths() {
    assert_eq!(
        cast_numeric_value(
            Value::Int(IntegerValue::from_signed(5)),
            &Type::named("int8"),
            None
        )
        .expect("integer-to-integer cast should preserve in-range values"),
        Value::Int(IntegerValue::from_signed(5))
    );

    assert_eq!(
        cast_numeric_value(
            Value::Int(IntegerValue::from_signed(5)),
            &Type::named("float64"),
            None
        )
        .expect("int to float cast should succeed"),
        Value::Float(5.0)
    );

    assert_eq!(
        cast_numeric_value(Value::Float(3.0), &Type::named("int32"), None)
            .expect("float to int cast should succeed"),
        Value::Int(IntegerValue::from_signed(3))
    );

    let overflow = cast_numeric_value(
        Value::Float(500.0),
        &Type::named("int8"),
        Some(Span::new(4, 9)),
    )
    .expect_err("narrow integer overflow should fail");
    assert!(overflow.message.contains("does not fit in `int8`"));

    let non_numeric = cast_numeric_value(
        Value::String("Aura".to_string()),
        &Type::named("int32"),
        Some(Span::new(2, 3)),
    )
    .expect_err("non-numeric casts should fail");
    assert!(non_numeric
        .message
        .contains("casts are only supported between numeric types"));

    let tuple_cast = cast_numeric_value(
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
        &Type::named("int64"),
        Some(Span::new(3, 7)),
    )
    .expect_err("tuples are structural values, not numeric cast sources");
    assert_eq!(
        tuple_cast.message,
        "casts are only supported between numeric types, found `tuple` and `int64`"
    );
    assert_eq!(tuple_cast.span, Some(Span::new(3, 7)));

    let integer_to_non_numeric = cast_numeric_value(
        Value::Int(IntegerValue::from_signed(5)),
        &Type::named("str"),
        None,
    )
    .expect_err("integer casts to nonnumeric targets should fail");
    assert!(
        integer_to_non_numeric
            .message
            .contains("found `integer` and `str`"),
        "unexpected integer cast diagnostic: {}",
        integer_to_non_numeric.message
    );

    let float_to_non_numeric = cast_numeric_value(Value::Float(1.5), &Type::named("str"), None)
        .expect_err("float casts to nonnumeric targets should fail");
    assert!(float_to_non_numeric
        .message
        .contains("found `float64` and `str`"));

    let non_finite = cast_numeric_value(Value::Float(f64::INFINITY), &Type::named("int32"), None)
        .expect_err("non-finite float casts to integers should fail");
    assert!(non_finite
        .message
        .contains("cannot cast non-finite float to `int32`"));

    let unsigned_negative = cast_numeric_value(Value::Float(-1.0), &Type::named("uint8"), None)
        .expect_err("negative float casts to unsigned integers should fail");
    assert!(unsigned_negative
        .message
        .contains("does not fit in `uint8`"));

    assert_eq!(
        cast_numeric_value(Value::Float(42.0), &Type::named("uint8"), None)
            .expect("float to unsigned integer cast should succeed"),
        Value::Int(IntegerValue::from_literal(42))
    );

    let unsigned_rounding_overflow =
        cast_numeric_value(Value::Float(u64::MAX as f64), &Type::named("uint64"), None)
            .expect_err("rounded float casts outside uint64 range should fail");
    assert!(unsigned_rounding_overflow
        .message
        .contains("does not fit in `uint64`"));

    assert_eq!(
        cast_numeric_value(Value::Float(3.25), &Type::named("float32"), None)
            .expect("float64 to float32 cast should succeed"),
        Value::Float((3.25f32) as f64)
    );
    assert_eq!(
        cast_numeric_value(Value::Float(3.25), &Type::named("float64"), None)
            .expect("float64 to float64 cast should succeed"),
        Value::Float(3.25)
    );

    let float64_precision = cast_numeric_value(
        Value::Int(IntegerValue::from_literal((1u128 << 53) + 1)),
        &Type::named("float64"),
        Some(Span::new(6, 7)),
    )
    .expect_err("precision-losing int64 to float64 casts should fail");
    assert!(float64_precision
        .message
        .contains("cannot be represented exactly as `float64`"));

    let float32_precision = cast_numeric_value(
        Value::Int(IntegerValue::from_literal((1u128 << 24) + 1)),
        &Type::named("float32"),
        Some(Span::new(8, 9)),
    )
    .expect_err("precision-losing int to float32 casts should fail");
    assert!(float32_precision
        .message
        .contains("cannot be represented exactly as `float32`"));
}

#[test]
fn cast_numeric_value_reports_source_types_for_runtime_values() {
    fn assert_source_type(value: Value, expected_source: &str) {
        let error = cast_numeric_value(value, &Type::named("int32"), None)
            .expect_err("non-numeric runtime values should not cast to integers");
        assert!(
            error
                .message
                .contains(&format!("found `{expected_source}` and `int32`")),
            "unexpected diagnostic for {expected_source}: {}",
            error.message
        );
    }

    assert_source_type(Value::Bool(true), "bool");
    assert_source_type(Value::String("Aura".to_string()), "str");
    assert_source_type(
        Value::Vec(VecValue {
            element_type: Type::named("int32"),
            elements: vec![],
        }),
        "list",
    );
    assert_source_type(
        Value::Set(SetValue {
            element_type: Type::named("str"),
            elements: vec![],
        }),
        "set",
    );
    assert_source_type(
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![],
        }),
        "dict",
    );
    assert_source_type(
        Value::Array(
            ArrayValue::zeros(ArrayDType::Int32, vec![1].into_boxed_slice())
                .expect("a cast diagnostic test array should be constructible"),
        ),
        "Array",
    );
    assert_source_type(Value::Duration(5), "Duration");
    assert_source_type(Value::Range(RangeValue { start: 1, end: 3 }), "Range");
    assert_source_type(
        Value::ModuleNamespace(ModuleNamespaceValue {
            path: "pkg.tools".to_string(),
        }),
        "module pkg.tools",
    );
    assert_source_type(Value::Unit, "None");
    assert_source_type(
        Value::Instance(super::InstanceValue {
            class_name: "Widget".to_string(),
            fields: Default::default(),
        }),
        "Widget",
    );
    assert_source_type(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: vec![],
        }),
        "Status",
    );

    let rng = RngValue::from_seed(42);
    let same_rng = rng.clone();
    let other_rng = RngValue::from_seed(42);
    assert_eq!(format!("{rng:?}"), "RngValue(..)");
    assert_eq!(rng, same_rng);
    assert_ne!(rng, other_rng);
    assert_value_equals_clone(Value::Rng(rng.clone()));
    assert_eq!(Value::Rng(rng.clone()).render(), "<rng>");
    assert_source_type(Value::Rng(rng), "random.Rng");

    let channel = ChannelValue::new();
    assert_eq!(format!("{channel:?}"), "ChannelValue(..)");
    assert_eq!(channel, channel.clone());
    assert_value_equals_clone(Value::Channel(channel.clone()));
    assert_eq!(Value::Channel(channel.clone()).render(), "<queue>");
    assert_source_type(Value::Channel(channel.clone()), "Queue");

    let task = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    assert_eq!(format!("{task:?}"), "TaskValue(..)");
    assert_eq!(task, task.clone());
    assert_value_equals_clone(Value::Task(task.clone()));
    assert_eq!(Value::Task(task.clone()).render(), "<task>");
    assert_source_type(Value::Task(task.clone()), "Task");
    wait_task_ready(&task).expect("test task should complete");

    let cancellation = CancellationContext::default();
    let task_group = TaskGroupValue::new(&cancellation);
    assert_eq!(format!("{task_group:?}"), "TaskGroupValue(..)");
    assert_eq!(task_group, task_group.clone());
    assert_value_equals_clone(Value::TaskGroup(task_group.clone()));
    assert_eq!(Value::TaskGroup(task_group.clone()).render(), "<tasks>");
    assert_source_type(Value::TaskGroup(task_group.clone()), "TaskGroup");

    let temp = TempDir::new("aura-runtime-value-cast");
    let file_path = temp.path().join("data.txt");
    let file = FileValue::create(file_path.to_str().expect("utf8 temp path"))
        .expect("temp file should be created");
    assert_eq!(format!("{file:?}"), "FileValue(..)");
    assert_eq!(file, file.clone());
    assert_value_equals_clone(Value::File(file.clone()));
    assert_eq!(Value::File(file.clone()).render(), "<file>");
    assert_source_type(Value::File(file.clone()), "fs.File");
    file.close();

    let tcp_listener =
        TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind locally");
    assert_eq!(format!("{tcp_listener:?}"), "TcpListenerValue(..)");
    assert_eq!(tcp_listener, tcp_listener.clone());
    let tcp_address = tcp_listener
        .local_addr()
        .expect("tcp listener should expose a local address");
    let accept_listener = tcp_listener.clone();
    let accept_thread = thread::spawn(move || {
        accept_listener
            .accept(Some(StdDuration::from_secs(1)), None)
            .expect("tcp listener should accept local client")
    });
    let tcp_stream = TcpStreamValue::connect(&tcp_address, Some(StdDuration::from_secs(1)), None)
        .expect("tcp stream should connect locally");
    let accepted_stream = accept_thread
        .join()
        .expect("tcp accept worker should join successfully");
    assert_eq!(format!("{tcp_stream:?}"), "TcpStreamValue(..)");
    assert_eq!(tcp_stream, tcp_stream.clone());
    assert_value_equals_clone(Value::TcpListener(tcp_listener.clone()));
    assert_value_equals_clone(Value::TcpStream(tcp_stream.clone()));
    assert_eq!(
        Value::TcpListener(tcp_listener.clone()).render(),
        "<tcp-listener>"
    );
    assert_eq!(
        Value::TcpStream(tcp_stream.clone()).render(),
        "<tcp-stream>"
    );
    assert_source_type(Value::TcpListener(tcp_listener.clone()), "net.TcpListener");
    assert_source_type(Value::TcpStream(tcp_stream.clone()), "net.TcpStream");
    tcp_stream.close();
    accepted_stream.close();
    tcp_listener.close();

    let udp_socket = UdpSocketValue::bind("127.0.0.1:0").expect("udp socket should bind locally");
    assert_eq!(format!("{udp_socket:?}"), "UdpSocketValue(..)");
    assert_eq!(udp_socket, udp_socket.clone());
    assert_value_equals_clone(Value::UdpSocket(udp_socket.clone()));
    assert_eq!(
        Value::UdpSocket(udp_socket.clone()).render(),
        "<udp-socket>"
    );
    assert_source_type(Value::UdpSocket(udp_socket.clone()), "net.UdpSocket");
    udp_socket.close();
    assert_value_equals_clone(Value::UdpDatagram(UdpDatagramValue {
        address: "127.0.0.1:9".to_string(),
        data: vec![1, 2, 3],
    }));
    assert_eq!(
        Value::UdpDatagram(UdpDatagramValue {
            address: "127.0.0.1:9".to_string(),
            data: vec![1, 2, 3],
        })
        .render(),
        "<udp-datagram 127.0.0.1:9 3 bytes>"
    );
    assert_source_type(
        Value::UdpDatagram(UdpDatagramValue {
            address: "127.0.0.1:9".to_string(),
            data: vec![1, 2, 3],
        }),
        "net.UdpDatagram",
    );
    assert_eq!(
        Value::HttpResponse(HttpResponseValue {
            status: 200,
            reason: "OK".to_string(),
            headers: vec![],
            body: vec![1, 2],
        })
        .render(),
        "<http-response 200 2 bytes>"
    );
    assert_value_equals_clone(Value::HttpResponse(HttpResponseValue {
        status: 200,
        reason: "OK".to_string(),
        headers: vec![],
        body: vec![1, 2],
    }));
    assert_source_type(
        Value::HttpResponse(HttpResponseValue {
            status: 200,
            reason: "OK".to_string(),
            headers: vec![],
            body: vec![],
        }),
        "net.HttpResponse",
    );

    let http_listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind locally");
    assert_eq!(format!("{http_listener:?}"), "HttpListenerValue(..)");
    assert_eq!(http_listener, http_listener.clone());
    assert_value_equals_clone(Value::HttpListener(http_listener.clone()));
    assert_eq!(
        Value::HttpListener(http_listener.clone()).render(),
        "<http-listener>"
    );
    assert_source_type(
        Value::HttpListener(http_listener.clone()),
        "net.HttpListener",
    );
    http_listener.close();

    let websocket_listener = WebSocketListenerValue::bind("127.0.0.1:0")
        .expect("websocket listener should bind locally");
    assert_eq!(
        format!("{websocket_listener:?}"),
        "WebSocketListenerValue(..)"
    );
    assert_eq!(websocket_listener, websocket_listener.clone());
    assert_value_equals_clone(Value::WebSocketListener(websocket_listener.clone()));
    assert_eq!(
        Value::WebSocketListener(websocket_listener.clone()).render(),
        "<websocket-listener>"
    );
    assert_source_type(
        Value::WebSocketListener(websocket_listener.clone()),
        "net.WebSocketListener",
    );

    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        vec![],
        vec![],
    );
    assert_eq!(format!("{completed:?}"), "ProcessCompletedValue(..)");
    assert_eq!(completed, completed.clone());
    assert_value_equals_clone(Value::ProcessCompleted(completed.clone()));
    assert_eq!(
        Value::ProcessCompleted(completed.clone()).render(),
        "<process-completed process.ExitStatus.Exited(0)>"
    );
    let failed_completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(2))],
        }),
        vec![],
        vec![],
    );
    assert!(failed_completed
        .check()
        .expect_err("non-zero completed processes should fail check()")
        .render()
        .contains("process exited with process.ExitStatus.Exited(2)"));
    assert_source_type(Value::ProcessCompleted(completed), "process.Completed");

    let child = ProcessChildValue::spawn(
        vec![
            std::env::current_exe()
                .expect("current test binary should be available")
                .to_string_lossy()
                .into_owned(),
            "--help".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("process child should spawn");
    let stdout_pipe = child.stdout().expect("stdout pipe should be captured");
    assert_eq!(format!("{child:?}"), "ProcessChildValue(..)");
    assert_eq!(child, child.clone());
    assert_value_equals_clone(Value::ProcessChild(child.clone()));
    assert_eq!(
        Value::ProcessChild(child.clone()).render(),
        "<process-child>"
    );
    assert_source_type(Value::ProcessChild(child.clone()), "process.Child");
    assert_eq!(format!("{stdout_pipe:?}"), "ProcessPipeValue(..)");
    assert_eq!(stdout_pipe, stdout_pipe.clone());
    assert_value_equals_clone(Value::ProcessPipe(stdout_pipe.clone()));
    assert_eq!(
        Value::ProcessPipe(stdout_pipe.clone()).render(),
        "<process-pipe>"
    );
    assert_source_type(Value::ProcessPipe(stdout_pipe.clone()), "process.Pipe");
    let _ = child.wait(Some(StdDuration::from_secs(1)), None);
    child.close();

    let failed_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 3".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("failing child should spawn");
    let error = failed_child
        .wait_ok(Some(StdDuration::from_secs(2)), None)
        .expect_err("wait_ok should reject non-zero exits");
    assert!(error.render().contains("ExitStatus.Exited(3)"));

    let timed_out_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 1".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("sleep child should spawn");
    let error = timed_out_child
        .wait_ok(Some(StdDuration::ZERO), None)
        .expect_err("wait_ok should surface timeouts");
    assert_eq!(error.render(), "Error.TimedOut");
    timed_out_child.close();

    let cancelled_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 1".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("cancellable child should spawn");
    let cancel_group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = cancel_group.child_cancellation();
    cancel_group.cancel();
    let error = cancelled_child
        .wait_ok(Some(StdDuration::from_secs(2)), Some(&cancellation))
        .expect_err("wait_ok should surface cancellation");
    assert_eq!(error.render(), "Error.Cancelled");
    cancelled_child.close();

    #[cfg(unix)]
    {
        let grouped_child = ProcessChildValue::spawn(
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 5".to_string(),
            ],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            true,
        )
        .expect("grouped sleep child should spawn");
        grouped_child.close();
    }

    let supervisor = ProcessSupervisorValue::new();
    assert_eq!(format!("{supervisor:?}"), "ProcessSupervisorValue(..)");
    assert_eq!(supervisor, supervisor.clone());
    assert_value_equals_clone(Value::ProcessSupervisor(supervisor.clone()));
    assert_eq!(
        Value::ProcessSupervisor(supervisor.clone()).render(),
        "<process-supervisor>"
    );
    assert_source_type(Value::ProcessSupervisor(supervisor), "process.Supervisor");

    #[cfg(unix)]
    {
        let unix_socket_path = unique_unix_socket_path("a-rv");
        let unix_path = unix_socket_path
            .to_str()
            .expect("unix socket path should be utf8");
        let unix_listener =
            UnixListenerValue::bind(unix_path).expect("unix listener should bind locally");
        assert_eq!(format!("{unix_listener:?}"), "UnixListenerValue(..)");
        assert_eq!(unix_listener, unix_listener.clone());
        assert_value_equals_clone(Value::UnixListener(unix_listener.clone()));
        assert_eq!(
            Value::UnixListener(unix_listener.clone()).render(),
            "<unix-listener>"
        );
        assert_source_type(
            Value::UnixListener(unix_listener.clone()),
            "net.UnixListener",
        );
        let unix_server = unix_listener.clone();
        let unix_accept = thread::spawn(move || {
            unix_server
                .accept(Some(StdDuration::from_secs(1)), None)
                .expect("unix listener should accept local client")
        });
        let unix_client = UnixStreamValue::connect(
            unix_path,
            Some(StdDuration::from_secs(1)),
            Some(&CancellationContext::default()),
        )
        .expect("unix stream should connect locally");
        let unix_stream = unix_accept
            .join()
            .expect("unix accept worker should join successfully");
        assert_eq!(format!("{unix_client:?}"), "UnixStreamValue(..)");
        assert_eq!(unix_client, unix_client.clone());
        assert_value_equals_clone(Value::UnixStream(unix_client.clone()));
        assert_eq!(
            Value::UnixStream(unix_client.clone()).render(),
            "<unix-stream>"
        );
        assert_source_type(Value::UnixStream(unix_client.clone()), "net.UnixStream");
        unix_client.close();
        unix_stream.close();
        unix_listener.close();
        let _ = fs::remove_file(unix_socket_path);
    }
}

#[test]
fn channel_runtime_helpers_cover_send_receive_and_close_paths() {
    let channel = ChannelValue::new();
    assert_eq!(channel.try_recv(), TryRecvResult::Empty);

    channel
        .send(Value::Int(IntegerValue::from_signed(5)))
        .expect("send should succeed on open channel");
    assert_eq!(
        channel.try_recv(),
        TryRecvResult::Value(Value::Int(IntegerValue::from_signed(5)))
    );

    channel.close();
    assert_eq!(channel.try_recv(), TryRecvResult::Closed);
    assert_eq!(
        channel
            .send(Value::Bool(true))
            .expect_err("closed channel should reject sends"),
        Value::Bool(true)
    );
    assert!(channel
        .recv_with_cancellation(None, None)
        .expect("an omitted queue timeout cannot overflow")
        .is_none());

    let bounded = ChannelValue::with_capacity(1);
    bounded
        .send(Value::Unit)
        .expect("bounded channel should accept one queued value");
    assert_eq!(
        bounded
            .try_send_result(Value::Bool(false))
            .expect_err("full bounded channel should reject try_send_result"),
        super::SendValueError::Full(Box::new(Value::Bool(false)))
    );
    assert_eq!(
        bounded
            .send_with_deadline(Value::Bool(true), None, None, true)
            .expect_err("fail-fast bounded sends should report full"),
        super::SendValueError::Full(Box::new(Value::Bool(true)))
    );
    assert_eq!(
        bounded
            .send_with_timeout(Value::Bool(true), Some(StdDuration::ZERO), None)
            .expect("zero fits the host deadline range")
            .expect_err("timed bounded sends should report timeout when capacity stays full"),
        super::SendValueError::TimedOut(Box::new(Value::Bool(true)))
    );
    let cancel_flag = Arc::new(super::RuntimeWakeSignal::new(true));
    let cancelled = CancellationContext {
        flags: vec![cancel_flag],
    };
    assert_eq!(
        bounded
            .send_with_cancellation(Value::Bool(true), Some(&cancelled))
            .expect_err("cancelled bounded sends should report cancellation"),
        super::SendValueError::Cancelled(Box::new(Value::Bool(true)))
    );

    let producer_channel = ChannelValue::new();
    let producer = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    producer_channel.register_producer_task(&producer);
    producer_channel.register_task_handle(&producer);
    assert_eq!(
        wait_task_ready(&producer).expect("producer task should complete"),
        Value::Unit
    );
    drop(producer);
    for _ in 0..100 {
        if producer_channel.registered_producer_tasks().is_empty()
            && producer_channel.registered_task_handles().is_empty()
        {
            break;
        }
        thread::sleep(StdDuration::from_millis(1));
    }
    assert!(producer_channel.registered_producer_tasks().is_empty());
    assert!(producer_channel.registered_task_handles().is_empty());
    assert!(producer_channel.all_registered_producer_tasks_completed());

    assert_eq!(
        channel
            .try_send_result(Value::Bool(false))
            .expect_err("closed channel should reject try_send_result"),
        super::SendValueError::Closed(Box::new(Value::Bool(false)))
    );
    for error in [
        super::SendValueError::Closed(Box::new(Value::Int(IntegerValue::from_signed(1)))),
        super::SendValueError::Cancelled(Box::new(Value::Int(IntegerValue::from_signed(2)))),
        super::SendValueError::TimedOut(Box::new(Value::Int(IntegerValue::from_signed(3)))),
        super::SendValueError::Full(Box::new(Value::Int(IntegerValue::from_signed(4)))),
    ] {
        let value = *error.clone().into_value();
        let expected = match error {
            super::SendValueError::Closed(value)
            | super::SendValueError::Cancelled(value)
            | super::SendValueError::TimedOut(value)
            | super::SendValueError::Full(value) => *value,
        };
        assert_eq!(value, expected);
    }
}

fn runtime_json(value: crate::json_codec::JsonValue) -> Value {
    super::json_value_to_runtime(value)
        .expect("test JSON values should fit the runtime materialization budget")
}

#[test]
fn phase73_observable_runtime_json_clone_preserves_empty_containers_and_nested_buffers() {
    use crate::json_codec::JsonValue;

    fn container_buffers(value: &Value, output: &mut Vec<usize>) {
        let Value::EnumVariant(variant) = value else {
            return;
        };
        match variant.payloads.as_slice() {
            [Value::Vec(vector)] => {
                if !vector.elements.is_empty() {
                    output.push(vector.elements.as_ptr() as usize);
                }
                for element in &vector.elements {
                    container_buffers(element, output);
                }
            }
            [Value::Map(map)] => {
                if !map.entries.is_empty() {
                    output.push(map.entries.as_ptr() as usize);
                }
                for (_, value) in &map.entries {
                    container_buffers(value, output);
                }
            }
            _ => {}
        }
    }

    let expected = JsonValue::object(vec![
        ("empty_array".to_string(), JsonValue::Array(Vec::new())),
        ("empty_object".to_string(), JsonValue::Object(Vec::new())),
        (
            "nested".to_string(),
            JsonValue::Array(vec![
                JsonValue::Null,
                JsonValue::Bool(false),
                JsonValue::Int(-7),
                JsonValue::Float(2.5),
                JsonValue::String("aura".to_string()),
                JsonValue::object(vec![
                    ("left".to_string(), JsonValue::Int(1)),
                    ("right".to_string(), JsonValue::Int(2)),
                ]),
            ]),
        ),
    ]);
    let original = runtime_json(expected.clone());
    let cloned = original.clone();

    assert_eq!(
        super::runtime_value_to_json(&cloned)
            .expect("a copied public json.Value must remain serializable"),
        expected,
    );
    let mut original_buffers = Vec::new();
    let mut cloned_buffers = Vec::new();
    container_buffers(&original, &mut original_buffers);
    container_buffers(&cloned, &mut cloned_buffers);
    assert_eq!(original_buffers.len(), cloned_buffers.len());
    assert!(
        original_buffers
            .iter()
            .zip(&cloned_buffers)
            .all(|(original, cloned)| original != cloned),
        "copying json.Value must rebuild every non-empty Array and Object buffer",
    );
}

#[test]
fn dynamic_json_runtime_conversion_round_trips_every_variant_and_preserves_object_slots() {
    use crate::json_codec::JsonValue;

    let json = JsonValue::object(vec![
        ("null".to_string(), JsonValue::Null),
        ("bool".to_string(), JsonValue::Bool(true)),
        ("int".to_string(), JsonValue::Int(i64::MIN)),
        ("float".to_string(), JsonValue::Float(1.5)),
        (
            "array".to_string(),
            JsonValue::Array(vec![JsonValue::String("value".to_string())]),
        ),
        (
            "object".to_string(),
            JsonValue::object(vec![("nested".to_string(), JsonValue::Int(i64::MAX))]),
        ),
    ]);

    let runtime = runtime_json(json.clone());
    let Value::EnumVariant(root) = &runtime else {
        panic!("json.Value.Object should use Aura's enum runtime representation");
    };
    assert_eq!(root.enum_name, "json.Value");
    assert_eq!(root.variant_name, "Object");
    let [Value::Map(object)] = root.payloads.as_slice() else {
        panic!("json.Value.Object should carry dict[str, json.Value]");
    };
    assert_eq!(object.key_type, Type::named("str"));
    assert_eq!(object.value_type, Type::named("json.Value"));
    assert_eq!(
        object
            .entries
            .iter()
            .map(|(key, _)| key.render())
            .collect::<Vec<_>>(),
        vec!["null", "bool", "int", "float", "array", "object"]
    );
    assert_eq!(
        super::runtime_value_to_json(&runtime)
            .expect("well-formed json.Value should convert back to the shared codec"),
        json
    );
}

#[test]
fn dynamic_json_parse_runtime_materialization_allocation_failure_is_au4005() {
    let error = super::with_json_runtime_allocation_budget(0, || {
        super::evaluate_host_builtin("json::parse", vec![Value::String("[null]".to_string())])
    })
    .expect_err("runtime-tree allocation failure must trap instead of aborting or returning Err");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing parsed JSON"
    );
}

#[test]
fn dynamic_json_parse_maps_the_public_node_limit_to_au4005() {
    let element_count = crate::json_codec::MAX_JSON_VALUE_NODES;
    let mut source = String::with_capacity(element_count.saturating_mul(5).saturating_add(2));
    source.push('[');
    for index in 0..element_count {
        if index > 0 {
            source.push(',');
        }
        source.push_str("null");
    }
    source.push(']');

    let error = super::json_parse_to_runtime(&source)
        .expect_err("the root plus one value beyond the public node cap must trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "JSON value exceeds the maximum materialized node count of 262144"
    );
}

#[test]
fn dynamic_json_shared_parse_adapter_borrows_the_source_allocation() {
    let source = "[null]".repeat(64);
    let source_ptr = source.as_ptr();
    let args = vec![Value::String(source)];

    let borrowed = super::host_string_ref_arg(&args, 0, "json::parse")
        .expect("json.parse should borrow a str argument");

    assert_eq!(borrowed.as_ptr(), source_ptr);
}

#[test]
fn dynamic_json_parse_materializes_a_structurally_dense_valid_input() {
    const ELEMENTS: usize = 16_384;
    let source = format!("[{}]", vec!["null"; ELEMENTS].join(","));

    let parsed = super::evaluate_host_builtin("json::parse", vec![Value::String(source)])
        .expect("a structurally dense input below the byte limit should parse");
    let Value::EnumVariant(result) = parsed else {
        panic!("json.parse should return Result");
    };
    assert_eq!(
        (result.enum_name.as_str(), result.variant_name.as_str()),
        ("Result", "Ok")
    );
    let [Value::EnumVariant(array)] = result.payloads.as_slice() else {
        panic!("Result.Ok should contain json.Value.Array");
    };
    let [Value::Vec(values)] = array.payloads.as_slice() else {
        panic!("json.Value.Array should contain list[json.Value]");
    };
    assert_eq!(values.element_type, Type::named("json.Value"));
    assert_eq!(values.elements.len(), ELEMENTS);
    assert!(values.elements.first().is_some_and(|value| {
        matches!(
            value,
            Value::EnumVariant(variant)
                if variant.enum_name == "json.Value"
                    && variant.variant_name == "Null"
                    && variant.payloads.is_empty()
        )
    }));
    assert!(values.elements.last().is_some_and(|value| {
        matches!(
            value,
            Value::EnumVariant(variant)
                if variant.enum_name == "json.Value"
                    && variant.variant_name == "Null"
                    && variant.payloads.is_empty()
        )
    }));
}

#[test]
fn dynamic_json_runtime_conversions_enforce_the_shared_node_limit() {
    use crate::json_codec::JsonValue;

    let value = JsonValue::Array(vec![JsonValue::Null, JsonValue::Null, JsonValue::Null]);
    let runtime =
        super::with_json_runtime_node_limit(4, || super::json_value_to_runtime(value.clone()))
            .expect("the root plus three elements should fit an exact four-node limit");
    let materialized =
        super::with_json_runtime_node_limit(4, || super::runtime_value_to_json(&runtime))
            .expect("dump conversion should accept the exact four-node boundary");
    assert_eq!(materialized, value);

    for operation in ["parse conversion", "dump conversion"] {
        let error = super::with_json_runtime_node_limit(3, || match operation {
            "parse conversion" => super::json_value_to_runtime(value.clone()).map(|_| ()),
            "dump conversion" => super::runtime_value_to_json(&runtime).map(|_| ()),
            _ => unreachable!(),
        })
        .expect_err("the fourth value node must exceed a three-node limit");
        assert_eq!(error.code, "AU4005", "{operation}");
        assert_eq!(
            error.message, "JSON value exceeds the maximum materialized node count of 3",
            "{operation}"
        );
    }

    let object = JsonValue::Object(vec![(
        "a very long object key".to_string(),
        JsonValue::Null,
    )]);
    let runtime_object =
        super::with_json_runtime_node_limit(2, || super::json_value_to_runtime(object.clone()))
            .expect("an object key must not consume a value-node slot");
    assert_eq!(
        super::with_json_runtime_node_limit(2, || {
            super::runtime_value_to_json(&runtime_object)
        })
        .expect("dump conversion must also exclude object keys from the node count"),
        object
    );
}

#[test]
fn dynamic_json_parse_node_budget_precedes_container_allocations() {
    use crate::json_codec::JsonValue;

    let cases = [
        (
            "array",
            JsonValue::Array(vec![JsonValue::Null, JsonValue::Null]),
        ),
        (
            "object",
            JsonValue::Object(vec![
                ("first".to_string(), JsonValue::Null),
                ("second".to_string(), JsonValue::Null),
            ]),
        ),
    ];

    for (label, value) in cases {
        let result = super::with_json_runtime_allocation_budget(0, || {
            super::with_json_runtime_node_limit(2, || super::json_value_to_runtime(value.clone()))
        });
        let error = result.expect_err(
            "the node-limit diagnostic must precede an injected materialization allocation failure",
        );
        assert_eq!(error.code, "AU4005", "{label}");
        assert_eq!(
            error.message, "JSON value exceeds the maximum materialized node count of 2",
            "{label}"
        );

        let allocation_error = super::with_json_runtime_allocation_budget(0, || {
            super::with_json_runtime_node_limit(3, || super::json_value_to_runtime(value.clone()))
        })
        .expect_err("an exact-fit container should proceed to its first fallible allocation");
        assert_eq!(allocation_error.code, "AU4005", "{label}");
        assert_eq!(
            allocation_error.message, "memory allocation failed while materializing parsed JSON",
            "{label}"
        );
    }
}

#[test]
fn dynamic_json_dump_node_budget_precedes_container_and_key_allocations() {
    use crate::json_codec::JsonValue;

    let cases = [
        (
            "array",
            JsonValue::Array(vec![JsonValue::Null, JsonValue::Null]),
        ),
        (
            "object",
            JsonValue::Object(vec![
                ("first".to_string(), JsonValue::Null),
                ("second".to_string(), JsonValue::Null),
            ]),
        ),
    ];

    for (label, value) in cases {
        let runtime = runtime_json(value.clone());
        assert_eq!(
            super::with_json_runtime_node_limit(3, || { super::runtime_value_to_json(&runtime) })
                .unwrap_or_else(|error| panic!(
                    "{label} should fit the exact three-node limit: {error}"
                )),
            value,
            "{label}"
        );

        let result = super::with_json_runtime_conversion_allocation_budget(0, || {
            super::with_json_runtime_node_limit(2, || super::runtime_value_to_json(&runtime))
        });
        let error = result.expect_err(
            "the node-limit diagnostic must precede an injected conversion allocation failure",
        );
        assert_eq!(error.code, "AU4005", "{label}");
        assert_eq!(
            error.message, "JSON value exceeds the maximum materialized node count of 2",
            "{label}"
        );

        let allocation_error = super::with_json_runtime_conversion_allocation_budget(0, || {
            super::with_json_runtime_node_limit(3, || super::runtime_value_to_json(&runtime))
        })
        .expect_err("an exact-fit container should proceed to its first fallible allocation");
        assert_eq!(allocation_error.code, "AU4005", "{label}");
        assert_eq!(
            allocation_error.message, "memory allocation failed while preparing JSON output",
            "{label}"
        );

        if label == "object" {
            let key_allocation_error =
                super::with_json_runtime_conversion_allocation_budget(1, || {
                    super::with_json_runtime_node_limit(3, || {
                        super::runtime_value_to_json(&runtime)
                    })
                })
                .expect_err(
                    "after the object buffer reserve, cloning its first key should remain fallible",
                );
            assert_eq!(key_allocation_error.code, "AU4005");
            assert_eq!(
                key_allocation_error.message,
                "memory allocation failed while preparing JSON output"
            );
        }
    }
}

#[test]
fn dynamic_json_runtime_reserve_failures_are_coded_resource_diagnostics() {
    let parse_error = super::json_parse_try_reserve(&mut Vec::<Value>::new(), usize::MAX)
        .expect_err("an impossible materialization reserve must be recoverable");
    assert_eq!(parse_error.code, "AU4005");
    assert_eq!(
        parse_error.message,
        "memory allocation failed while materializing parsed JSON"
    );

    let dump_error =
        super::json_runtime_conversion_try_reserve(&mut Vec::<Value>::new(), usize::MAX)
            .expect_err("an impossible dump-conversion reserve must be recoverable");
    assert_eq!(dump_error.code, "AU4005");
    assert!(
        dump_error
            .message
            .starts_with("memory allocation failed while preparing JSON output:"),
        "the allocator detail must remain attached to the dump diagnostic: {dump_error}"
    );
}

#[test]
fn dynamic_json_metadata_validation_is_structural_and_allocation_free() {
    use crate::json_codec::JsonValue;

    assert!(super::json_exact_nominal_type(
        &Type::Named("json.Value".to_string(), Vec::new()),
        "json.Value"
    ));
    assert!(!super::json_exact_nominal_type(
        &Type::Named("json.Value".to_string(), vec![Type::Unit]),
        "json.Value"
    ));
    assert!(!super::json_exact_nominal_type(
        &Type::TypeParam("json.Value".to_string()),
        "json.Value"
    ));

    for (name, value) in [
        ("json::into_array", JsonValue::Array(Vec::new())),
        ("json::into_object", JsonValue::Object(Vec::new())),
    ] {
        let runtime = runtime_json(value.clone());

        let converted = super::with_json_runtime_conversion_allocation_budget(0, || {
            super::runtime_value_to_json(&runtime)
        })
        .unwrap_or_else(|error| {
            panic!("{name} metadata validation should allocate no temporary type: {error}")
        });
        assert_eq!(converted, value, "{name}");

        let accessor = super::with_json_runtime_conversion_allocation_budget(0, || {
            super::evaluate_host_builtin(name, vec![runtime])
        })
        .unwrap_or_else(|error| {
            panic!("{name} metadata validation should allocate no temporary type: {error}")
        });
        assert!(
            accessor.render().starts_with("Option.Some("),
            "{name} should accept exact canonical metadata"
        );
    }
}

#[test]
fn dynamic_json_runtime_conversion_rejects_noncanonical_payload_metadata() {
    fn json_variant(variant_name: &str, payload: Value) -> Value {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "json.Value".to_string(),
            variant_name: variant_name.to_string(),
            payloads: vec![payload],
        })
    }

    let malformed = [
        (
            "int32 Int payload",
            json_variant("Int", Value::Int(IntegerValue::from_i32(7))),
            "exactly `int64`",
        ),
        (
            "untyped Int payload",
            json_variant("Int", Value::Int(IntegerValue::from_signed(7))),
            "exactly `int64`",
        ),
        (
            "wrong Array element metadata",
            json_variant(
                "Array",
                Value::Vec(VecValue {
                    element_type: Type::named("str"),
                    elements: Vec::new(),
                }),
            ),
            "exactly `list[json.Value]`",
        ),
        (
            "wrong Object key metadata",
            json_variant(
                "Object",
                Value::Map(MapValue {
                    key_type: Type::named("int64"),
                    value_type: Type::named("json.Value"),
                    entries: Vec::new(),
                }),
            ),
            "exactly `dict[str, json.Value]`",
        ),
        (
            "wrong Object value metadata",
            json_variant(
                "Object",
                Value::Map(MapValue {
                    key_type: Type::named("str"),
                    value_type: Type::named("str"),
                    entries: Vec::new(),
                }),
            ),
            "exactly `dict[str, json.Value]`",
        ),
    ];

    for (label, value, expected_message) in malformed {
        let error = super::runtime_value_to_json(&value).expect_err(label);
        assert_eq!(error.code, "AU4001", "{label}");
        assert!(error.message.contains(expected_message), "{label}: {error}");
    }

    for canonical in [
        crate::json_codec::JsonValue::Int(7),
        crate::json_codec::JsonValue::Array(Vec::new()),
        crate::json_codec::JsonValue::Object(Vec::new()),
    ] {
        let runtime = runtime_json(canonical.clone());
        assert_eq!(
            super::runtime_value_to_json(&runtime)
                .unwrap_or_else(|error| panic!("canonical metadata should pass: {error}")),
            canonical
        );
    }
}

#[test]
fn dynamic_json_rejects_deserialized_int64_metadata_with_an_out_of_range_payload() {
    let malformed_integer: IntegerValue = serde_json::from_value(serde_json::json!({
        "representation": { "Unsigned": 9_223_372_036_854_775_808_u64 },
        "runtime_kind": "Int64"
    }))
    .expect("the wire shape should deserialize so the runtime validator can reject its payload");
    assert_eq!(malformed_integer.runtime_kind(), Some(IntegerKind::Int64));

    let malformed = Value::EnumVariant(EnumVariantValue {
        enum_name: "json.Value".to_string(),
        variant_name: "Int".to_string(),
        payloads: vec![Value::Int(malformed_integer)],
    });
    let error = super::runtime_value_to_json(&malformed)
        .expect_err("exact int64 metadata must not mask an out-of-range serialized payload");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "malformed runtime `json.Value`: Value.Int payload is outside `int64`"
    );
}

#[test]
fn dynamic_json_accessors_reject_noncanonical_payload_metadata() {
    fn json_variant(variant_name: &str, payload: Value) -> Value {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "json.Value".to_string(),
            variant_name: variant_name.to_string(),
            payloads: vec![payload],
        })
    }

    let malformed = [
        (
            "json::as_int",
            json_variant("Int", Value::Int(IntegerValue::from_i32(7))),
        ),
        (
            "json::into_array",
            json_variant(
                "Array",
                Value::Vec(VecValue {
                    element_type: Type::named("str"),
                    elements: Vec::new(),
                }),
            ),
        ),
        (
            "json::into_object",
            json_variant(
                "Object",
                Value::Map(MapValue {
                    key_type: Type::named("int64"),
                    value_type: Type::named("json.Value"),
                    entries: Vec::new(),
                }),
            ),
        ),
        (
            "json::into_object",
            json_variant(
                "Object",
                Value::Map(MapValue {
                    key_type: Type::named("str"),
                    value_type: Type::named("str"),
                    entries: Vec::new(),
                }),
            ),
        ),
    ];

    for (name, value) in malformed {
        let error = super::evaluate_host_builtin(name, vec![value])
            .expect_err("malformed JSON payload metadata should be rejected");
        assert_eq!(error.code, "AU4001", "{name}: {error}");
    }

    for (name, canonical) in [
        ("json::as_int", crate::json_codec::JsonValue::Int(7)),
        (
            "json::into_array",
            crate::json_codec::JsonValue::Array(Vec::new()),
        ),
        (
            "json::into_object",
            crate::json_codec::JsonValue::Object(Vec::new()),
        ),
    ] {
        let result = super::evaluate_host_builtin(name, vec![runtime_json(canonical)])
            .unwrap_or_else(|error| panic!("{name} should accept canonical metadata: {error}"));
        assert!(
            result.render().starts_with("Option.Some("),
            "{name} should return Option.Some for the exact variant"
        );
    }
}

#[test]
fn dynamic_json_host_boundary_rejects_malformed_runtime_shapes() {
    fn variant(enum_name: &str, variant_name: &str, payloads: Vec<Value>) -> Value {
        Value::EnumVariant(EnumVariantValue {
            enum_name: enum_name.to_string(),
            variant_name: variant_name.to_string(),
            payloads,
        })
    }

    fn error(name: &str, value: Value) -> Diagnostic {
        super::evaluate_host_builtin(name, vec![value])
            .expect_err("a malformed runtime value must not cross the JSON host boundary")
    }

    for (name, value, expected) in [
        (
            "json::is_null",
            Value::Bool(false),
            "expected a runtime `json.Value`",
        ),
        (
            "json::is_null",
            variant("json.Value", "Null", vec![Value::Unit]),
            "malformed runtime `json.Value.Null` payload",
        ),
        (
            "json::as_bool",
            variant("json.Value", "Bool", Vec::new()),
            "malformed runtime `json.Value.Bool` payload",
        ),
        (
            "json::as_bool",
            variant("json.Value", "Bool", vec![Value::String("true".into())]),
            "malformed runtime `json.Value.Bool` payload",
        ),
        (
            "json::as_float",
            variant(
                "json.Value",
                "Float",
                vec![Value::Int(IntegerValue::from_i64(1))],
            ),
            "malformed runtime `json.Value.Float` payload",
        ),
        (
            "json::into_string",
            Value::String("not an enum".into()),
            "expected a runtime `json.Value`",
        ),
        (
            "json::into_string",
            variant("json.Value", "String", vec![Value::Bool(false)]),
            "malformed runtime `json.Value.String` payload",
        ),
        (
            "json::into_array",
            variant("other.Value", "Array", vec![Value::Unit]),
            "expected enum `json.Value`",
        ),
        (
            "json::into_object",
            variant("json.Value", "Object", Vec::new()),
            "malformed runtime `json.Value.Object` payload",
        ),
    ] {
        let diagnostic = error(name, value);
        assert_eq!(diagnostic.code, "AU4001", "{name}: {diagnostic}");
        assert!(
            diagnostic.message.contains(expected),
            "{name}: expected `{expected}`, found `{diagnostic}`"
        );
    }

    let wrong_key = variant(
        "json.Value",
        "Object",
        vec![Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("json.Value"),
            entries: vec![(
                Value::Int(IntegerValue::from_i64(1)),
                runtime_json(crate::json_codec::JsonValue::Null),
            )],
        })],
    );
    let diagnostic = super::runtime_value_to_json(&wrong_key)
        .expect_err("runtime object keys must be str values, not merely str metadata");
    assert_eq!(diagnostic.code, "AU4001");
    assert!(diagnostic.message.contains("Object key must be `str`"));

    for (label, value, expected) in [
        (
            "non-enum JSON root",
            Value::Bool(false),
            "expected `json.Value`, found `false`",
        ),
        (
            "wrong JSON enum root",
            variant("other.Value", "Null", Vec::new()),
            "expected enum `json.Value`, found `other.Value`",
        ),
    ] {
        let diagnostic = match super::runtime_value_to_json(&value) {
            Ok(converted) => {
                panic!("{label} unexpectedly converted to {converted:?}; expected {expected}")
            }
            Err(diagnostic) => diagnostic,
        };
        assert_eq!(diagnostic.code, "AU4001");
        assert!(
            diagnostic.message.contains(expected),
            "{label}: expected `{expected}`, found `{diagnostic}`"
        );
    }

    let wrong_later_key = variant(
        "json.Value",
        "Object",
        vec![Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("json.Value"),
            entries: vec![
                (
                    Value::String("valid".into()),
                    runtime_json(crate::json_codec::JsonValue::Null),
                ),
                (
                    Value::Int(IntegerValue::from_i64(2)),
                    runtime_json(crate::json_codec::JsonValue::Bool(true)),
                ),
            ],
        })],
    );
    let diagnostic = super::runtime_value_to_json(&wrong_later_key)
        .expect_err("every runtime object key must be validated, not only the first");
    assert_eq!(diagnostic.code, "AU4001");
    assert!(
        diagnostic
            .message
            .contains("Object key must be `str`, found `2`"),
        "unexpected later-key diagnostic: {diagnostic}"
    );

    for indent in [
        Value::Int(IntegerValue::from_i64(2)),
        variant("Option", "None", vec![Value::Unit]),
        variant("Option", "Some", vec![Value::String("2".into())]),
    ] {
        let diagnostic = super::evaluate_host_builtin(
            "json::dumps",
            vec![runtime_json(crate::json_codec::JsonValue::Null), indent],
        )
        .expect_err("json.dumps must reject malformed Option[int64] runtime values");
        assert_eq!(diagnostic.code, "AU4001");
        assert!(diagnostic.message.contains("expects `indent`"));
    }
}

#[test]
fn dynamic_json_materialization_counts_the_root_and_recovers_after_checkpoints() {
    let error = super::with_json_runtime_node_limit(0, || {
        super::json_value_to_runtime(crate::json_codec::JsonValue::Null)
    })
    .expect_err("the root value must consume one node from the shared materialization budget");
    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "JSON value exceeds the maximum materialized node count of 0"
    );

    let value = super::with_json_runtime_allocation_budget(16, || {
        super::json_value_to_runtime(crate::json_codec::JsonValue::object(vec![(
            "key".to_string(),
            crate::json_codec::JsonValue::String("value".to_string()),
        )]))
    })
    .expect("successful allocation checkpoints must decrement without changing the result");
    assert_eq!(
        super::runtime_value_to_json(&value).expect("materialized JSON must remain canonical"),
        crate::json_codec::JsonValue::object(vec![(
            "key".to_string(),
            crate::json_codec::JsonValue::String("value".to_string()),
        )])
    );
}

#[test]
fn dynamic_json_dumps_rejects_noncanonical_indent_integer_metadata() {
    for indent in [
        IntegerValue::from_i32(2),
        IntegerValue::from_signed(2),
        IntegerValue::from_u64(2),
    ] {
        let error = super::evaluate_host_builtin(
            "json::dumps",
            vec![
                runtime_json(crate::json_codec::JsonValue::Null),
                option_some(Value::Int(indent)),
            ],
        )
        .expect_err("json::dumps must require exact int64 indent metadata");
        assert_eq!(error.code, "AU4001");
        assert_eq!(
            error.message,
            "`json::dumps` expects `indent` to contain an `int64`"
        );
    }

    assert_eq!(
        super::evaluate_host_builtin(
            "json::dumps",
            vec![
                runtime_json(crate::json_codec::JsonValue::Null),
                option_some(Value::Int(IntegerValue::from_i64(2))),
            ],
        )
        .expect("canonical int64 indent metadata should remain valid"),
        Value::String("null".to_string())
    );
}

#[test]
fn dynamic_json_runtime_conversion_rejects_depth_before_building_an_unbounded_clone() {
    let mut value = runtime_json(crate::json_codec::JsonValue::Null);
    for _ in 0..=crate::json_codec::MAX_JSON_DEPTH {
        value = Value::EnumVariant(EnumVariantValue {
            enum_name: "json.Value".to_string(),
            variant_name: "Array".to_string(),
            payloads: vec![Value::Vec(VecValue {
                element_type: Type::named("json.Value"),
                elements: vec![value],
            })],
        });
    }

    let error = super::runtime_value_to_json(&value)
        .expect_err("runtime-to-codec conversion must enforce the dump depth limit");
    assert_eq!(error.code, "AU4003");
    assert_eq!(error.message, "JSON value exceeds the maximum depth of 128");
}

#[test]
fn dynamic_json_runtime_conversion_fits_a_forced_512_kib_task_stack_at_the_depth_limit() {
    fn nested_runtime_array(depth: usize) -> Value {
        let mut value = runtime_json(crate::json_codec::JsonValue::Null);
        for _ in 0..depth {
            value = Value::EnumVariant(EnumVariantValue {
                enum_name: "json.Value".to_string(),
                variant_name: "Array".to_string(),
                payloads: vec![Value::Vec(VecValue {
                    element_type: Type::named("json.Value"),
                    elements: vec![value],
                })],
            });
        }
        value
    }

    let maximum = nested_runtime_array(crate::json_codec::MAX_JSON_DEPTH);
    let too_deep = nested_runtime_array(crate::json_codec::MAX_JSON_DEPTH + 1);
    let result = run_lightweight_root_task(move || {
        let task = spawn_lightweight_task_with_stack(512 * 1024, move || {
            let cloned = maximum.clone();
            assert_eq!(cloned.render(), maximum.render());
            drop(cloned);
            let rendered = maximum.render();
            assert!(rendered.starts_with("json.Value.Array([json.Value.Array(["));
            assert!(rendered.contains("json.Value.Null"));
            assert!(rendered.ends_with("])"));
            let dumped = super::evaluate_host_builtin("json::dumps", vec![maximum, option_none()])
                .expect("the exact JSON depth limit should fit a 512 KiB task stack");
            assert_eq!(
                dumped,
                Value::String(format!(
                    "{}null{}",
                    "[".repeat(crate::json_codec::MAX_JSON_DEPTH),
                    "]".repeat(crate::json_codec::MAX_JSON_DEPTH)
                ))
            );
            let error = super::evaluate_host_builtin("json::dumps", vec![too_deep, option_none()])
                .expect_err("depth 129 must retain the public nesting diagnostic");
            assert_eq!(error.code, "AU4003");
            assert_eq!(error.message, "JSON value exceeds the maximum depth of 128");
            Ok(Value::Unit)
        })?;
        wait_task_ready(&task)?;
        Ok(Value::Unit)
    });

    assert!(
        result.is_ok(),
        "dynamic JSON conversion must fit a forced 512 KiB task stack: {result:?}"
    );
}

#[test]
fn json_codec_service_bounds_admission_and_recovers_permits_after_failures() {
    let recovery_pool = super::JsonCodecPool::start_with_limits(1, 1);
    let panic_pool = recovery_pool.clone();
    let (panic_result, panic_receiver) = std::sync::mpsc::channel();
    let panic_probe = thread::spawn(move || {
        let outcome = super::run_json_codec_operation_on_pool_with(
            super::reserve_json_codec_slot(&panic_pool),
            || -> std::result::Result<
                crate::json_codec::JsonValue,
                crate::json_codec::JsonCodecError,
            > { panic!("injected JSON codec panic") },
        );
        let _ = panic_result.send(outcome);
    });
    let panic = panic_receiver
        .recv_timeout(StdDuration::from_secs(1))
        .expect("panic containment must complete within one second")
        .expect_err("a codec panic must be contained");
    panic_probe
        .join()
        .expect("the bounded panic probe should join");
    assert!(matches!(
        panic,
        super::JsonCodecServiceError::Panicked(message)
            if message == "injected JSON codec panic"
    ));

    let clone_pool = recovery_pool.clone();
    let (clone_result, clone_receiver) = std::sync::mpsc::channel();
    let clone_probe = thread::spawn(move || {
        super::fail_next_json_codec_source_clone();
        let outcome = super::prepare_json_codec_source_with_pool(&clone_pool, || {
            super::clone_json_codec_source("null")
        })
        .err();
        let _ = clone_result.send(outcome);
    });
    let error = clone_receiver
        .recv_timeout(StdDuration::from_secs(1))
        .expect("post-panic reservation and clone failure must finish within one second")
        .expect("the injected source-clone failure must be reported");
    clone_probe
        .join()
        .expect("the bounded clone-failure probe should join");
    assert_eq!(error.code, "AU4005");

    let success_pool = recovery_pool.clone();
    let (success_result, success_receiver) = std::sync::mpsc::channel();
    let success_probe = thread::spawn(move || {
        let outcome = super::run_json_codec_operation_on_pool_with(
            super::reserve_json_codec_slot(&success_pool),
            || Ok(crate::json_codec::JsonValue::Null),
        );
        let _ = success_result.send(outcome);
    });
    assert_eq!(
        success_receiver
            .recv_timeout(StdDuration::from_secs(1))
            .expect("post-clone-failure reservation must finish within one second")
            .expect("panic and clone failure must each restore the sole permit"),
        crate::json_codec::JsonValue::Null
    );
    success_probe
        .join()
        .expect("the bounded recovery probe should join");

    fn submit_blocker(pool: &Arc<super::JsonCodecPool>, release: Arc<AtomicBool>) {
        let reservation = super::reserve_json_codec_slot(pool);
        let result = Arc::new(Mutex::new(None));
        let completion = ChannelValue::new();
        reservation.submit(super::JsonCodecJob {
            operation: Box::new(move || {
                while !release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                Ok(crate::json_codec::JsonValue::Null)
            }),
            result,
            completion,
        });
    }

    let saturated_pool = super::JsonCodecPool::start_with_limits(1, 2);
    let release = Arc::new(AtomicBool::new(false));
    let _release_guard = AtomicReleaseGuard(release.clone());
    submit_blocker(&saturated_pool, release.clone());
    submit_blocker(&saturated_pool, release.clone());
    let watchdog_fired = Arc::new(AtomicBool::new(false));
    let watchdog_release = release.clone();
    let watchdog_flag = watchdog_fired.clone();
    let (watchdog_done, watchdog_cancel) = std::sync::mpsc::channel();
    let watchdog = thread::spawn(move || {
        if watchdog_cancel
            .recv_timeout(StdDuration::from_secs(1))
            .is_err()
        {
            watchdog_flag.store(true, Ordering::SeqCst);
            watchdog_release.store(true, Ordering::SeqCst);
        }
    });

    super::reset_json_codec_source_clone_count();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let parse_pool = saturated_pool.clone();
        let parse = spawn_lightweight_task(move || {
            let (source, reservation) =
                super::prepare_json_codec_source_with_pool(&parse_pool, || {
                    super::clone_json_codec_source("null")
                })?;
            let parsed = super::run_json_codec_operation_on_pool_with(reservation, move || {
                crate::json_codec::parse(&source)
            })
            .map_err(|error| Diagnostic::new(format!("{error:?}")))?;
            assert_eq!(parsed, crate::json_codec::JsonValue::Null);
            Ok(Value::Unit)
        })?;
        let release_in_timer = release.clone();
        let timer = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(10), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            assert_eq!(
                super::json_codec_source_clone_count(),
                0,
                "saturated admission must precede the fallible source clone"
            );
            release_in_timer.store(true, Ordering::SeqCst);
            Ok(Value::Unit)
        })?;
        wait_task_ready(&timer)?;
        wait_task_ready(&parse)?;
        Ok(Value::Unit)
    });
    let _ = watchdog_done.send(());
    watchdog
        .join()
        .expect("codec admission watchdog should join");
    assert!(
        result.is_ok(),
        "saturated codec admission must park while sibling timers progress: {result:?}"
    );
    assert!(
        !watchdog_fired.load(Ordering::SeqCst),
        "saturated codec admission exceeded the bounded one-second watchdog"
    );

    let cancellation_pool = super::JsonCodecPool::start_with_limits(1, 1);
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let group = TaskGroupValue::new(&CancellationContext::default());
        let cancellation = group.child_cancellation();
        let parse = spawn_lightweight_task_with_cancellation(cancellation, move || {
            let parsed = super::run_json_codec_operation_on_pool_with(
                super::reserve_json_codec_slot(&cancellation_pool),
                || {
                    thread::sleep(StdDuration::from_millis(30));
                    Ok(crate::json_codec::JsonValue::Null)
                },
            )
            .map_err(|error| Diagnostic::new(format!("{error:?}")))?;
            assert_eq!(parsed, crate::json_codec::JsonValue::Null);
            let cancellation = super::current_lightweight_task_cancellation()
                .expect("the cancellable parse task has a current cancellation context");
            assert!(
                super::poll_cancellation(&cancellation),
                "the next ordinary cancellation boundary must observe deferred cancellation"
            );
            Ok(Value::Unit)
        })?;
        let cancel_group = group.clone();
        let canceller = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(5), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            cancel_group.cancel();
            Ok(Value::Unit)
        })?;
        wait_task_ready(&canceller)?;
        wait_task_ready(&parse)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "cancellation must remain deferred until synchronous JSON parse completes: {result:?}"
    );
}

#[test]
fn json_codec_non_task_admission_waits_for_capacity_before_cloning() {
    let pool = super::JsonCodecPool::start_with_limits(1, 1);
    let release = Arc::new(AtomicBool::new(false));
    let worker_started = Arc::new(AtomicBool::new(false));
    let blocker_result = Arc::new(Mutex::new(None));
    let blocker_completion = ChannelValue::new();
    let release_in_worker = release.clone();
    let started_in_worker = worker_started.clone();
    super::reserve_json_codec_slot(&pool).submit(super::JsonCodecJob {
        operation: Box::new(move || {
            started_in_worker.store(true, Ordering::SeqCst);
            while !release_in_worker.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            Ok(crate::json_codec::JsonValue::Null)
        }),
        result: blocker_result,
        completion: blocker_completion,
    });

    let start_wait = Instant::now();
    while !worker_started.load(Ordering::SeqCst) {
        assert!(
            start_wait.elapsed() < StdDuration::from_secs(1),
            "the sole codec worker must start the capacity-blocking job"
        );
        thread::yield_now();
    }

    let clone_called = Arc::new(AtomicBool::new(false));
    let clone_called_in_waiter = clone_called.clone();
    let pool_in_waiter = pool.clone();
    let (waiter_started_tx, waiter_started_rx) = std::sync::mpsc::channel();
    let (prepared_tx, prepared_rx) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        waiter_started_tx
            .send(())
            .expect("admission waiter should announce its start");
        let prepared = super::prepare_json_codec_source_with_pool(&pool_in_waiter, || {
            clone_called_in_waiter.store(true, Ordering::SeqCst);
            Ok("null".to_string())
        });
        prepared_tx
            .send(prepared)
            .expect("admission waiter should publish its outcome");
    });
    waiter_started_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("non-task admission waiter should start");
    thread::sleep(StdDuration::from_millis(20));
    assert!(
        !clone_called.load(Ordering::SeqCst),
        "bounded admission must happen before cloning the JSON source"
    );

    release.store(true, Ordering::SeqCst);
    let (source, reservation) = prepared_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("capacity release should wake the non-task admission waiter")
        .expect("post-admission source preparation should succeed");
    assert_eq!(source, "null");
    assert!(
        clone_called.load(Ordering::SeqCst),
        "the source clone should run after capacity becomes available"
    );
    drop(reservation);
    waiter.join().expect("admission waiter should join");

    let clone_count_before = super::json_codec_source_clone_count();
    let (empty, recovered) =
        super::prepare_json_codec_source_with_pool(&pool, || super::clone_json_codec_source(""))
            .expect("an empty source must clone without allocating after capacity recovers");
    assert!(empty.is_empty());
    assert_eq!(
        super::json_codec_source_clone_count(),
        clone_count_before + 1,
        "the empty source must still cross the post-admission clone boundary"
    );
    drop(recovered);
}

#[test]
fn dynamic_json_host_builtins_parse_dump_and_expose_exact_typed_accessors() {
    use crate::json_codec::JsonValue;

    fn call(name: &str, args: Vec<Value>) -> Value {
        super::evaluate_host_builtin(name, args)
            .unwrap_or_else(|error| panic!("{name} should succeed: {error}"))
    }

    let parsed = call(
        "json::parse",
        vec![Value::String(
            r#"{"z":1.0,"items":[true,null,"x"],"f":1.5}"#.to_string(),
        )],
    );
    let Value::EnumVariant(result) = parsed else {
        panic!("json.parse should return Result");
    };
    assert_eq!(
        (result.enum_name.as_str(), result.variant_name.as_str()),
        ("Result", "Ok")
    );
    let [value] = result.payloads.as_slice() else {
        panic!("Result.Ok should carry one json.Value");
    };
    assert_eq!(
        super::runtime_value_to_json(value).expect("parsed runtime value should be well formed"),
        JsonValue::object(vec![
            ("z".to_string(), JsonValue::Int(1)),
            (
                "items".to_string(),
                JsonValue::Array(vec![
                    JsonValue::Bool(true),
                    JsonValue::Null,
                    JsonValue::String("x".to_string()),
                ]),
            ),
            ("f".to_string(), JsonValue::Float(1.5)),
        ])
    );
    assert_eq!(
        call("json::dumps", vec![value.clone(), option_none()]),
        Value::String(r#"{"f":1.5,"items":[true,null,"x"],"z":1}"#.to_string())
    );
    assert_eq!(
        call(
            "json::dumps",
            vec![
                value.clone(),
                option_some(Value::Int(IntegerValue::from_i64(2))),
            ],
        ),
        Value::String(
            "{\n  \"f\": 1.5,\n  \"items\": [\n    true,\n    null,\n    \"x\"\n  ],\n  \"z\": 1\n}"
                .to_string()
        )
    );

    let null = runtime_json(JsonValue::Null);
    let boolean = runtime_json(JsonValue::Bool(true));
    let integer = runtime_json(JsonValue::Int(7));
    let float = runtime_json(JsonValue::Float(1.5));
    let string = runtime_json(JsonValue::String("aura".to_string()));
    let array = runtime_json(JsonValue::Array(vec![JsonValue::Int(2)]));
    let object = runtime_json(JsonValue::object(vec![(
        "k".to_string(),
        JsonValue::Bool(false),
    )]));
    let string_payload_ptr = match &string {
        Value::EnumVariant(variant) => match variant.payloads.as_slice() {
            [Value::String(value)] => value.as_ptr(),
            _ => panic!("json.Value.String should contain one str"),
        },
        _ => panic!("json.Value.String should be an enum variant"),
    };
    let array_payload_ptr = match &array {
        Value::EnumVariant(variant) => match variant.payloads.as_slice() {
            [Value::Vec(value)] => value.elements.as_ptr(),
            _ => panic!("json.Value.Array should contain one Vec"),
        },
        _ => panic!("json.Value.Array should be an enum variant"),
    };
    let object_payload_ptr = match &object {
        Value::EnumVariant(variant) => match variant.payloads.as_slice() {
            [Value::Map(value)] => value.entries.as_ptr(),
            _ => panic!("json.Value.Object should contain one Map"),
        },
        _ => panic!("json.Value.Object should be an enum variant"),
    };

    assert_eq!(call("json::is_null", vec![null]), Value::Bool(true));
    assert_eq!(
        call("json::is_null", vec![integer.clone()]),
        Value::Bool(false)
    );
    assert_eq!(
        call("json::as_bool", vec![boolean]).render(),
        "Option.Some(true)"
    );
    assert_eq!(
        call("json::as_bool", vec![runtime_json(JsonValue::Null)]).render(),
        "Option.None"
    );
    assert_eq!(
        call("json::as_int", vec![integer.clone()]).render(),
        "Option.Some(7)"
    );
    assert_eq!(
        call("json::as_int", vec![runtime_json(JsonValue::Bool(true))]).render(),
        "Option.None"
    );
    assert_eq!(
        call("json::as_float", vec![float]).render(),
        "Option.Some(1.5)"
    );
    assert_eq!(
        call("json::as_float", vec![integer]).render(),
        "Option.None",
        "typed accessors must not coerce Int to Float"
    );
    let Value::EnumVariant(string_option) = call("json::into_string", vec![string]) else {
        panic!("json.into_string should return Option");
    };
    assert!(matches!(
        string_option.payloads.as_slice(),
        [Value::String(value)]
            if value == "aura" && value.as_ptr() == string_payload_ptr
    ));
    let Value::EnumVariant(array_option) = call("json::into_array", vec![array]) else {
        panic!("json.into_array should return Option");
    };
    assert!(matches!(
        array_option.payloads.as_slice(),
        [Value::Vec(VecValue { elements, .. })]
            if elements == &vec![runtime_json(JsonValue::Int(2))]
                && elements.as_ptr() == array_payload_ptr
    ));
    let Value::EnumVariant(object_option) = call("json::into_object", vec![object]) else {
        panic!("json.into_object should return Option");
    };
    assert!(matches!(
        object_option.payloads.as_slice(),
        [Value::Map(MapValue { entries, .. })]
            if entries.len() == 1
                && entries[0].0 == Value::String("k".to_string())
                && entries.as_ptr() == object_payload_ptr
    ));
    for (name, expected_variant) in [
        ("json::into_string", JsonValue::Null),
        ("json::into_array", JsonValue::Bool(false)),
        ("json::into_object", JsonValue::Int(1)),
    ] {
        assert_eq!(
            call(name, vec![runtime_json(expected_variant)]).render(),
            "Option.None"
        );
    }
}

#[test]
fn dynamic_json_runtime_maps_typed_parse_errors_and_dump_trap_categories() {
    use crate::json_codec::{JsonCodecError, JsonValue, MAX_JSON_DEPTH, MAX_JSON_OUTPUT_BYTES};

    fn call(name: &str, args: Vec<Value>) -> Result<Value, Diagnostic> {
        super::evaluate_host_builtin(name, args)
    }

    let nested = format!(
        "{}0{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "]".repeat(MAX_JSON_DEPTH + 1)
    );
    for (source, expected_variant, expected_ints) in [
        ("{\"x\":".to_string(), "Syntax", vec![1, 6]),
        ("1e400".to_string(), "NumberOutOfRange", vec![1, 1]),
        (
            nested,
            "NestingTooDeep",
            vec![MAX_JSON_DEPTH as i128, 1, (MAX_JSON_DEPTH + 1) as i128],
        ),
    ] {
        let parsed = call("json::parse", vec![Value::String(source)])
            .expect("parse data failures should return typed Result.Err values");
        let Value::EnumVariant(result) = parsed else {
            panic!("json.parse should return Result");
        };
        assert_eq!(result.variant_name, "Err");
        let [Value::EnumVariant(error)] = result.payloads.as_slice() else {
            panic!("Result.Err should contain json.Error");
        };
        assert_eq!(error.enum_name, "json.Error");
        assert_eq!(error.variant_name, expected_variant);
        let actual_ints = error
            .payloads
            .iter()
            .filter_map(|value| match value {
                Value::Int(value) => value.as_i128(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_ints, expected_ints);
        for value in error.payloads.iter().filter_map(|value| match value {
            Value::Int(value) => Some(value),
            _ => None,
        }) {
            assert_eq!(
                value.runtime_kind(),
                Some(crate::integer::IntegerKind::Int32)
            );
        }
    }

    let oversized = super::json_parse_error_value(JsonCodecError::InputTooLarge {
        actual_bytes: 67_108_865,
        limit_bytes: 67_108_864,
    })
    .expect("typed input limit error should materialize");
    let Value::EnumVariant(oversized) = oversized else {
        panic!("input limit failure should use json.Error");
    };
    assert_eq!(oversized.variant_name, "InputTooLarge");
    assert!(matches!(
        oversized.payloads.as_slice(),
        [Value::Int(actual), Value::Int(limit)]
            if actual.runtime_kind() == Some(crate::integer::IntegerKind::Int64)
                && limit.runtime_kind() == Some(crate::integer::IntegerKind::Int64)
    ));

    let invalid_indent = call(
        "json::dumps",
        vec![
            runtime_json(JsonValue::Null),
            option_some(Value::Int(IntegerValue::from_i64(17))),
        ],
    )
    .expect_err("indent above sixteen should trap");
    assert_eq!(invalid_indent.code, "AU4003");

    let non_finite = call(
        "json::dumps",
        vec![runtime_json(JsonValue::Float(f64::INFINITY)), option_none()],
    )
    .expect_err("non-finite floats should trap");
    assert_eq!(non_finite.code, "AU4001");

    let mut too_deep = JsonValue::Null;
    for _ in 0..=MAX_JSON_DEPTH {
        too_deep = JsonValue::Array(vec![too_deep]);
    }
    let depth = call("json::dumps", vec![runtime_json(too_deep), option_none()])
        .expect_err("dump nesting above the limit should trap");
    assert_eq!(depth.code, "AU4003");

    for codec_error in [
        JsonCodecError::OutputTooLarge {
            limit_bytes: MAX_JSON_OUTPUT_BYTES as u64,
        },
        JsonCodecError::MaterializationTooLarge {
            limit: crate::json_codec::MAX_JSON_VALUE_NODES,
        },
        JsonCodecError::AllocationFailed,
    ] {
        let diagnostic = super::json_dump_error_to_diagnostic(codec_error);
        assert_eq!(diagnostic.code, "AU4005");
    }

    let mut impossible = String::new();
    let allocation = impossible
        .try_reserve(usize::MAX)
        .expect_err("impossible capacity should fail deterministically");
    let diagnostic = super::json_runtime_allocation_error(allocation);
    assert_eq!(diagnostic.code, "AU4005");
    assert!(diagnostic.message.contains("preparing JSON output"));
}

#[test]
fn legacy_json_validity_walks_nested_arrays_and_objects() {
    for source in [
        r#"{"outer":{"items":[null,true,1.5,"text"]}}"#,
        r#"[{"left":1},{"right":[2,3]}]"#,
    ] {
        assert_eq!(
            super::evaluate_host_builtin("json::is_valid", vec![Value::String(source.to_string())])
                .expect("json.is_valid should accept a str"),
            Value::Bool(true),
            "valid nested JSON should remain valid: {source}"
        );
    }

    assert_eq!(
        super::evaluate_host_builtin(
            "json::is_valid",
            vec![Value::String(r#"{"outer":{"number":1e400}}"#.to_string())],
        )
        .expect("json.is_valid should report invalid data as false"),
        Value::Bool(false)
    );
}

#[test]
fn host_control_plane_builtins_cover_success_and_error_boundaries() {
    fn call(name: &str, args: Vec<Value>) -> Value {
        super::evaluate_host_builtin(name, args)
            .unwrap_or_else(|error| panic!("{name} should succeed: {error}"))
    }
    fn string_map(entries: &[(&str, &str)]) -> Value {
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("str"),
            entries: entries
                .iter()
                .map(|(key, value)| {
                    (
                        Value::String((*key).to_string()),
                        Value::String((*value).to_string()),
                    )
                })
                .collect(),
        })
    }

    let previous_program_args = std::env::var_os("AURA_PROGRAM_ARGS_JSON");
    std::env::set_var("AURA_PROGRAM_ARGS_JSON", "[\"spoofed\"]");
    let Value::Vec(actual_args) = call("sys::args", vec![]) else {
        panic!("sys.args should return a Vec");
    };
    assert_eq!(
        actual_args.elements,
        super::host_process_args()
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>(),
        "the retired environment transport must not override real host argv"
    );
    let explicit_args = vec!["alpha".to_string(), "beta".to_string()];
    let Value::Vec(explicit) =
        super::evaluate_host_builtin_with_program_args("sys::args", vec![], &explicit_args)
            .expect("an explicit MIR argv context should be accepted")
    else {
        panic!("sys.args should return a Vec");
    };
    assert_eq!(
        explicit.elements,
        vec![
            Value::String("alpha".to_string()),
            Value::String("beta".to_string())
        ]
    );
    match previous_program_args {
        Some(value) => std::env::set_var("AURA_PROGRAM_ARGS_JSON", value),
        None => std::env::remove_var("AURA_PROGRAM_ARGS_JSON"),
    }
    assert_eq!(
        call(
            "sys::env",
            vec![Value::String(
                "AURA_TEST_ENV_THAT_DOES_NOT_EXIST_52D3".to_string()
            )]
        )
        .render(),
        "Option.None"
    );
    assert!(call("sys::current_dir", vec![])
        .render()
        .starts_with("Result.Ok("));
    assert!(matches!(call("sys::unix_time_ms", vec![]), Value::Int(_)));
    assert!(matches!(
        call("sys::monotonic_time_ms", vec![]),
        Value::Int(_)
    ));
    assert!(super::host_millis_value(u128::MAX, "test clock").is_err());

    assert_eq!(
        call(
            "path::join",
            vec![Value::String("a".into()), Value::String("b".into())]
        ),
        Value::String(
            std::path::Path::new("a")
                .join("b")
                .to_string_lossy()
                .to_string()
        )
    );
    assert_eq!(
        call("path::parent", vec![Value::String("a/b".into())]).render(),
        "Option.Some(a)"
    );
    assert_eq!(
        call("path::file_name", vec![Value::String("a/b.au".into())]).render(),
        "Option.Some(b.au)"
    );
    assert_eq!(
        call("path::extension", vec![Value::String("a/b.au".into())]).render(),
        "Option.Some(au)"
    );
    assert_eq!(
        call(
            "path::extension",
            vec![Value::String("no-extension".into())]
        )
        .render(),
        "Option.None"
    );
    assert_eq!(
        call("path::is_absolute", vec![Value::String("relative".into())]),
        Value::Bool(false)
    );

    let labels = string_map(&[("name", "aura")]);
    assert_eq!(
        call("json::is_valid", vec![Value::String("[]".into())]),
        Value::Bool(true)
    );
    assert_eq!(
        call("json::is_valid", vec![Value::String("{".into())]),
        Value::Bool(false)
    );
    assert_eq!(
        call("json::is_valid", vec![Value::String("1e400".into())]),
        Value::Bool(false),
        "the legacy validator must retain its finite-number contract when the dynamic parser enables arbitrary precision"
    );
    assert_eq!(
        call("json::stringify_map", vec![labels.clone()]).render(),
        "Result.Ok({\"name\":\"aura\"})"
    );
    assert!(call(
        "json::parse_string_map",
        vec![Value::String("{\"name\":\"aura\"}".into())]
    )
    .render()
    .starts_with("Result.Ok("));
    assert!(
        call("json::parse_string_map", vec![Value::String("[]".into())])
            .render()
            .starts_with("Result.Err(")
    );
    assert_eq!(
        call(
            "toml::is_valid",
            vec![Value::String("name = \"aura\"".into())]
        ),
        Value::Bool(true)
    );
    assert_eq!(
        call("toml::is_valid", vec![Value::String("name =".into())]),
        Value::Bool(false)
    );
    assert!(call("toml::stringify_map", vec![labels.clone()])
        .render()
        .starts_with("Result.Ok("));
    assert!(call(
        "toml::parse_string_map",
        vec![Value::String("name = \"aura\"".into())]
    )
    .render()
    .starts_with("Result.Ok("));
    assert!(call(
        "toml::parse_string_map",
        vec![Value::String("name = [1]".into())]
    )
    .render()
    .starts_with("Result.Err("));

    call("metrics::reset", vec![]);
    call(
        "metrics::increment",
        vec![
            Value::String("jobs".into()),
            Value::Int(IntegerValue::from_signed(2)),
        ],
    );
    assert_eq!(
        call("metrics::get", vec![Value::String("jobs".into())]),
        Value::Int(IntegerValue::from_signed(2))
    );
    assert_eq!(
        call("metrics::get", vec![Value::String("missing".into())]),
        Value::Int(IntegerValue::zero())
    );
    for level in ["debug", "info", "warn", "error"] {
        assert_eq!(
            call(
                &format!("log::{level}"),
                vec![Value::String("ready".into()), labels.clone()]
            ),
            Value::Unit
        );
    }
    assert_eq!(
        call(
            "trace::event",
            vec![Value::String("boot".into()), labels.clone()]
        ),
        Value::Unit
    );

    assert!(super::evaluate_host_builtin("sys::args", vec![Value::Unit]).is_err());
    assert!(super::evaluate_host_builtin("sys::env", vec![Value::Bool(true)]).is_err());
    assert!(super::evaluate_host_builtin("json::stringify_map", vec![Value::Unit]).is_err());
    assert!(super::evaluate_host_builtin(
        "metrics::increment",
        vec![Value::String("x".into()), Value::Unit]
    )
    .is_err());
    assert!(super::evaluate_host_builtin(
        "metrics::increment",
        vec![
            Value::String("x".into()),
            Value::Int(IntegerValue::from_literal(u128::MAX)),
        ]
    )
    .is_err());
    call("metrics::reset", vec![]);
    call(
        "metrics::increment",
        vec![
            Value::String("overflow".into()),
            Value::Int(IntegerValue::from_signed(i128::from(i64::MAX))),
        ],
    );
    assert!(super::evaluate_host_builtin(
        "metrics::increment",
        vec![
            Value::String("overflow".into()),
            Value::Int(IntegerValue::from_signed(1)),
        ]
    )
    .is_err());
    assert_eq!(
        call("metrics::get", vec![Value::String("overflow".into())]),
        Value::Int(IntegerValue::from_signed(i128::from(i64::MAX)))
    );
    assert!(super::evaluate_host_builtin(
        "metrics::increment",
        vec![
            Value::String("outside-int64".into()),
            Value::Int(IntegerValue::from_signed(i128::from(i64::MAX) + 1)),
        ]
    )
    .is_err());
    call(
        "metrics::increment",
        vec![
            Value::String("underflow".into()),
            Value::Int(IntegerValue::from_signed(i128::from(i64::MIN))),
        ],
    );
    assert!(super::evaluate_host_builtin(
        "metrics::increment",
        vec![
            Value::String("underflow".into()),
            Value::Int(IntegerValue::from_signed(-1)),
        ]
    )
    .is_err());
    assert_eq!(
        call("metrics::get", vec![Value::String("underflow".into())]),
        Value::Int(IntegerValue::from_signed(i128::from(i64::MIN)))
    );
    assert!(super::evaluate_host_builtin("missing::call", vec![]).is_err());
}

#[test]
fn bounded_channel_waits_for_capacity_before_accepting_another_value() {
    let channel = ChannelValue::with_capacity(1);
    channel
        .send(Value::Int(IntegerValue::from_signed(1)))
        .expect("first bounded send should succeed");

    let delayed_recv = channel.clone();
    let worker = thread::spawn(move || {
        thread::sleep(StdDuration::from_millis(80));
        delayed_recv.try_recv()
    });

    let start = Instant::now();
    channel
        .send(Value::Int(IntegerValue::from_signed(2)))
        .expect("second send should succeed after capacity frees");
    let elapsed = start.elapsed();
    let received = worker
        .join()
        .expect("bounded channel worker should join successfully");

    assert_eq!(
        received,
        TryRecvResult::Value(Value::Int(IntegerValue::from_signed(1)))
    );
    assert!(
        elapsed >= StdDuration::from_millis(60),
        "bounded send should wait for free capacity; elapsed {:?}",
        elapsed
    );
    assert_eq!(
        channel.try_recv(),
        TryRecvResult::Value(Value::Int(IntegerValue::from_signed(2)))
    );
}

#[test]
fn task_and_cancellation_helpers_cover_current_runtime_contract() {
    let task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(9)))
    }));
    assert_eq!(task.runtime_type_name(), None);
    task.set_runtime_type_name("Task[int32]".to_string());
    assert_eq!(task.runtime_type_name().as_deref(), Some("Task[int32]"));
    assert_eq!(
        task.clone().runtime_type_name().as_deref(),
        Some("Task[int32]"),
        "Task aliases must observe the same native-runtime type metadata"
    );
    assert_eq!(
        wait_task_ready(&task).expect("first wait should succeed"),
        Value::Int(IntegerValue::from_signed(9))
    );
    assert_eq!(
        wait_task_ready(&task).expect("cached wait should also succeed"),
        Value::Int(IntegerValue::from_signed(9))
    );

    let cancellation = CancellationContext::default();
    assert!(!cancellation.is_cancelled());
    let group = TaskGroupValue::new(&cancellation);
    let registered = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    group.register_task(registered.clone());
    assert_eq!(group.drain_tasks(), vec![registered]);
    group.cancel();
    assert!(group.child_cancellation().is_cancelled());

    let inactive_spawn = spawn_lightweight_task(|| Ok(Value::Unit))
        .expect_err("spawning outside a lightweight scheduler should fail");
    assert!(inactive_spawn
        .message
        .contains("requires an active task scheduler"));
    let inactive_cancellable_spawn =
        spawn_lightweight_task_with_cancellation(CancellationContext::default(), || {
            Ok(Value::Unit)
        })
        .expect_err("cancellable spawning outside a lightweight scheduler should fail");
    assert!(inactive_cancellable_spawn
        .message
        .contains("requires an active task scheduler"));

    let channel = ChannelValue::new();
    assert_eq!(channel.runtime_type_name(), None);
    channel.set_runtime_type_name("Queue[str]".to_string());
    assert_eq!(channel.runtime_type_name().as_deref(), Some("Queue[str]"));
    assert_eq!(
        channel.clone().runtime_type_name().as_deref(),
        Some("Queue[str]"),
        "Queue aliases must observe the same native-runtime type metadata"
    );
}

#[test]
fn task_group_wake_flags_cover_already_completed_and_duplicate_registrations() {
    let completed = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    assert_eq!(
        wait_task_ready(&completed).expect("completed task should be ready"),
        Value::Unit
    );

    let completion_flag = Arc::new(super::RuntimeWakeSignal::new(false));
    completed.register_group_completion_wake_flag(completion_flag.clone());
    assert!(completion_flag.load(Ordering::SeqCst));
    completion_flag.store(false, Ordering::SeqCst);
    completed.register_group_completion_wake_flag(completion_flag.clone());
    assert!(completion_flag.load(Ordering::SeqCst));

    let failed = TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("boom"))));
    for _ in 0..100 {
        if failed.completed_result().is_some() {
            break;
        }
        thread::sleep(StdDuration::from_millis(1));
    }
    assert!(failed.unobserved_error().is_some());

    let failure_flag = Arc::new(super::RuntimeWakeSignal::new(false));
    failed.register_group_failure_wake_flag(failure_flag.clone());
    assert!(failure_flag.load(Ordering::SeqCst));
    failure_flag.store(false, Ordering::SeqCst);
    failed.register_group_failure_wake_flag(failure_flag.clone());
    assert!(failure_flag.load(Ordering::SeqCst));

    let idle_group = TaskGroupValue::new(&CancellationContext::default());
    idle_group
        .inner
        .failure_wake_flag
        .store(true, Ordering::SeqCst);
    idle_group.clear_failure_wake_if_no_unobserved_error();
    assert!(!idle_group.inner.failure_wake_flag.load(Ordering::SeqCst));

    let failed_group = TaskGroupValue::new(&CancellationContext::default());
    failed_group.register_task(failed);
    assert!(failed_group.inner.failure_wake_flag.load(Ordering::SeqCst));
    failed_group.clear_failure_wake_if_no_unobserved_error();
    assert!(failed_group.inner.failure_wake_flag.load(Ordering::SeqCst));

    let running_group = TaskGroupValue::new(&CancellationContext::default());
    let blocker = ChannelValue::new();
    let release = blocker.clone();
    let running = TaskValue::from_handle(thread::spawn(move || {
        let _ = blocker.recv_with_cancellation(None, None);
        Ok(Value::Unit)
    }));
    running_group.register_task(running.clone());
    running_group
        .inner
        .completion_wake_flag
        .store(true, Ordering::SeqCst);
    running_group.clear_completion_wake_if_tasks_still_running();
    assert!(!running_group
        .inner
        .completion_wake_flag
        .load(Ordering::SeqCst));

    release.close();
    assert_eq!(
        wait_task_ready(&running).expect("released task should complete"),
        Value::Unit
    );
}

#[test]
fn task_execution_finalization_maps_failures_to_task_results() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let direct_failure = finalize_task_execution(|| -> std::result::Result<Value, Diagnostic> {
        std::panic::panic_any(LightweightTaskFailureSignal(Diagnostic::new(
            "direct failure",
        )));
    });
    let owned_panic = finalize_task_execution(|| -> std::result::Result<Value, Diagnostic> {
        std::panic::panic_any("owned panic".to_string());
    });
    let static_panic = finalize_task_execution(|| -> std::result::Result<Value, Diagnostic> {
        std::panic::panic_any("static panic");
    });
    let opaque_panic = finalize_task_execution(|| -> std::result::Result<Value, Diagnostic> {
        std::panic::panic_any(17usize);
    });

    std::panic::set_hook(previous_hook);

    match direct_failure {
        TaskExecutionResult::Ready(Err(error)) => assert_eq!(error.message, "direct failure"),
        other => panic!("expected direct failure diagnostic, got {other:?}"),
    }
    match owned_panic {
        TaskExecutionResult::Ready(Err(error)) => assert!(error.message.contains("owned panic")),
        other => panic!("expected owned panic diagnostic, got {other:?}"),
    }
    match static_panic {
        TaskExecutionResult::Ready(Err(error)) => assert!(error.message.contains("static panic")),
        other => panic!("expected static panic diagnostic, got {other:?}"),
    }
    match opaque_panic {
        TaskExecutionResult::Ready(Err(error)) => assert!(error.message.contains("non-string")),
        other => panic!("expected opaque panic diagnostic, got {other:?}"),
    }

    let cancelled_root = run_lightweight_root_task(|| -> std::result::Result<Value, Diagnostic> {
        std::panic::panic_any(TaskCancelledSignal);
    })
    .expect_err("cancelled root task should surface as a diagnostic");
    assert!(cancelled_root
        .message
        .contains("root Aura task was cancelled"));
}

#[test]
fn p63_retry_cancellation_propagates_as_task_cancellation_not_a_panic_error() {
    super::propagate_retry_task_cancellation();

    let entered = Arc::new(AtomicBool::new(false));
    let entered_probe = entered.clone();
    let result = run_lightweight_root_task(move || {
        let cancellation_signal = Arc::new(super::RuntimeWakeSignal::new(false));
        let cancellation = CancellationContext {
            flags: vec![cancellation_signal.clone()],
        };
        let task = spawn_lightweight_task_with_cancellation(cancellation, move || {
            entered_probe.store(true, Ordering::SeqCst);
            cancellation_signal.store(true, Ordering::SeqCst);
            super::propagate_retry_task_cancellation();
            Ok(Value::Unit)
        })?;

        let status = task
            .wait_result_with_cancellation(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        Ok(Value::Bool(matches!(status, TaskWaitStatus::Cancelled)))
    })
    .expect("the root scheduler should observe a cancelled child without failing");

    assert!(entered.load(Ordering::SeqCst));
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn p63_host_task_join_handoff_normalizes_a_panicked_thread_to_a_diagnostic() {
    let task = TaskValue::from_handle(thread::spawn(
        || -> std::result::Result<Value, Diagnostic> {
            std::panic::panic_any(37usize);
        },
    ));

    let status = task
        .wait_result_with_cancellation(Some(StdDuration::from_secs(1)), None)
        .expect("joining a panicked host task should produce a ready diagnostic");
    let TaskWaitStatus::Ready(Err(error)) = status else {
        panic!("a panicked host task must complete with a diagnostic");
    };
    assert_eq!(error.message, "spawned task panicked");
}

#[test]
fn lightweight_worker_count_defaults_and_rejects_invalid_overrides() {
    assert_eq!(super::decode_lightweight_worker_count(None, 7).unwrap(), 7);
    assert_eq!(
        super::decode_lightweight_worker_count(Some(OsStr::new("1")), 7).unwrap(),
        1
    );
    assert_eq!(
        super::decode_lightweight_worker_count(Some(OsStr::new("16")), 7).unwrap(),
        16
    );

    for invalid in ["", "0", "-1", "+2", " 2", "2 ", "two"] {
        let error = super::decode_lightweight_worker_count(Some(OsStr::new(invalid)), 7)
            .expect_err("invalid worker overrides must be diagnosed before execution");
        assert_eq!(error.code, "AU4006");
        assert_eq!(
            error.message,
            format!("invalid AURA_WORKERS value `{invalid}`: expected a positive integer")
        );
    }

    let overflow = format!("{}0", usize::MAX);
    let error = super::decode_lightweight_worker_count(Some(OsStr::new(&overflow)), 7)
        .expect_err("an overflowing worker count must be rejected");
    assert_eq!(error.code, "AU4006");
    assert_eq!(
        error.message,
        format!("invalid AURA_WORKERS value `{overflow}`: expected a positive integer")
    );

    let error = super::decode_lightweight_worker_count(None, 0)
        .expect_err("an impossible zero-core host must not create an empty runtime");
    assert_eq!(error.code, "AU4006");
}

#[test]
fn lightweight_worker_runner_rejects_an_empty_pool_before_starting_work() {
    let entry_ran = Arc::new(AtomicBool::new(false));
    let entry_probe = entry_ran.clone();
    let error = super::run_lightweight_root_task_with_worker_count(0, move || {
        entry_probe.store(true, Ordering::SeqCst);
        Ok(Value::Unit)
    })
    .expect_err("an empty worker pool must be diagnosed");

    assert_eq!(error.code, "AU4006");
    assert_eq!(error.message, "Aura runtime requires at least one worker");
    assert!(
        !entry_ran.load(Ordering::SeqCst),
        "invalid worker configuration must fail before the root entry runs"
    );
}

#[test]
fn worker_coordinator_preserves_round_robin_admission_and_shutdown_accounting() {
    let workers = super::LightweightWorkerCoordinator::new(2);
    let first_reactor = RuntimeReactor::new().expect("first worker reactor should initialize");
    let second_reactor = RuntimeReactor::new().expect("second worker reactor should initialize");
    workers.register_reactor(0, first_reactor.handle());
    workers.register_reactor(1, second_reactor.handle());

    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let cleanup_probe = cleanup_count.clone();
        let (task, request) = super::prepare_lightweight_task(
            None,
            None,
            true,
            Box::new(|| Ok(Value::Unit)),
            Some(Box::new(move || {
                cleanup_probe.fetch_add(1, Ordering::SeqCst);
            })),
        )
        .expect("coordinator admission task should prepare");
        workers.submit(request);
        tasks.push(task);
    }

    assert_eq!(workers.pending_requests.load(Ordering::SeqCst), 3);
    assert_eq!(workers.next_task_id(), 1);
    assert_eq!(workers.next_task_id(), 2);
    let requests = [
        workers
            .take_request(0)
            .expect("the first request belongs to worker zero"),
        workers
            .take_request(1)
            .expect("the second request belongs to worker one"),
        workers
            .take_request(0)
            .expect("round-robin admission returns to worker zero"),
    ];
    assert!(
        workers.take_request(1).is_none(),
        "worker one must not receive a fourth request"
    );
    assert_eq!(workers.pending_requests.load(Ordering::SeqCst), 0);
    for request in requests {
        assert!(
            super::cancel_unadmitted_lightweight_task(request).is_none(),
            "ordinary cancellation cleanup must not report a panic"
        );
    }
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 3);
    assert!(tasks.iter().all(|task| matches!(
        task.completed_result(),
        Some(TaskExecutionResult::Cancelled)
    )));

    workers.mark_cleanup_complete(0);
    workers.mark_cleanup_complete(0);
    assert!(
        !workers.cleanup_is_globally_complete(),
        "one worker reporting twice must not complete global cleanup"
    );
    workers.mark_cleanup_complete(1);
    assert!(workers.cleanup_is_globally_complete());

    workers.fail(Diagnostic::new("first worker failure"));
    workers.fail(Diagnostic::new("later worker failure"));
    assert_eq!(
        workers
            .fatal_diagnostic()
            .expect("the coordinator must retain its first fatal error")
            .message,
        "first worker failure"
    );
    assert!(workers.shutdown.load(Ordering::SeqCst));

    let mut late_reactor =
        RuntimeReactor::new().expect("late worker reactor should still initialize");
    workers.register_reactor(0, late_reactor.handle());
    let started = Instant::now();
    assert!(late_reactor
        .poll(Some(crate::hosted_ci_timing_limit(StdDuration::from_secs(
            1
        ))))
        .expect("shutdown wake should be observable")
        .is_empty());
    assert!(
        started.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(250)),
        "a worker registered during shutdown must be woken immediately"
    );
}

#[test]
fn unadmitted_cleanup_panics_are_diagnosed_after_terminalizing_the_task() {
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let cleanup_probe = cleanup_count.clone();
    let (task, request) = super::prepare_lightweight_task(
        None,
        None,
        true,
        Box::new(|| Ok(Value::Unit)),
        Some(Box::new(move || {
            cleanup_probe.fetch_add(1, Ordering::SeqCst);
            panic!("cleanup contract violation");
        })),
    )
    .expect("cleanup-containment task should prepare");

    let diagnostic = super::cancel_unadmitted_lightweight_task(request)
        .expect("a cleanup panic must become a runtime diagnostic");
    assert!(diagnostic
        .message
        .contains("Aura task cleanup panicked: cleanup contract violation"));
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
    assert!(
        matches!(
            task.completed_result(),
            Some(TaskExecutionResult::Cancelled)
        ),
        "cleanup failure must not leave a durable task handle Running"
    );
}

#[test]
fn worker_infrastructure_failures_cancel_root_cleanup_once_and_return_diagnostics() {
    let cases = [
        (
            super::LightweightWorkerFaults {
                reactor_initialization_at: Some(0),
                ..Default::default()
            },
            "failed to initialize Aura worker 0 reactor",
            true,
            true,
        ),
        (
            super::LightweightWorkerFaults {
                worker_panic_at: Some(0),
                ..Default::default()
            },
            "internal error: Aura worker 0 panicked: injected Aura worker panic",
            false,
            true,
        ),
        (
            super::LightweightWorkerFaults {
                thread_spawn_at: Some(1),
                ..Default::default()
            },
            "failed to start Aura worker 1",
            false,
            false,
        ),
    ];

    for (faults, expected, cleanup_panics, entry_must_not_run) in cases {
        let entry_ran = Arc::new(AtomicBool::new(false));
        let entry_probe = entry_ran.clone();
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_probe = cleanup_count.clone();
        let suspend_started_entry = !entry_must_not_run;
        let error = super::run_lightweight_root_task_on_workers_with_faults(
            2,
            Box::new(move || {
                entry_probe.store(true, Ordering::SeqCst);
                if suspend_started_entry {
                    let _ = super::yield_current_lightweight_wait(super::TaskWaitRegistration {
                        recv_channels: Vec::new(),
                        ignore_closed_recv_channels: false,
                        send_channels: Vec::new(),
                        task_waits: Vec::new(),
                        deadline: None,
                        cancellation: None,
                        fd_wait: None,
                    });
                }
                Ok(Value::Unit)
            }),
            Some(Box::new(move || {
                cleanup_probe.fetch_add(1, Ordering::SeqCst);
                if cleanup_panics {
                    panic!("cleanup panic during worker failure recovery");
                }
            })),
            faults,
        )
        .expect_err("the injected worker infrastructure failure must reach the caller");

        assert!(
            error.message.contains(expected),
            "unexpected injected failure diagnostic: {error:?}"
        );
        if entry_must_not_run {
            assert!(
                !entry_ran.load(Ordering::SeqCst),
                "worker failure before admission must not run the root entry"
            );
        }
        assert_eq!(
            cleanup_count.load(Ordering::SeqCst),
            1,
            "failed admission must run root cleanup exactly once"
        );
    }
}

#[test]
fn p63_single_worker_infrastructure_failure_cancels_unadmitted_root_exactly_once() {
    let cases = [
        (
            super::LightweightWorkerFaults {
                reactor_initialization_at: Some(0),
                ..Default::default()
            },
            "failed to initialize Aura worker 0 reactor",
        ),
        (
            super::LightweightWorkerFaults {
                worker_panic_at: Some(0),
                ..Default::default()
            },
            "internal error: Aura worker 0 panicked: injected Aura worker panic",
        ),
        (
            super::LightweightWorkerFaults {
                thread_spawn_at: Some(0),
                ..Default::default()
            },
            "failed to start Aura worker 0: injected Aura worker thread spawn failure",
        ),
    ];

    for (faults, expected_diagnostic) in cases {
        let entry_ran = Arc::new(AtomicBool::new(false));
        let entry_probe = entry_ran.clone();
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_probe = cleanup_count.clone();

        let error = super::run_lightweight_root_task_on_workers_with_faults(
            1,
            Box::new(move || {
                entry_probe.store(true, Ordering::SeqCst);
                Ok(Value::Unit)
            }),
            Some(Box::new(move || {
                cleanup_probe.fetch_add(1, Ordering::SeqCst);
            })),
            faults,
        )
        .expect_err("an injected worker failure must reach the root caller");

        assert!(
            error.message.contains(expected_diagnostic),
            "unexpected injected worker failure diagnostic: {error:?}"
        );
        assert!(
            !entry_ran.load(Ordering::SeqCst),
            "a root task whose only worker failed before admission must never run"
        );
        assert_eq!(
            cleanup_count.load(Ordering::SeqCst),
            1,
            "failed root admission must run forced-exit cleanup exactly once"
        );
    }
}

#[test]
fn lightweight_tasks_are_pinned_across_yield_timer_and_queue_waits() {
    let result = super::run_lightweight_root_task_with_worker_count(3, || {
        let channel = ChannelValue::new();
        let receiver_channel = channel.clone();
        let receiver = spawn_lightweight_task(move || {
            let before = super::current_lightweight_worker_index()
                .expect("a running task must know its pinned worker");
            super::yield_now_with_runtime_scheduler();
            let after_yield = super::current_lightweight_worker_index()
                .expect("yield must resume on the same worker");
            let value = receiver_channel
                .recv_with_cancellation(Some(StdDuration::from_secs(1)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
                .expect("the sender should provide a queue item");
            let after_queue = super::current_lightweight_worker_index()
                .expect("queue wake must resume on the same worker");
            let _ = sleep_with_runtime_scheduler(StdDuration::from_millis(2), None);
            let after_timer = super::current_lightweight_worker_index()
                .expect("timer wake must resume on the same worker");
            Ok(Value::Tuple(TupleValue {
                element_types: vec![Type::named("int64"); 5],
                elements: vec![
                    Value::Int(IntegerValue::from_signed(before as i128)),
                    Value::Int(IntegerValue::from_signed(after_yield as i128)),
                    value,
                    Value::Int(IntegerValue::from_signed(after_queue as i128)),
                    Value::Int(IntegerValue::from_signed(after_timer as i128)),
                ],
            }))
        })?;
        let sender = spawn_lightweight_task(move || {
            channel
                .send(Value::Int(IntegerValue::from_signed(29)))
                .map_err(|_| Diagnostic::new("test queue unexpectedly rejected its item"))?;
            Ok(Value::Unit)
        })?;
        let receiver_value = wait_task_ready(&receiver)?;
        let _ = wait_task_ready(&sender)?;
        Ok(receiver_value)
    })
    .expect("the pinned-worker run should complete");

    let Value::Tuple(values) = result else {
        panic!("expected affinity tuple, got {result:?}");
    };
    let worker_ids = [
        &values.elements[0],
        &values.elements[1],
        &values.elements[3],
        &values.elements[4],
    ]
    .into_iter()
    .map(|value| match value {
        Value::Int(value) => value.as_i128().expect("worker index must be signed"),
        other => panic!("expected worker index, got {other:?}"),
    })
    .collect::<Vec<_>>();
    assert!(
        worker_ids.iter().all(|worker| *worker == worker_ids[0]),
        "one coroutine migrated across workers: {worker_ids:?}"
    );
    assert_eq!(
        values.elements[2],
        Value::Int(IntegerValue::from_signed(29))
    );
}

#[test]
fn lightweight_workers_make_cpu_progress_concurrently() {
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Barrier::new(3));
    let result = super::run_lightweight_root_task_with_worker_count(3, {
        let active = active.clone();
        let maximum = maximum.clone();
        let release = release.clone();
        move || {
            let mut tasks = Vec::new();
            for _ in 0..2 {
                let active = active.clone();
                let maximum = maximum.clone();
                let release = release.clone();
                tasks.push(spawn_lightweight_task(move || {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    release.wait();
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(Value::Unit)
                })?);
            }
            release.wait();
            for task in tasks {
                let _ = wait_task_ready(&task)?;
            }
            Ok(Value::Unit)
        }
    });

    assert_eq!(result.unwrap(), Value::Unit);
    assert_eq!(
        maximum.load(Ordering::SeqCst),
        2,
        "two pinned workers must execute CPU-bound task bodies at the same time"
    );
}

#[test]
fn cross_worker_task_completion_error_cancellation_and_claim_races_are_atomic() {
    let race = Arc::new(Barrier::new(3));
    let result = super::run_lightweight_root_task_with_worker_count(4, {
        let race = race.clone();
        move || {
            let owned = super::spawn_lightweight_task_with_result_repeatability(false, || {
                Ok(Value::String("owned".to_string()))
            })?;
            assert_eq!(wait_task_ready(&owned)?, Value::String("owned".to_string()));

            let first_task = owned.clone();
            let first_race = race.clone();
            let first = spawn_lightweight_task(move || {
                first_race.wait();
                Ok(Value::Bool(first_task.claim_result_observation().is_ok()))
            })?;
            let second_task = owned.clone();
            let second_race = race.clone();
            let second = spawn_lightweight_task(move || {
                second_race.wait();
                Ok(Value::Bool(second_task.claim_result_observation().is_ok()))
            })?;
            race.wait();
            let claims = [wait_task_ready(&first)?, wait_task_ready(&second)?];
            assert_eq!(
                claims
                    .iter()
                    .filter(|value| **value == Value::Bool(true))
                    .count(),
                1,
                "exactly one worker may claim a non-repeatable task result"
            );

            let failed = spawn_lightweight_task(|| Err(Diagnostic::new("worker failure")))?;
            let failure = wait_task_ready(&failed)
                .expect_err("a cross-worker task error must wake and reach its observer");
            assert_eq!(failure.message, "worker failure");

            let cancelled = spawn_lightweight_task(|| {
                cancel_current_lightweight_task_boundary();
            })?;
            match cancelled
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Cancelled => {}
                other => panic!("expected cross-worker cancellation, got {other:?}"),
            }
            Ok(Value::Unit)
        }
    });
    assert_eq!(result.unwrap(), Value::Unit);
}

#[test]
fn task_group_and_queue_registration_precede_remote_task_submission() {
    let registered = Arc::new(AtomicBool::new(false));
    let result = super::run_lightweight_root_task_with_worker_count(2, {
        let registered = registered.clone();
        move || {
            let group = TaskGroupValue::new(&CancellationContext::default());
            let queue = ChannelValue::new();
            let entry_registered = registered.clone();
            let registration_flag = registered.clone();
            let registration_group = group.clone();
            let registration_queue = queue.clone();
            let task = super::spawn_lightweight_task_with_result_repeatability_registered(
                true,
                move || {
                    assert!(
                        entry_registered.load(Ordering::SeqCst),
                        "remote execution must not begin before group and queue publication"
                    );
                    Ok(Value::Unit)
                },
                move |task| {
                    registration_group.register_task(task.clone());
                    registration_queue.register_task_handle(task);
                    registration_flag.store(true, Ordering::SeqCst);
                },
            )?;

            assert!(
                registered.load(Ordering::SeqCst),
                "the spawn API must return only after registration"
            );
            assert_eq!(group.drain_tasks(), vec![task.clone()]);
            assert_eq!(queue.registered_task_handles(), vec![task.clone()]);
            wait_task_ready(&task)
        }
    });
    assert_eq!(result.unwrap(), Value::Unit);
}

#[test]
fn direct_cleanup_runs_once_on_the_task_pinned_worker() {
    let entry_worker = Arc::new(AtomicUsize::new(usize::MAX));
    let cleanup_worker = Arc::new(AtomicUsize::new(usize::MAX));
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let result = super::run_lightweight_root_task_with_worker_count(3, {
        let entry_worker = entry_worker.clone();
        let cleanup_worker = cleanup_worker.clone();
        let cleanup_count = cleanup_count.clone();
        move || {
            let entry_probe = entry_worker.clone();
            let cleanup_probe = cleanup_worker.clone();
            let count_probe = cleanup_count.clone();
            let task = unsafe {
                spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup(
                    CancellationContext::default(),
                    move || {
                        entry_probe.store(
                            super::current_lightweight_worker_index()
                                .expect("direct task entry must run on a worker"),
                            Ordering::SeqCst,
                        );
                        super::exit_current_lightweight_task(TaskExecutionResult::Cancelled);
                    },
                    move || {
                        cleanup_probe.store(
                            super::current_lightweight_worker_index()
                                .expect("direct cleanup must retain its task context"),
                            Ordering::SeqCst,
                        );
                        count_probe.fetch_add(1, Ordering::SeqCst);
                    },
                )?
            };
            match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Cancelled => Ok(Value::Unit),
                other => panic!("expected force-cleaned task cancellation, got {other:?}"),
            }
        }
    });

    assert_eq!(result.unwrap(), Value::Unit);
    assert_ne!(entry_worker.load(Ordering::SeqCst), usize::MAX);
    assert_eq!(
        cleanup_worker.load(Ordering::SeqCst),
        entry_worker.load(Ordering::SeqCst),
        "a generated task's external cleanup must run on its pinned worker"
    );
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_drains_spawn_races_without_leaving_running_handles() {
    let release = Arc::new(Barrier::new(2));
    let raced_task = Arc::new(Mutex::new(None::<TaskValue>));
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let result = super::run_lightweight_root_task_with_worker_count(3, {
        let release = release.clone();
        let raced_task = raced_task.clone();
        let cleanup_count = cleanup_count.clone();
        move || {
            let child_release = release.clone();
            let child_task = raced_task.clone();
            let child_cleanup = cleanup_count.clone();
            let _spawner = spawn_lightweight_task(move || {
                child_release.wait();
                let task = unsafe {
                    spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup(
                        CancellationContext::default(),
                        || {
                            let _ = super::yield_current_lightweight_wait(
                                super::TaskWaitRegistration {
                                    recv_channels: Vec::new(),
                                    ignore_closed_recv_channels: false,
                                    send_channels: Vec::new(),
                                    task_waits: Vec::new(),
                                    deadline: None,
                                    cancellation: None,
                                    fd_wait: None,
                                },
                            );
                            Ok(Value::Unit)
                        },
                        move || {
                            child_cleanup.fetch_add(1, Ordering::SeqCst);
                        },
                    )?
                };
                *lock_mutex(&child_task) = Some(task);
                Ok(Value::Unit)
            })?;
            release.wait();
            Ok(Value::Unit)
        }
    });
    assert_eq!(result.unwrap(), Value::Unit);

    let task = lock_mutex(&raced_task)
        .clone()
        .expect("the racing spawn must publish its durable task handle");
    assert!(
        matches!(
            task.completed_result(),
            Some(TaskExecutionResult::Cancelled | TaskExecutionResult::Ready(_))
        ),
        "shutdown must not abandon an admitted or inbox-resident task as Running"
    );
    assert_eq!(
        cleanup_count.load(Ordering::SeqCst),
        1,
        "the shutdown race must run generated cleanup exactly once"
    );
}

#[test]
fn task_result_observation_claim_is_shared_by_aliases_and_repeatable_when_allowed() {
    let repeatable = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::Bool(true))),
        true,
    );
    repeatable
        .claim_result_observation()
        .expect("repeatable result should allow its first observation");
    repeatable
        .claim_result_observation()
        .expect("repeatable result should allow repeated observation");

    let single_consumer = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("owned".to_string()))),
        false,
    );
    let alias = single_consumer.clone();
    single_consumer
        .claim_result_observation()
        .expect("single-consumer result should allow its first observation");
    let error = alias
        .claim_result_observation()
        .expect_err("an alias must share the consumed observation right");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "task result has already been observed; non-repeatable task results allow exactly one observing attempt"
    );
}

#[test]
fn task_result_observation_claim_has_exactly_one_race_winner() {
    let task = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("owned".to_string()))),
        false,
    );
    let barrier = Arc::new(Barrier::new(3));
    let observers = (0..2)
        .map(|_| {
            let task = task.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                task.claim_result_observation()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = observers
        .into_iter()
        .map(|observer| observer.join().expect("observer should not panic"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    let loser = outcomes
        .into_iter()
        .find_map(std::result::Result::err)
        .expect("one competing observer should lose");
    assert_eq!(loser.code, "AU4001");
}

#[test]
fn task_result_batch_claim_rejects_duplicate_aliases_and_cleanup_does_not_consume() {
    let task = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("owned".to_string()))),
        false,
    );
    let alias = task.clone();
    assert_eq!(
        wait_task_ready(&task).expect("task should complete before cleanup probe"),
        Value::String("owned".to_string())
    );
    let cancellation = CancellationContext::default();
    assert!(!task_group_cleanup_should_cancel(
        std::slice::from_ref(&task),
        &cancellation
    ));

    let error = claim_task_result_observations(&[task.clone(), alias])
        .expect_err("one helper must not deliver a non-repeatable result twice");
    assert_eq!(error.code, "AU4001");
    let error = claim_task_result_observations(&[task])
        .expect_err("the failed duplicate attempt must still consume the observation right");
    assert_eq!(error.code, "AU4001");
}

#[test]
fn phase58_select_uses_cancellation_first_then_original_source_index() {
    let first = ChannelValue::new();
    let second = ChannelValue::new();
    assert_eq!(
        first.try_send(Value::String("first".to_string())),
        super::TrySendResult::Sent
    );
    assert_eq!(
        second.try_send(Value::String("second".to_string())),
        super::TrySendResult::Sent
    );

    let selected = select_runtime_values(
        vec![
            Value::Channel(first.clone()),
            Value::Duration(0),
            Value::Channel(second.clone()),
        ],
        None,
    )
    .expect("the lowest ready source should win");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(first))"
    );
    assert_eq!(first.try_recv(), TryRecvResult::Empty);
    assert_eq!(
        second.try_recv(),
        TryRecvResult::Value(Value::String("second".to_string())),
        "a losing queue must remain unchanged"
    );

    let duplicate = ChannelValue::new();
    assert_eq!(
        duplicate.try_send(Value::String("once".to_string())),
        super::TrySendResult::Sent
    );
    let selected = select_runtime_values(
        vec![
            Value::Channel(duplicate.clone()),
            Value::Channel(duplicate.clone()),
        ],
        None,
    )
    .expect("duplicate queue sources compete independently");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(once))"
    );
    assert_eq!(
        duplicate.try_recv(),
        TryRecvResult::Empty,
        "one selected duplicate must remove exactly one item"
    );

    let cancelled_queue = ChannelValue::new();
    assert_eq!(
        cancelled_queue.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();
    let selected = select_runtime_values(
        vec![Value::Channel(cancelled_queue.clone()), Value::Duration(0)],
        Some(&cancellation),
    )
    .expect("current-task cancellation is an outcome");
    assert_eq!(selected.render(), "SelectOutcome.Cancelled");
    assert_eq!(
        cancelled_queue.try_recv(),
        TryRecvResult::Value(Value::Unit),
        "cancellation must be decided before consuming a ready source"
    );
}

#[test]
fn phase58_select_preserves_queue_task_and_deadline_outcome_shapes() {
    let buffered_then_closed = ChannelValue::new();
    assert_eq!(
        buffered_then_closed.try_send(Value::String("buffered".to_string())),
        super::TrySendResult::Sent
    );
    buffered_then_closed.close();
    let selected = select_runtime_values(vec![Value::Channel(buffered_then_closed.clone())], None)
        .expect("a buffered item should precede a closed queue outcome");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(buffered))"
    );
    let selected = select_runtime_values(vec![Value::Channel(buffered_then_closed)], None)
        .expect("the drained closed queue should remain ready");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Closed)"
    );

    let closed = ChannelValue::new();
    closed.close();
    let selected = select_runtime_values(vec![Value::Channel(closed)], None)
        .expect("a closed queue should be ready");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Closed)"
    );

    let ready_task =
        TaskValue::from_handle(thread::spawn(|| Ok(Value::String("finished".to_string()))));
    assert_eq!(
        wait_task_ready(&ready_task).expect("the task should complete"),
        Value::String("finished".to_string())
    );
    let selected = select_runtime_values(
        vec![Value::Duration(1_000_000_000), Value::Task(ready_task)],
        None,
    )
    .expect("the completed task should win");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(1, TaskResult.Ready(finished))"
    );

    let error_task =
        TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("selected failure"))));
    let selected = select_runtime_values(vec![Value::Task(error_task)], None)
        .expect("a failed child is still a ready select source");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(0, TaskResult.Error(selected failure))"
    );

    let selected = super::run_lightweight_root_task_with_worker_count(2, || {
        let cancelled_task = spawn_lightweight_task(|| {
            cancel_current_lightweight_task_boundary();
        })?;
        select_runtime_values(
            vec![Value::Task(cancelled_task), Value::Duration(1_000_000_000)],
            None,
        )
    })
    .expect("a child cancelled on another worker should wake select");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(0, TaskResult.Cancelled)"
    );

    let selected = select_runtime_values(vec![Value::Duration(0), Value::Duration(0)], None)
        .expect("an immediate deadline should win");
    assert_eq!(selected.render(), "SelectOutcome.Deadline(0)");
}

#[test]
fn phase58_select_claims_nonrepeatable_tasks_before_waiting() {
    let ready_queue = ChannelValue::new();
    assert_eq!(
        ready_queue.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let losing_task = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("owned".to_string()))),
        false,
    );

    let selected = select_runtime_values(
        vec![
            Value::Channel(ready_queue),
            Value::Task(losing_task.clone()),
        ],
        None,
    )
    .expect("the ready queue should win");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item())"
    );
    let error = losing_task
        .claim_result_observation()
        .expect_err("a losing nonrepeatable task observation is abandoned");
    assert_eq!(error.code, "AU4001");

    let repeatable_queue = ChannelValue::new();
    assert_eq!(
        repeatable_queue.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let repeatable_task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::String("repeatable".to_string()))
    }));
    let selected = select_runtime_values(
        vec![
            Value::Channel(repeatable_queue),
            Value::Task(repeatable_task.clone()),
        ],
        None,
    )
    .expect("a repeatable task may lose without losing reuse");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item())"
    );
    assert_eq!(
        wait_task_ready(&repeatable_task).expect("a losing repeatable task remains observable"),
        Value::String("repeatable".to_string())
    );

    let selected_nonrepeatable = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("selected-owned".to_string()))),
        false,
    );
    let selected = select_runtime_values(
        vec![
            Value::Task(selected_nonrepeatable.clone()),
            Value::Duration(1_000_000_000),
        ],
        None,
    )
    .expect("a selected nonrepeatable task should deliver its one result");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(0, TaskResult.Ready(selected-owned))"
    );
    let error = selected_nonrepeatable
        .claim_result_observation()
        .expect_err("the selected nonrepeatable result is consumed");
    assert_eq!(error.code, "AU4001");

    let duplicate =
        TaskValue::from_handle_with_result_repeatability(thread::spawn(|| Ok(Value::Unit)), false);
    let error = select_runtime_values(
        vec![Value::Task(duplicate.clone()), Value::Task(duplicate)],
        None,
    )
    .expect_err("runtime duplicate defense must reject one observation right twice");
    assert_eq!(error.code, "AU4001");
}

#[test]
fn phase58_select_validates_every_source_and_deadline_before_observing_readiness() {
    let empty = select_runtime_values(Vec::new(), None)
        .expect_err("the runtime must defend the one-or-more source contract");
    assert_eq!(empty.code, "AU4001");

    let ready = ChannelValue::new();
    assert_eq!(ready.try_send(Value::Unit), super::TrySendResult::Sent);
    let invalid_source = select_runtime_values(
        vec![
            Value::Channel(ready.clone()),
            Value::String("not a select source".to_string()),
        ],
        None,
    )
    .expect_err("runtime descriptors are validated before a ready source is consumed");
    assert_eq!(invalid_source.code, "AU4001");
    assert_eq!(
        ready.try_recv(),
        TryRecvResult::Value(Value::Unit),
        "validation failure must not consume a source"
    );

    let negative = select_runtime_values(vec![Value::Duration(-1)], None)
        .expect_err("negative relative deadlines are invalid");
    assert_eq!(negative.code, "AU4001");
    assert!(negative.message.contains("non-negative"));

    let overflow = select_runtime_values(vec![Value::Duration(i128::MAX)], None)
        .expect_err("host deadline overflow is invalid");
    assert_eq!(overflow.code, "AU4001");
    assert!(
        overflow.message.contains("host timer range")
            || overflow.message.contains("host deadline range")
    );
}

#[test]
fn phase58_select_rejects_a_deadline_that_overflows_after_source_validation() {
    let mut accepted = 0_i128;
    let mut rejected = i128::MAX;
    while accepted + 1 < rejected {
        let candidate = accepted + (rejected - accepted) / 2;
        if super::duration_to_host_timer(candidate, "select deadline").is_ok() {
            accepted = candidate;
        } else {
            rejected = candidate;
        }
    }

    let validation_margin = StdDuration::from_millis(100);
    let duration = accepted
        .checked_sub(validation_margin.as_nanos() as i128)
        .expect("the host Instant range should exceed the validation margin");
    let hook_ran = Arc::new(AtomicBool::new(false));
    let hook_ran_inside = hook_ran.clone();
    install_after_select_source_validation_hook(move || {
        hook_ran_inside.store(true, Ordering::SeqCst);
        thread::sleep(StdDuration::from_millis(250));
    });

    let error = select_runtime_values(vec![Value::Duration(duration)], None)
        .expect_err("elapsed validation time must not wrap an absolute select deadline");
    assert!(
        hook_ran.load(Ordering::SeqCst),
        "the relative duration must pass validation before the absolute deadline overflows"
    );
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "select deadline exceeds the host deadline range"
    );
}

#[test]
fn phase58_select_captures_one_deadline_base_after_all_sources_validate() {
    let validation_finished_at = Arc::new(Mutex::new(None));
    let hook_timestamp = validation_finished_at.clone();
    install_after_select_source_validation_hook(move || {
        thread::sleep(StdDuration::from_millis(20));
        *lock_mutex(&hook_timestamp) = Some(Instant::now());
    });

    let selected = select_runtime_values(
        vec![
            Value::Duration(100_000_000),
            Value::Channel(ChannelValue::new()),
            Value::Duration(100_000_000),
        ],
        None,
    )
    .expect("validated relative deadlines should share a post-validation base");
    let validation_finished_at =
        lock_mutex(&validation_finished_at).expect("the validation hook must run");
    assert_eq!(selected.render(), "SelectOutcome.Deadline(0)");
    assert!(
        validation_finished_at.elapsed() >= StdDuration::from_millis(80),
        "deadline time must start after every source and duration has validated"
    );
}

#[test]
fn phase58_select_repeatable_task_winner_remains_reusable() {
    let task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::String("repeatable-winner".to_string()))
    }));
    assert_eq!(
        wait_task_ready(&task).expect("the repeatable task should complete"),
        Value::String("repeatable-winner".to_string())
    );

    for attempt in 0..2 {
        let selected = select_runtime_values(
            vec![Value::Task(task.clone()), Value::Duration(1_000_000_000)],
            None,
        )
        .expect("a repeatable completed task may win select repeatedly");
        assert_eq!(
            selected.render(),
            "SelectOutcome.Task(0, TaskResult.Ready(repeatable-winner))",
            "repeatable winner attempt {attempt} changed its result"
        );
    }
    assert_eq!(
        wait_task_ready(&task).expect("select must not consume a repeatable task result"),
        Value::String("repeatable-winner".to_string())
    );
}

#[test]
fn phase58_select_uses_original_index_across_queue_task_deadline_permutations() {
    fn ready_task() -> TaskValue {
        let task = TaskValue::from_handle(thread::spawn(|| {
            Ok(Value::String("ready-task".to_string()))
        }));
        assert_eq!(
            wait_task_ready(&task).expect("the task should be ready before arbitration"),
            Value::String("ready-task".to_string())
        );
        task
    }

    let queue_first = ChannelValue::new();
    assert_eq!(
        queue_first.try_send(Value::String("queue-first".to_string())),
        super::TrySendResult::Sent
    );
    let selected = select_runtime_values(
        vec![
            Value::Channel(queue_first.clone()),
            Value::Task(ready_task()),
            Value::Duration(0),
        ],
        None,
    )
    .expect("the lowest ready Queue should win");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(queue-first))"
    );
    assert_eq!(queue_first.try_recv(), TryRecvResult::Empty);

    let queue_last = ChannelValue::new();
    assert_eq!(
        queue_last.try_send(Value::String("queue-loser".to_string())),
        super::TrySendResult::Sent
    );
    let task_first = ready_task();
    let selected = select_runtime_values(
        vec![
            Value::Task(task_first.clone()),
            Value::Duration(0),
            Value::Channel(queue_last.clone()),
        ],
        None,
    )
    .expect("the lowest ready Task should win");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(0, TaskResult.Ready(ready-task))"
    );
    assert_eq!(
        queue_last.try_recv(),
        TryRecvResult::Value(Value::String("queue-loser".to_string()))
    );
    assert_eq!(
        wait_task_ready(&task_first).expect("a repeatable winning Task remains reusable"),
        Value::String("ready-task".to_string())
    );

    let deadline_loser_queue = ChannelValue::new();
    assert_eq!(
        deadline_loser_queue.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let selected = select_runtime_values(
        vec![
            Value::Duration(0),
            Value::Channel(deadline_loser_queue.clone()),
            Value::Task(ready_task()),
        ],
        None,
    )
    .expect("the lowest ready Deadline should win");
    assert_eq!(selected.render(), "SelectOutcome.Deadline(0)");
    assert_eq!(
        deadline_loser_queue.try_recv(),
        TryRecvResult::Value(Value::Unit),
        "the losing queue must remain unchanged"
    );
}

#[test]
fn phase58_select_committed_queue_winner_is_not_replaced_by_later_cancellation() {
    let queue = ChannelValue::new();
    assert_eq!(
        queue.try_send(Value::String("committed".to_string())),
        super::TrySendResult::Sent
    );
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let hook_ran = Arc::new(AtomicBool::new(false));
    let hook_ran_inside = hook_ran.clone();
    install_after_select_queue_commit_hook(move || {
        hook_ran_inside.store(true, Ordering::SeqCst);
        group.cancel();
    });

    let selected = select_runtime_values(vec![Value::Channel(queue)], Some(&cancellation))
        .expect("cancellation after the atomic receive must not revoke the committed winner");
    assert!(hook_ran.load(Ordering::SeqCst));
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(committed))"
    );

    let closed_queue = ChannelValue::new();
    closed_queue.close();
    let closed_group = TaskGroupValue::new(&CancellationContext::default());
    let closed_cancellation = closed_group.child_cancellation();
    let closed_hook_ran = Arc::new(AtomicBool::new(false));
    let closed_hook_ran_inside = closed_hook_ran.clone();
    install_after_select_queue_commit_hook(move || {
        closed_hook_ran_inside.store(true, Ordering::SeqCst);
        closed_group.cancel();
    });

    let selected = select_runtime_values(
        vec![Value::Channel(closed_queue)],
        Some(&closed_cancellation),
    )
    .expect("cancellation after observing queue closure must not revoke the committed outcome");
    assert!(closed_hook_ran.load(Ordering::SeqCst));
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Closed)"
    );
}

#[test]
fn phase58_select_unwind_during_subscription_cleans_every_loser_registration() {
    let queue = ChannelValue::new();
    let task = TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(20));
        Ok(Value::Unit)
    }));
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let inspected_queue = queue.clone();
    let inspected_task = task.clone();
    let inspected_cancellation = cancellation.clone();

    let error = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(|| {
            panic!("injected select subscription unwind");
        });
        select_runtime_values(
            vec![
                Value::Channel(queue),
                Value::Task(task),
                Value::Duration(1_000_000_000),
            ],
            Some(&cancellation),
        )
    })
    .expect_err("the injected subscription unwind should fail the selecting task");
    assert!(error
        .message
        .contains("injected select subscription unwind"));
    assert!(
        lock_mutex(&inspected_queue.inner.recv_reactor_subscribers).is_empty(),
        "queue subscription must be rolled back during unwind"
    );
    assert!(
        lock_mutex(&inspected_task.inner.completion_reactor_subscribers).is_empty(),
        "task subscription must be rolled back during unwind"
    );
    assert!(
        inspected_cancellation
            .flags
            .iter()
            .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()),
        "cancellation subscription must be rolled back during unwind"
    );
    assert_eq!(
        inspected_queue.try_send(Value::String("late-after-unwind".to_string())),
        super::TrySendResult::Sent,
        "late publication after unwind must remain harmless"
    );
    assert_eq!(
        inspected_queue.try_recv(),
        TryRecvResult::Value(Value::String("late-after-unwind".to_string()))
    );
}

#[test]
fn phase58_select_check_subscribe_recheck_and_loser_cleanup_are_race_safe() {
    let winner = ChannelValue::new();
    let losing_queue = ChannelValue::new();
    let losing_task = TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(50));
        Ok(Value::Unit)
    }));
    let cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = cancellation_group.child_cancellation();
    let injected_winner = winner.clone();

    let selected = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(move || {
            assert_eq!(
                injected_winner.try_send(Value::String("published".to_string())),
                super::TrySendResult::Sent
            );
        });
        let outcome = select_runtime_values(
            vec![
                Value::Channel(winner),
                Value::Channel(losing_queue.clone()),
                Value::Task(losing_task.clone()),
                Value::Duration(1_000_000_000),
            ],
            Some(&cancellation),
        )?;
        assert!(
            lock_mutex(&losing_queue.inner.recv_reactor_subscribers).is_empty(),
            "the losing queue registration must be removed before select returns"
        );
        assert!(
            lock_mutex(&losing_task.inner.completion_reactor_subscribers).is_empty(),
            "the losing task registration must be removed before select returns"
        );
        assert!(
            cancellation
                .flags
                .iter()
                .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()),
            "the losing cancellation registration must be removed before select returns"
        );
        assert_eq!(
            losing_queue.try_send(Value::String("late".to_string())),
            super::TrySendResult::Sent,
            "a late losing notification should be harmless"
        );
        assert_eq!(
            losing_queue.try_recv(),
            TryRecvResult::Value(Value::String("late".to_string())),
            "a late loser wake must not consume the losing source"
        );
        Ok(outcome)
    })
    .expect("publication during registration must not be lost");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(published))"
    );
}

#[test]
fn phase58_select_concurrent_queue_and_task_publication_enqueues_waiter_once() {
    let queue = ChannelValue::new();
    let publication_barrier = Arc::new(Barrier::new(3));
    let task_barrier = publication_barrier.clone();
    let task = TaskValue::from_handle(thread::spawn(move || {
        task_barrier.wait();
        Ok(Value::String("task-ready".to_string()))
    }));
    let inspected_queue = queue.clone();
    let inspected_task = task.clone();
    let ready_enqueues = Arc::new(AtomicUsize::new(0));
    let observed_enqueues = ready_enqueues.clone();
    let select_returns = Arc::new(AtomicUsize::new(0));
    let observed_returns = select_returns.clone();

    let selected = run_lightweight_root_task(move || {
        super::install_next_task_wait_ready_enqueue_counter(ready_enqueues);
        let published_queue = queue.clone();
        let published_task = task.clone();
        super::install_after_task_wait_subscribe_hook(move || {
            assert_eq!(
                lock_mutex(&published_queue.inner.recv_reactor_subscribers).len(),
                1,
                "the Queue must be registered before concurrent publication"
            );
            assert_eq!(
                lock_mutex(&published_task.inner.completion_reactor_subscribers).len(),
                1,
                "the Task must be registered before concurrent publication"
            );

            let queue_barrier = publication_barrier.clone();
            let queue_publisher = thread::spawn(move || {
                queue_barrier.wait();
                assert_eq!(
                    published_queue.try_send(Value::String("queue-ready".to_string())),
                    super::TrySendResult::Sent
                );
            });
            publication_barrier.wait();
            queue_publisher
                .join()
                .expect("the concurrent Queue publisher should finish");
            while published_task.completed_result().is_none() {
                thread::yield_now();
            }
        });

        let outcome = select_runtime_values(
            vec![
                Value::Channel(queue),
                Value::Task(task),
                Value::Duration(1_000_000_000),
            ],
            None,
        )?;
        select_returns.fetch_add(1, Ordering::SeqCst);
        assert!(
            lock_mutex(&inspected_queue.inner.recv_reactor_subscribers).is_empty(),
            "the selected Queue registration must be retired"
        );
        assert!(
            lock_mutex(&inspected_task.inner.completion_reactor_subscribers).is_empty(),
            "the losing Task registration must be retired"
        );
        assert_eq!(
            wait_task_ready(&inspected_task).expect("the losing repeatable Task remains reusable"),
            Value::String("task-ready".to_string())
        );

        let sleep_started = Instant::now();
        let wake = sleep_with_runtime_scheduler(StdDuration::from_millis(20), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        assert_eq!(
            wake,
            super::RuntimeSchedulerWakeReason::TimedOut,
            "a duplicate select enqueue must not resume the task's next suspension"
        );
        assert!(
            sleep_started.elapsed() >= StdDuration::from_millis(15),
            "the next suspension must not be resumed by a stale select wake"
        );
        super::clear_task_wait_ready_enqueue_counter();
        Ok(outcome)
    })
    .expect("concurrent Queue and Task publication should produce one select winner");

    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(queue-ready))",
        "one arbitration pass must choose the lowest ready original index"
    );
    assert_eq!(
        observed_enqueues.load(Ordering::SeqCst),
        1,
        "both source notifications must coalesce into one waiter enqueue"
    );
    assert_eq!(
        observed_returns.load(Ordering::SeqCst),
        1,
        "the selecting coroutine must return from select exactly once"
    );
}

#[test]
fn phase58_select_rechecks_task_deadline_and_cancellation_registration_races() {
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = TaskValue::from_handle(thread::spawn(move || {
        release_rx
            .recv()
            .expect("the registration hook should release the task");
        Ok(Value::String("task-race".to_string()))
    }));
    let hook_task = task.clone();
    let task_loser_queue = ChannelValue::new();
    let task_cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let task_cancellation = task_cancellation_group.child_cancellation();
    let selected = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(move || {
            release_tx
                .send(())
                .expect("the pending task should be released");
            while hook_task.completed_result().is_none() {
                thread::yield_now();
            }
        });
        let outcome = select_runtime_values(
            vec![
                Value::Channel(task_loser_queue.clone()),
                Value::Task(task.clone()),
                Value::Duration(1_000_000_000),
            ],
            Some(&task_cancellation),
        )?;
        assert!(lock_mutex(&task_loser_queue.inner.recv_reactor_subscribers).is_empty());
        assert!(lock_mutex(&task.inner.completion_reactor_subscribers).is_empty());
        assert!(task_cancellation
            .flags
            .iter()
            .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()));
        Ok(outcome)
    })
    .expect("task completion between subscription and recheck must not be lost");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Task(1, TaskResult.Ready(task-race))"
    );

    let deadline_loser_queue = ChannelValue::new();
    let (deadline_release_tx, deadline_release_rx) = std::sync::mpsc::channel();
    let deadline_loser_task = TaskValue::from_handle(thread::spawn(move || {
        deadline_release_rx
            .recv()
            .expect("the deadline winner should release the losing task");
        Ok(Value::Unit)
    }));
    let deadline_cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let deadline_cancellation = deadline_cancellation_group.child_cancellation();
    let selected = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(|| {
            thread::sleep(StdDuration::from_millis(3));
        });
        let outcome = select_runtime_values(
            vec![
                Value::Channel(deadline_loser_queue.clone()),
                Value::Task(deadline_loser_task.clone()),
                Value::Duration(1_000_000),
            ],
            Some(&deadline_cancellation),
        )?;
        deadline_release_tx
            .send(())
            .expect("the selected deadline should release the losing task");
        assert!(lock_mutex(&deadline_loser_queue.inner.recv_reactor_subscribers).is_empty());
        assert!(lock_mutex(&deadline_loser_task.inner.completion_reactor_subscribers).is_empty());
        assert!(deadline_cancellation
            .flags
            .iter()
            .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()));
        assert_eq!(
            deadline_loser_queue.try_send(Value::Unit),
            super::TrySendResult::Sent,
            "a late queue notification after a deadline win is harmless"
        );
        Ok(outcome)
    })
    .expect("deadline expiry between subscription and recheck must not be lost");
    assert_eq!(selected.render(), "SelectOutcome.Deadline(2)");

    let cancelled_loser_queue = ChannelValue::new();
    let (cancelled_release_tx, cancelled_release_rx) = std::sync::mpsc::channel();
    let cancelled_loser_task = TaskValue::from_handle(thread::spawn(move || {
        cancelled_release_rx
            .recv()
            .expect("the cancellation winner should release the losing task");
        Ok(Value::Unit)
    }));
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let selected = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(move || group.cancel());
        let outcome = select_runtime_values(
            vec![
                Value::Channel(cancelled_loser_queue.clone()),
                Value::Task(cancelled_loser_task.clone()),
                Value::Duration(1_000_000_000),
            ],
            Some(&cancellation),
        )?;
        cancelled_release_tx
            .send(())
            .expect("the selected cancellation should release the losing task");
        assert!(lock_mutex(&cancelled_loser_queue.inner.recv_reactor_subscribers).is_empty());
        assert!(lock_mutex(&cancelled_loser_task.inner.completion_reactor_subscribers).is_empty());
        assert!(cancellation
            .flags
            .iter()
            .all(|flag| lock_mutex(&flag.reactor_subscribers).is_empty()));
        assert_eq!(
            cancelled_loser_queue.try_send(Value::Unit),
            super::TrySendResult::Sent,
            "a late queue notification after cancellation is harmless"
        );
        Ok(outcome)
    })
    .expect("cancellation between subscription and recheck must not be lost");
    assert_eq!(selected.render(), "SelectOutcome.Cancelled");
}

#[test]
fn phase58_select_rearms_when_a_queue_wake_loses_the_atomic_receive_race() {
    let queue = ChannelValue::new();
    let hook_queue = queue.clone();
    let selected = run_lightweight_root_task(move || {
        super::install_after_task_wait_subscribe_hook(move || {
            assert_eq!(
                hook_queue.try_send(Value::String("stolen".to_string())),
                super::TrySendResult::Sent
            );
            assert_eq!(
                hook_queue.try_recv(),
                TryRecvResult::Value(Value::String("stolen".to_string())),
                "an external consumer should be able to win before arbitration"
            );
            let final_queue = hook_queue.clone();
            thread::spawn(move || {
                thread::sleep(StdDuration::from_millis(3));
                assert_eq!(
                    final_queue.try_send(Value::String("final".to_string())),
                    super::TrySendResult::Sent
                );
            });
        });
        select_runtime_values(
            vec![Value::Channel(queue), Value::Duration(1_000_000_000)],
            None,
        )
    })
    .expect("a lost receive race should rearm the same composite wait");
    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(final))"
    );
}

#[test]
fn phase58_select_cross_worker_publication_wakes_the_pinned_waiter() {
    let queue = ChannelValue::new();
    let observed_queue = queue.clone();
    let selected = super::run_lightweight_root_task_with_worker_count(2, move || {
        let producer_queue = queue.clone();
        let _producer = spawn_lightweight_task(move || {
            assert_eq!(
                producer_queue.try_send(Value::String("cross-worker".to_string())),
                super::TrySendResult::Sent
            );
            Ok(Value::Unit)
        })?;
        select_runtime_values(
            vec![Value::Channel(queue), Value::Duration(1_000_000_000)],
            None,
        )
    })
    .expect("a different worker should wake the selecting task directly");

    assert_eq!(
        selected.render(),
        "SelectOutcome.Queue(0, QueueReceive.Item(cross-worker))"
    );
    assert_eq!(
        observed_queue.try_recv(),
        TryRecvResult::Empty,
        "one cross-worker publication must enqueue and deliver exactly one item"
    );
}

#[test]
fn lightweight_task_cancel_boundary_marks_child_cancelled() {
    let result = run_lightweight_root_task(|| {
        let task = spawn_lightweight_task(|| {
            cancel_current_lightweight_task_boundary();
        })?;
        match task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Cancelled => Ok(Value::Bool(true)),
            other => panic!("expected cancelled child task, got {other:?}"),
        }
    });

    assert_eq!(
        result.expect("root task should complete"),
        Value::Bool(true)
    );
}

#[test]
fn channel_and_task_helpers_tolerate_poisoned_locks() {
    let channel = ChannelValue::new();
    let poisoned_channel = channel.clone();
    let _ = thread::spawn(move || {
        let _guard = poisoned_channel
            .inner
            .state
            .lock()
            .expect("poison setup lock");
        panic!("poison channel lock");
    })
    .join();
    channel
        .send(Value::Int(IntegerValue::from_signed(11)))
        .expect("poisoned channel lock should recover");
    assert_eq!(
        channel.try_recv(),
        TryRecvResult::Value(Value::Int(IntegerValue::from_signed(11)))
    );
    channel.close();
    assert_eq!(
        channel
            .recv_with_cancellation(None, None)
            .expect("an omitted queue timeout cannot overflow"),
        None
    );

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let poisoned_group = group.clone();
    let _ = thread::spawn(move || {
        let _guard = poisoned_group
            .inner
            .tasks
            .lock()
            .expect("poison setup lock");
        panic!("poison task-group lock");
    })
    .join();
    let registered = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    group.register_task(registered.clone());
    assert_eq!(group.drain_tasks(), vec![registered]);

    let task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(17)))
    }));
    let poisoned_task = task.clone();
    let _ = thread::spawn(move || {
        let _guard = poisoned_task
            .inner
            .handle
            .lock()
            .expect("poison setup lock");
        panic!("poison task lock");
    })
    .join();
    assert_eq!(
        wait_task_ready(&task).expect("poisoned task handle lock should recover"),
        Value::Int(IntegerValue::from_signed(17))
    );
}

#[test]
fn condvar_helpers_tolerate_poisoned_wait_guards() {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let poisoned_pair = pair.clone();
    let _ = thread::spawn(move || {
        let (lock, _) = &*poisoned_pair;
        let _guard = lock.lock().expect("poison setup lock");
        panic!("poison condvar wait lock");
    })
    .join();

    let (lock, condvar) = &*pair;
    let mut guard = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let notify_pair = pair.clone();
    let notifier = thread::spawn(move || {
        let (lock, condvar) = &*notify_pair;
        let mut guard = match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = true;
        condvar.notify_all();
    });
    while !*guard {
        guard = wait_condvar(condvar, guard);
    }
    notifier.join().expect("notifier should finish");

    let timeout_pair = Arc::new((Mutex::new(false), Condvar::new()));
    let poisoned_timeout_pair = timeout_pair.clone();
    let _ = thread::spawn(move || {
        let (lock, _) = &*poisoned_timeout_pair;
        let _guard = lock.lock().expect("poison setup lock");
        panic!("poison condvar timeout lock");
    })
    .join();

    let (lock, condvar) = &*timeout_pair;
    let guard = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let timeout_notify_pair = timeout_pair.clone();
    let timeout_notifier = thread::spawn(move || {
        let (lock, condvar) = &*timeout_notify_pair;
        let mut guard = match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = true;
        condvar.notify_all();
    });
    let (guard, timed_out) = wait_timeout_condvar(condvar, guard, StdDuration::from_secs(1));
    assert!(!timed_out);
    assert!(*guard);
    timeout_notifier
        .join()
        .expect("timeout notifier should finish");
}

#[test]
fn non_unix_tls_listener_wait_timeout_blocks_when_no_handshakes_are_pending() {
    assert_eq!(
        non_unix_tls_listener_wait_timeout(true, None, None)
            .expect("idle non-Unix TLS wait should not fail"),
        None
    );
}

#[test]
fn non_unix_tls_listener_wait_timeout_uses_full_deadline_when_queue_is_empty() {
    let deadline = Instant::now() + StdDuration::from_millis(200);
    let wait = non_unix_tls_listener_wait_timeout(true, Some(deadline), None)
        .expect("deadline-based non-Unix TLS wait should not fail")
        .expect("deadline-based wait should produce a timeout");
    assert!(
        wait > StdDuration::from_millis(100),
        "idle wait should use the remaining deadline instead of a fixed slice, got {:?}",
        wait
    );
}

#[test]
fn non_unix_tls_listener_wait_timeout_keeps_short_slices_when_handshakes_are_pending() {
    let deadline = Instant::now() + StdDuration::from_millis(200);
    let wait = non_unix_tls_listener_wait_timeout(false, Some(deadline), None)
        .expect("pending-handshake non-Unix TLS wait should not fail")
        .expect("pending-handshake wait should produce a slice");
    assert!(
        wait <= StdDuration::from_millis(50),
        "pending handshakes should still advance on short slices, got {:?}",
        wait
    );
}

#[test]
fn runtime_scheduler_wakes_sleep_on_cancellation() {
    let parent = CancellationContext::default();
    let group = TaskGroupValue::new(&parent);
    let cancellation = group.child_cancellation();
    let start = Instant::now();
    let blocked_sleep = crate::hosted_ci_timing_limit(StdDuration::from_millis(250));
    let worker =
        thread::spawn(move || sleep_with_runtime_scheduler(blocked_sleep, Some(&cancellation)));

    thread::sleep(StdDuration::from_millis(20));
    group.cancel();
    assert_eq!(
        worker
            .join()
            .expect("scheduler sleep worker should join")
            .expect("a short sleep should fit the host deadline range"),
        super::RuntimeSchedulerWakeReason::Cancelled
    );
    assert!(
        start.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(100)),
        "scheduler sleep should wake promptly when cancelled; elapsed {:?}",
        start.elapsed()
    );
}

#[test]
fn runtime_scheduler_wakes_select_wait_on_cancellation() {
    let parent = CancellationContext::default();
    let group = TaskGroupValue::new(&parent);
    let cancellation = group.child_cancellation();
    let channel = ChannelValue::new();
    let start = Instant::now();
    let blocked_wait = crate::hosted_ci_timing_limit(StdDuration::from_millis(250));
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + blocked_wait;
        let _ = wait_for_runtime_scheduler(
            vec![channel],
            true,
            Vec::new(),
            Vec::new(),
            Some(deadline),
            Some(&cancellation),
        );
    });

    thread::sleep(StdDuration::from_millis(20));
    group.cancel();
    worker.join().expect("scheduler wait worker should join");
    assert!(
        start.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(100)),
        "scheduler wait should wake promptly when cancelled; elapsed {:?}",
        start.elapsed()
    );
}

#[test]
fn queue_iteration_wait_wakes_for_unobserved_task_group_failure() {
    let group = TaskGroupValue::new(&CancellationContext::default());
    let task = TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("boom"))));
    group.register_task(task);
    thread::sleep(StdDuration::from_millis(20));

    assert_eq!(
        recv_for_task_group_iteration(
            &ChannelValue::new(),
            &CancellationContext::default(),
            &group
        ),
        RecvValueResult::Cancelled
    );
}

#[test]
fn phase73_observable_runtime_task_group_cancellation_precedes_empty_completion() {
    let group = TaskGroupValue::new(&CancellationContext::default());
    group.cancel();

    assert_eq!(
        recv_for_task_group_iteration(
            &ChannelValue::new(),
            &CancellationContext::default(),
            &group,
        ),
        RecvValueResult::Cancelled,
        "an explicit task-group cancellation must not be reported as normal iteration exhaustion",
    );

    let parent = TaskGroupValue::new(&CancellationContext::default());
    let caller_cancellation = parent.child_cancellation();
    parent.cancel();
    assert_eq!(
        recv_for_task_group_iteration(
            &ChannelValue::new(),
            &caller_cancellation,
            &TaskGroupValue::new(&CancellationContext::default()),
        ),
        RecvValueResult::Cancelled,
        "caller cancellation must also precede empty task-group exhaustion",
    );
}

#[test]
fn phase73_observable_runtime_task_group_distinguishes_values_and_normal_completion() {
    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let channel = ChannelValue::new();
    channel
        .send(Value::String("first".to_string()))
        .expect("the first produced value should be admitted");
    channel
        .send(Value::String("second".to_string()))
        .expect("the second produced value should be admitted");

    assert_eq!(
        recv_for_task_group_iteration(&channel, &cancellation, &group),
        RecvValueResult::Value(Value::String("first".to_string())),
        "iteration must preserve producer ordering when more values remain queued",
    );
    assert_eq!(
        recv_for_task_group_iteration(&channel, &cancellation, &group),
        RecvValueResult::Value(Value::String("second".to_string())),
    );
    channel.close();
    assert_eq!(
        recv_for_task_group_iteration(&channel, &cancellation, &group),
        RecvValueResult::Closed,
        "an explicitly closed receive channel must end iteration normally",
    );

    assert_eq!(
        recv_for_task_group_iteration(
            &ChannelValue::new(),
            &CancellationContext::default(),
            &TaskGroupValue::new(&CancellationContext::default()),
        ),
        RecvValueResult::Closed,
        "an empty completed task group must end iteration without waiting forever",
    );
}

#[test]
fn phase73_observable_runtime_task_group_wait_wakes_as_cancelled() {
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = CancellationContext::default();
    let channel = ChannelValue::new();
    let (release_task, task_release) = std::sync::mpsc::channel();
    let task = TaskValue::from_handle(thread::spawn(move || {
        task_release
            .recv()
            .expect("the task-group iteration probe should release its producer");
        Ok(Value::Unit)
    }));
    group.register_task(task.clone());

    let waiting_group = group.clone();
    let (wait_started, started) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        wait_started
            .send(())
            .expect("the queue-iteration waiter should announce its start");
        recv_for_task_group_iteration(&channel, &cancellation, &waiting_group)
    });
    started
        .recv_timeout(StdDuration::from_secs(1))
        .expect("the queue-iteration waiter should start");
    thread::sleep(StdDuration::from_millis(20));
    group.cancel();

    assert_eq!(
        waiter.join().expect("the cancelled waiter should join"),
        RecvValueResult::Cancelled,
        "cancelling a live task group must wake a blocked receive-based iteration",
    );
    release_task
        .send(())
        .expect("the producer task should be released after the observation");
    assert_eq!(
        wait_task_ready(&task).expect("the released producer should finish"),
        Value::Unit,
    );
}

#[test]
fn task_group_cleanup_probe_detects_unbounded_waits_after_fresh_spawns() {
    let root_result = run_lightweight_root_task(|| {
        let group = TaskGroupValue::new(&CancellationContext::default());
        let channel = ChannelValue::new();
        let child_cancellation = group.child_cancellation();
        let child_channel = channel.clone();
        let waiting =
            spawn_lightweight_task_with_cancellation(child_cancellation.clone(), move || {
                let _ =
                    child_channel.recv_result_with_cancellation(None, Some(&child_cancellation));
                Ok(Value::Unit)
            })?;
        group.register_task(waiting);

        let tasks = group.drain_tasks();
        Ok(Value::Bool(task_group_cleanup_should_cancel(
            &tasks,
            &CancellationContext::default(),
        )))
    });

    assert_eq!(root_result.unwrap(), Value::Bool(true));
}

#[test]
fn task_wait_reachability_snapshot_does_not_retain_tasks_or_queues() {
    let channel = ChannelValue::new();
    let task = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));
    assert_eq!(
        wait_task_ready(&task).expect("snapshot dependency task should complete"),
        Value::Unit
    );

    let task_weak = Arc::downgrade(&task.inner);
    let channel_weak = Arc::downgrade(&channel.inner);
    let registration = super::TaskWaitRegistration {
        recv_channels: vec![channel.clone()],
        ignore_closed_recv_channels: false,
        send_channels: vec![channel.clone()],
        task_waits: vec![task.clone()],
        deadline: None,
        cancellation: None,
        fd_wait: None,
    };
    let snapshot = registration.reachability_snapshot();
    assert_eq!(snapshot.recv_channels.len(), 1);
    assert_eq!(snapshot.send_channels.len(), 1);
    assert_eq!(snapshot.task_waits.len(), 1);

    drop(registration);
    drop(task);
    drop(channel);
    for _ in 0..100 {
        if task_weak.upgrade().is_none() {
            break;
        }
        thread::yield_now();
    }
    assert!(task_weak.upgrade().is_none());
    assert!(channel_weak.upgrade().is_none());
}

#[test]
fn p63_task_wait_reachability_follows_live_dependencies_without_retaining_them() {
    let waiting = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    let dependency = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    let dependency_weak = Arc::downgrade(&dependency.inner);
    *lock_mutex(&waiting.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: vec![dependency_weak.clone(), dependency_weak.clone()],
        deadline: None,
        cancellation: None,
    });

    assert!(
        waiting.unbounded_wait_has_reachable_waker(),
        "a runnable joined task can still make progress for its waiter"
    );

    *lock_mutex(&dependency.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: vec![Arc::downgrade(&waiting.inner)],
        deadline: None,
        cancellation: None,
    });
    assert!(
        !waiting.unbounded_wait_has_reachable_waker(),
        "a pure task-wait cycle has no reachable source of progress"
    );

    super::complete_lightweight_task_state(
        &dependency.inner,
        TaskExecutionResult::Ready(Ok(Value::Unit)),
    );
    assert!(
        waiting.unbounded_wait_has_reachable_waker(),
        "a completed dependency makes the join ready even if its prior wait graph was cyclic"
    );

    drop(dependency);
    assert!(dependency_weak.upgrade().is_none());
    assert!(
        !waiting.unbounded_wait_has_reachable_waker(),
        "a dead weak dependency must not be treated as a reachable waker"
    );
}

#[test]
fn p63_task_join_reachability_tracks_weak_dependencies_and_external_wake_sources() {
    let joiner = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    let dependency = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    let dependency_weak = Arc::downgrade(&dependency.inner);
    *lock_mutex(&joiner.inner.join_dependencies) =
        Some(vec![dependency_weak.clone(), dependency_weak.clone()]);

    assert!(
        joiner.unbounded_wait_has_reachable_waker(),
        "a runnable join dependency is a reachable source of progress"
    );

    *lock_mutex(&dependency.inner.join_dependencies) = Some(vec![Arc::downgrade(&joiner.inner)]);
    assert!(
        !joiner.unbounded_wait_has_reachable_waker(),
        "a duplicate dependency inside a pure join cycle must remain bounded and unreachable"
    );

    super::complete_lightweight_task_state(
        &dependency.inner,
        TaskExecutionResult::Ready(Ok(Value::Unit)),
    );
    assert!(
        joiner.unbounded_wait_has_reachable_waker(),
        "completion makes a formerly cyclic join dependency ready"
    );
    drop(dependency);
    assert!(dependency_weak.upgrade().is_none());
    assert!(
        !joiner.unbounded_wait_has_reachable_waker(),
        "the join graph must not retain a completed dependency"
    );

    *lock_mutex(&joiner.inner.join_dependencies) = None;
    *lock_mutex(&joiner.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: Some(Instant::now() + StdDuration::from_secs(1)),
        cancellation: None,
    });
    assert!(
        joiner.unbounded_wait_has_reachable_waker(),
        "a finite deadline is an external wake source"
    );

    let cancellation_signal = Arc::new(super::RuntimeWakeSignal::new(false));
    *lock_mutex(&joiner.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: None,
        cancellation: Some(CancellationContext {
            flags: vec![cancellation_signal.clone()],
        }),
    });
    assert!(!joiner.unbounded_wait_has_reachable_waker());
    cancellation_signal.store(true, Ordering::SeqCst);
    assert!(
        joiner.unbounded_wait_has_reachable_waker(),
        "cancellation becomes an external wake source once signalled"
    );

    let recv_channel = ChannelValue::new();
    let send_channel = ChannelValue::with_capacity(1);
    assert_eq!(
        send_channel.try_send(Value::Unit),
        super::TrySendResult::Sent
    );
    let peer = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    recv_channel.register_task_handle(&peer);
    *lock_mutex(&peer.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: vec![Arc::downgrade(&recv_channel.inner)],
        ignore_closed_recv_channels: false,
        send_channels: Vec::new(),
        task_waits: Vec::new(),
        deadline: None,
        cancellation: None,
    });
    *lock_mutex(&joiner.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: vec![
            Arc::downgrade(&recv_channel.inner),
            Arc::downgrade(&recv_channel.inner),
        ],
        ignore_closed_recv_channels: false,
        send_channels: vec![
            Arc::downgrade(&send_channel.inner),
            Arc::downgrade(&send_channel.inner),
        ],
        task_waits: Vec::new(),
        deadline: None,
        cancellation: None,
    });
    assert!(
        !joiner.unbounded_wait_has_reachable_waker(),
        "duplicate Queue edges and a Queue/task cycle must terminate without inventing progress"
    );
}

#[test]
fn unobserved_task_cleanup_does_not_register_discarded_queue_results() {
    let returned_queue = ChannelValue::new();
    let root_result = run_lightweight_root_task({
        let returned_queue = returned_queue.clone();
        move || {
            let child_queue = returned_queue.clone();
            let child = spawn_lightweight_task(move || Ok(Value::Channel(child_queue)))?;

            let unobserved = child
                .wait_result_with_cancellation(None, None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            assert!(matches!(
                unobserved,
                TaskWaitStatus::Ready(Ok(Value::Channel(_)))
            ));
            assert!(
                returned_queue.registered_task_handles().is_empty(),
                "an unobserved cleanup wait must not deliver the discarded Queue result"
            );

            let observed = child
                .wait_result_with_cancellation_observed(None, None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            assert!(matches!(
                observed,
                TaskWaitStatus::Ready(Ok(Value::Channel(_)))
            ));
            assert_eq!(returned_queue.registered_task_handles().len(), 1);
            Ok(Value::Unit)
        }
    });

    assert_eq!(root_result.unwrap(), Value::Unit);
}

#[test]
fn queue_task_role_registries_are_keyed_and_prune_task_churn() {
    let channel = ChannelValue::new();
    let persistent = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };

    for _ in 0..8 {
        let transients = (0..512)
            .map(|_| TaskValue {
                inner: super::new_lightweight_task_state(true),
            })
            .collect::<Vec<_>>();
        for transient in &transients {
            channel.register_producer_task(transient);
            channel.register_task_handle(transient);
            ChannelValue::register_task(&channel.inner.sender_tasks, transient);
            ChannelValue::register_task(&channel.inner.receiver_tasks, transient);
        }
    }
    for _ in 0..512 {
        channel.register_producer_task(&persistent);
        channel.register_task_handle(&persistent);
        ChannelValue::register_task(&channel.inner.sender_tasks, &persistent);
        ChannelValue::register_task(&channel.inner.receiver_tasks, &persistent);
    }

    assert!(lock_mutex(&channel.inner.producer_tasks).tasks.len() <= 1024);
    assert!(lock_mutex(&channel.inner.task_handles).tasks.len() <= 1024);
    assert!(lock_mutex(&channel.inner.sender_tasks).tasks.len() <= 1024);
    assert!(lock_mutex(&channel.inner.receiver_tasks).tasks.len() <= 1024);

    assert_eq!(
        channel.registered_producer_tasks(),
        vec![persistent.clone()]
    );
    assert_eq!(channel.registered_task_handles(), vec![persistent.clone()]);
    assert_eq!(channel.registered_sender_tasks(), vec![persistent.clone()]);
    assert_eq!(channel.registered_receiver_tasks(), vec![persistent]);
    assert_eq!(lock_mutex(&channel.inner.producer_tasks).tasks.len(), 1);
    assert_eq!(lock_mutex(&channel.inner.task_handles).tasks.len(), 1);
    assert_eq!(lock_mutex(&channel.inner.sender_tasks).tasks.len(), 1);
    assert_eq!(lock_mutex(&channel.inner.receiver_tasks).tasks.len(), 1);
}

#[test]
fn queue_iteration_ignores_nonproducer_reachability_handles() {
    let channel = ChannelValue::new();
    let nonproducer = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };

    channel.register_task_handle(&nonproducer);

    assert!(
        channel.all_registered_producer_tasks_completed(),
        "a live task that only holds a Queue handle must not keep Queue iteration open"
    );

    channel.register_producer_task(&nonproducer);
    assert!(
        !channel.all_registered_producer_tasks_completed(),
        "the same live task must keep iteration open once it is explicitly registered as a producer"
    );

    super::complete_lightweight_task_state(&nonproducer.inner, TaskExecutionResult::Cancelled);
    assert!(channel.all_registered_producer_tasks_completed());
}

#[test]
fn task_group_reachability_rechecks_queue_after_a_waker_completes() {
    let channel = ChannelValue::with_capacity(1);
    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    let waiting = TaskValue {
        inner: super::new_lightweight_task_state(true),
    };
    *lock_mutex(&waiting.inner.current_wait) = Some(super::TaskWaitReachability {
        recv_channels: Vec::new(),
        ignore_closed_recv_channels: false,
        send_channels: vec![Arc::downgrade(&channel.inner)],
        task_waits: Vec::new(),
        deadline: None,
        cancellation: None,
    });

    let drained = channel.clone();
    super::install_after_task_group_send_reachability_initial_check_hook(move || {
        assert_eq!(drained.try_recv(), TryRecvResult::Value(Value::Unit));
    });
    assert!(
        waiting.unbounded_wait_has_reachable_waker(),
        "capacity made available during the graph walk must prevent cancellation"
    );
}

#[test]
fn dense_queue_wait_cycle_has_bounded_reachability_traversal() {
    let channel = ChannelValue::with_capacity(1);
    assert_eq!(channel.try_send(Value::Unit), super::TrySendResult::Sent);
    let tasks = (0..128)
        .map(|_| TaskValue {
            inner: super::new_lightweight_task_state(true),
        })
        .collect::<Vec<_>>();
    for task in &tasks {
        channel.register_task_handle(task);
        *lock_mutex(&task.inner.current_wait) = Some(super::TaskWaitReachability {
            recv_channels: Vec::new(),
            ignore_closed_recv_channels: false,
            send_channels: vec![Arc::downgrade(&channel.inner)],
            task_waits: Vec::new(),
            deadline: None,
            cancellation: None,
        });
    }

    let (reachable, channel_expansions) = tasks[0].unbounded_wait_reachability();
    assert!(!reachable);
    assert_eq!(
        channel_expansions, 1,
        "one shared send channel should be expanded once per graph walk"
    );
}

#[test]
fn task_completion_clears_abandoned_join_dependencies() {
    let task_state = super::new_lightweight_task_state(true);
    let dependency = super::new_lightweight_task_state(true);
    *lock_mutex(&task_state.join_dependencies) = Some(vec![Arc::downgrade(&dependency)]);

    super::complete_lightweight_task_state(&task_state, TaskExecutionResult::Cancelled);

    assert!(lock_mutex(&task_state.join_dependencies).is_none());
}

#[test]
fn value_equality_and_render_cover_collection_shapes() {
    let vec_value = Value::Vec(VecValue {
        element_type: Type::named("int32"),
        elements: vec![
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(2)),
        ],
    });
    assert_eq!(vec_value.render(), "[1, 2]");

    let set_a = Value::Set(SetValue {
        element_type: Type::named("int32"),
        elements: vec![
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(2)),
        ],
    });
    let set_b = Value::Set(SetValue {
        element_type: Type::named("int32"),
        elements: vec![
            Value::Int(IntegerValue::from_signed(2)),
            Value::Int(IntegerValue::from_signed(1)),
        ],
    });
    assert_eq!(set_a, set_b);
    assert_ne!(
        set_a,
        Value::Set(SetValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        })
    );
    assert_ne!(
        set_a,
        Value::Set(SetValue {
            element_type: Type::named("int32"),
            elements: vec![
                Value::Int(IntegerValue::from_signed(1)),
                Value::Int(IntegerValue::from_signed(3)),
            ],
        })
    );
    assert_eq!(set_a.render(), "{1, 2}");
    assert_eq!(
        Value::Set(SetValue {
            element_type: Type::named("int64"),
            elements: Vec::new(),
        })
        .render(),
        "set()"
    );

    let map_a = Value::Map(MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![
            (
                Value::String("a".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            ),
            (
                Value::String("b".to_string()),
                Value::Int(IntegerValue::from_signed(2)),
            ),
        ],
    });
    let map_b = Value::Map(MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![
            (
                Value::String("b".to_string()),
                Value::Int(IntegerValue::from_signed(2)),
            ),
            (
                Value::String("a".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            ),
        ],
    });
    assert_eq!(map_a, map_b);
    assert_ne!(
        map_a,
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("a".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            )],
        })
    );
    assert_ne!(
        map_a,
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![
                (
                    Value::String("a".to_string()),
                    Value::Int(IntegerValue::from_signed(1)),
                ),
                (
                    Value::String("b".to_string()),
                    Value::Int(IntegerValue::from_signed(3)),
                ),
            ],
        })
    );
    assert_eq!(map_a.render(), "{a: 1, b: 2}");
    assert_eq!(
        Value::ModuleNamespace(ModuleNamespaceValue {
            path: "pkg.tools".to_string(),
        })
        .render(),
        "<module pkg.tools>"
    );
    assert_value_equals_clone(Value::ModuleNamespace(ModuleNamespaceValue {
        path: "pkg.tools".to_string(),
    }));
    assert_value_equals_clone(Value::Duration(5));
    assert_value_equals_clone(Value::Range(RangeValue { start: 1, end: 4 }));
    assert_eq!(Value::Unit.render(), "");
    assert_value_equals_clone(Value::Unit);
    assert_ne!(Value::Unit, Value::Bool(false));

    assert_eq!(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Done".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(2))],
        })
        .render(),
        "Status.Done(2)"
    );
    assert_eq!(
        Value::Range(RangeValue { start: 1, end: 4 }).render(),
        "range(1, 4)"
    );
}

#[test]
fn nested_queue_producer_registration_walks_tuples_collections_instances_and_variants() {
    let queue_in_tuple = ChannelValue::new();
    let queue_in_vec = ChannelValue::new();
    let queue_in_set = ChannelValue::new();
    let queue_in_map_key = ChannelValue::new();
    let queue_in_map_value = ChannelValue::new();
    let queue_in_instance = ChannelValue::new();
    let queue_in_variant = ChannelValue::new();
    let task = TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)));

    let nested_values = [
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("Queue")],
            elements: vec![Value::Channel(queue_in_tuple.clone())],
        }),
        Value::Vec(VecValue {
            element_type: Type::named("Queue"),
            elements: vec![Value::Channel(queue_in_vec.clone())],
        }),
        Value::Set(SetValue {
            element_type: Type::named("Queue"),
            elements: vec![Value::Channel(queue_in_set.clone())],
        }),
        Value::Map(MapValue {
            key_type: Type::named("Queue"),
            value_type: Type::named("Queue"),
            entries: vec![(
                Value::Channel(queue_in_map_key.clone()),
                Value::Channel(queue_in_map_value.clone()),
            )],
        }),
        Value::Instance(super::InstanceValue {
            class_name: "Envelope".to_string(),
            fields: BTreeMap::from([(
                "queue".to_string(),
                Value::Channel(queue_in_instance.clone()),
            )]),
        }),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Envelope".to_string(),
            variant_name: "Some".to_string(),
            payloads: vec![Value::Channel(queue_in_variant.clone())],
        }),
    ];

    super::register_task_as_queue_producer_for_values(nested_values.iter(), &task);
    queue_in_vec.register_task_handle(&task);

    for queue in [
        queue_in_tuple,
        queue_in_vec,
        queue_in_set,
        queue_in_map_key,
        queue_in_map_value,
        queue_in_instance,
        queue_in_variant,
    ] {
        assert_eq!(queue.registered_task_handles(), vec![task.clone()]);
    }

    wait_task_ready(&task).expect("registered producer task should complete");
}

#[test]
fn file_and_encoding_helpers_cover_binary_roundtrip_surface() {
    let temp = TempDir::new("aura-runtime-bytes");
    let path = temp.path().join("data.bin");
    let encoded = b"aura".to_vec();

    let file = FileValue::create(path.to_str().expect("temp path should be valid UTF-8"))
        .expect("file create should succeed");
    file.write_bytes(&encoded)
        .expect("write_bytes should succeed");
    file.flush().expect("flush should succeed");
    file.close();

    let reopened = FileValue::open(path.to_str().expect("temp path should be valid UTF-8"))
        .expect("file open should succeed");
    let read_back = reopened.read_bytes().expect("read_bytes should succeed");
    assert_eq!(read_back, encoded);
    assert_eq!(
        io_decode_utf8(&read_back).expect("decode_utf8 should succeed"),
        "aura"
    );
    let invalid = io_decode_utf8(&[0xff]).expect_err("invalid UTF-8 should be rejected");
    assert_eq!(invalid.kind(), io::ErrorKind::InvalidData);

    let appender = FileValue::append(path.to_str().expect("temp path should be valid UTF-8"))
        .expect("file append should succeed");
    appender
        .write_all("-tail")
        .expect("append write_all should succeed");
    appender.flush().expect("append flush should succeed");
    appender.close();

    let appended = FileValue::open(path.to_str().expect("temp path should be valid UTF-8"))
        .expect("appended file should reopen");
    assert_eq!(
        appended.read_all().expect("read_all should decode text"),
        "aura-tail"
    );
    appended.close();

    let closed = FileValue::create(
        temp.path()
            .join("closed.txt")
            .to_str()
            .expect("temp path should be valid UTF-8"),
    )
    .expect("closed file should be created");
    closed.close();
    assert_eq!(
        closed
            .read_all()
            .expect_err("closed file read_all should fail")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        closed
            .read_bytes()
            .expect_err("closed file read_bytes should fail")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        closed
            .write_all("closed")
            .expect_err("closed file write_all should fail")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        closed
            .write_bytes(b"closed")
            .expect_err("closed file write_bytes should fail")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        closed
            .flush()
            .expect_err("closed file flush should fail")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn filesystem_helpers_surface_directory_conflicts_precisely() {
    let temp = TempDir::new("aura-fs-errors");
    let dir = temp.path().join("data");
    fs::create_dir(&dir).expect("directory should be created");

    let already_exists = create_dir_once(&dir).expect_err("existing directory should fail");
    assert_eq!(already_exists.kind(), std::io::ErrorKind::AlreadyExists);

    let is_directory = remove_file_checked(&dir).expect_err("directory removal should fail");
    assert_eq!(is_directory.kind(), std::io::ErrorKind::IsADirectory);

    let rendered = io_error(is_directory);
    let Value::EnumVariant(variant) = rendered else {
        panic!("io_error should return an enum variant");
    };
    assert_eq!(variant.variant_name, "IsDirectory");
}

#[test]
fn io_error_maps_closed_and_cancelled_resource_conditions() {
    let Value::EnumVariant(closed) = io_error(super::closed_resource_error()) else {
        panic!("closed resource errors should render as io.Error variants");
    };
    assert_eq!(closed.variant_name, "Closed");

    let Value::EnumVariant(cancelled) = io_error(super::cancelled_resource_error()) else {
        panic!("cancelled resource errors should render as io.Error variants");
    };
    assert_eq!(cancelled.variant_name, "Cancelled");
}

#[test]
fn io_error_maps_standard_error_kinds_to_stable_variants() {
    let cases = [
        (io::ErrorKind::NotFound, "NotFound"),
        (io::ErrorKind::PermissionDenied, "PermissionDenied"),
        (io::ErrorKind::AlreadyExists, "AlreadyExists"),
        (io::ErrorKind::IsADirectory, "IsDirectory"),
        (io::ErrorKind::ConnectionRefused, "ConnectionRefused"),
        (io::ErrorKind::ConnectionReset, "ConnectionReset"),
        (io::ErrorKind::ConnectionAborted, "ConnectionAborted"),
        (io::ErrorKind::NotConnected, "NotConnected"),
        (io::ErrorKind::AddrInUse, "AddrInUse"),
        (io::ErrorKind::AddrNotAvailable, "AddrNotAvailable"),
        (io::ErrorKind::BrokenPipe, "BrokenPipe"),
        (io::ErrorKind::TimedOut, "TimedOut"),
        (io::ErrorKind::WouldBlock, "WouldBlock"),
        (io::ErrorKind::UnexpectedEof, "UnexpectedEof"),
        (io::ErrorKind::InvalidInput, "InvalidInput"),
        (io::ErrorKind::InvalidData, "InvalidData"),
    ];
    for (kind, expected) in cases {
        let Value::EnumVariant(variant) = io_error(io::Error::new(kind, "plain error")) else {
            panic!("io_error should render {expected} as an enum variant");
        };
        assert_eq!(variant.enum_name, "io.Error");
        assert_eq!(variant.variant_name, expected);
        assert!(variant.payloads.is_empty());
    }

    let Value::EnumVariant(other) = io_error(io::Error::other("other diagnostic payload")) else {
        panic!("other io errors should render as io.Error.Other");
    };
    assert_eq!(other.variant_name, "Other");
    assert!(matches!(
        other.payloads.as_slice(),
        [Value::String(message)] if message == "other diagnostic payload"
    ));
}

#[test]
fn process_child_helpers_cover_empty_command_and_cancellation_edges() {
    fn assert_variant(value: Value, enum_name: &str, variant_name: &str) {
        let Value::EnumVariant(variant) = value else {
            panic!("expected {enum_name}.{variant_name} to render as an enum variant");
        };
        assert_eq!(variant.enum_name, enum_name);
        assert_eq!(variant.variant_name, variant_name);
        assert!(variant.payloads.is_empty());
    }

    let empty_command = ProcessChildValue::spawn(
        Vec::new(),
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect_err("empty process commands should fail before spawning");
    assert_eq!(empty_command.kind(), io::ErrorKind::InvalidInput);

    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();
    let child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 1".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("slow process should spawn");
    assert!(matches!(
        child.wait(Some(StdDuration::from_secs(1)), Some(&cancellation)),
        ProcessChildWaitStatus::Cancelled
    ));
    assert_variant(
        child
            .wait_or_none(Some(StdDuration::from_secs(1)), Some(&cancellation))
            .expect_err("cancelled wait_or_none should return a process error"),
        "Error",
        "Cancelled",
    );
    assert_variant(
        child
            .wait_ok(Some(StdDuration::from_secs(1)), Some(&cancellation))
            .expect_err("cancelled wait_ok should return a process error"),
        "Error",
        "Cancelled",
    );
    child.close();

    let killed_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 10".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("a live child should spawn for the kill contract");
    killed_child
        .kill()
        .expect("killing a running process should request immediate termination");
    let ProcessChildWaitStatus::Exited(killed_status) =
        killed_child.wait(Some(StdDuration::from_secs(2)), None)
    else {
        panic!("a killed process should still yield its exit status");
    };
    assert!(!killed_status.success());
    killed_child.close();

    let completed_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 0".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("short-lived process should spawn");
    let status = completed_child
        .wait_or_none(Some(StdDuration::from_secs(2)), None)
        .expect("successful wait_or_none should not produce a process error")
        .expect("completed process should return an exit status");
    assert!(status.success());
    let cached_status = completed_child
        .wait_ok(Some(StdDuration::from_secs(2)), None)
        .expect("cached successful exits should satisfy wait_ok");
    assert!(cached_status.success());
    assert!(completed_child
        .try_wait_once()
        .expect("cached try_wait_once should not fail")
        .expect("cached try_wait_once should return the prior status")
        .success());
    completed_child
        .terminate()
        .expect("terminating an already exited process should be a no-op");
    completed_child
        .kill()
        .expect("killing an already exited process should be a no-op");
    completed_child.close();
}

#[cfg(unix)]
#[test]
fn process_completed_values_preserve_output_status_and_check_semantics() {
    let success = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .status()
        .expect("successful status should be observable");
    let completed = ProcessCompletedValue::new(
        super::process_exit_status(success),
        b"standard output".to_vec(),
        b"standard error".to_vec(),
    );
    assert!(completed.success());
    completed
        .check()
        .expect("zero exit status should satisfy check");
    assert_eq!(
        completed.stdout().expect("stdout should decode"),
        "standard output"
    );
    assert_eq!(
        completed.stderr().expect("stderr should decode"),
        "standard error"
    );
    assert_eq!(completed.stdout_bytes(), b"standard output");
    assert_eq!(completed.stderr_bytes(), b"standard error");

    let failure = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .status()
        .expect("failing status should be observable");
    let failed =
        ProcessCompletedValue::new(super::process_exit_status(failure), vec![0xff], vec![0xfe]);
    assert!(!failed.success());
    assert_eq!(
        failed
            .stdout()
            .expect_err("invalid stdout UTF-8 must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        failed
            .stderr()
            .expect_err("invalid stderr UTF-8 must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(failed.stdout_bytes(), [0xff]);
    assert_eq!(failed.stderr_bytes(), [0xfe]);
    assert!(failed
        .check()
        .expect_err("non-zero exit status must fail check")
        .render()
        .contains("process exited with ExitStatus.Exited(7)"));
}

#[cfg(unix)]
#[test]
fn process_stdout_supports_line_then_byte_reads() {
    let child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'outline\\nrest'".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("stdout-producing child should spawn");
    let stdout = child.stdout().expect("stdout pipe should be captured");
    assert_eq!(
        stdout
            .read_line(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default())
            )
            .expect("stdout line should read")
            .as_deref(),
        Some("outline")
    );
    assert_eq!(
        stdout
            .read_bytes(
                4,
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default())
            )
            .expect("stdout bytes should read")
            .as_deref(),
        Some(&b"rest"[..])
    );
    assert!(matches!(
        child.wait(Some(StdDuration::from_secs(2)), None),
        ProcessChildWaitStatus::Exited(status) if status.success()
    ));
    child.close();
}

#[cfg(unix)]
#[test]
fn process_supervisor_wait_or_none_and_stop_cover_public_outcomes() {
    let empty = ProcessSupervisorValue::new();
    assert!(empty
        .wait_or_none(Some(StdDuration::ZERO), None)
        .expect("empty supervisor wait should not fail")
        .is_none());

    let completed = ProcessSupervisorValue::new();
    completed
        .start(
            "quick".to_string(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Never,
            StdDuration::ZERO,
            None,
            false,
        )
        .expect("quick supervised process should start");
    let event = completed
        .wait_or_none(Some(StdDuration::from_secs(2)), None)
        .expect("completed supervisor wait should not fail")
        .expect("completed supervisor wait should produce an event");
    let Value::EnumVariant(event) = event else {
        panic!("supervisor event should be a typed enum variant");
    };
    assert_eq!(event.enum_name, "SupervisorEvent");
    assert_eq!(event.variant_name, "Exited");
    assert_eq!(event.payloads[0], Value::String("quick".to_string()));
    assert!(completed.is_empty());

    let running = ProcessSupervisorValue::new();
    running
        .start(
            "slow".to_string(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 10".to_string(),
            ],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Never,
            StdDuration::ZERO,
            None,
            false,
        )
        .expect("slow supervised process should start");
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();
    let cancelled = running
        .wait_or_none(Some(StdDuration::from_secs(2)), Some(&cancellation))
        .expect_err("cancelled supervisor wait should return a process error");
    assert_eq!(cancelled.render(), "Error.Cancelled");
    running
        .stop()
        .expect("supervisor stop should close children");
    assert!(running.is_empty());
    running.close();
}

#[cfg(unix)]
#[test]
fn process_pipe_helpers_cover_stderr_reads_and_closed_edges() {
    let child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'errline\\nmore' >&2".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        false,
    )
    .expect("stderr-producing child should spawn");
    let stderr = child.stderr().expect("stderr pipe should be captured");
    assert_eq!(
        stderr
            .read_line(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default())
            )
            .expect("stderr line should read")
            .as_deref(),
        Some("errline")
    );
    assert_eq!(
        stderr
            .read_bytes(
                4,
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default())
            )
            .expect("stderr bytes should read")
            .as_deref(),
        Some(&b"more"[..])
    );
    let _ = child.wait(Some(StdDuration::from_secs(2)), None);
    stderr
        .flush()
        .expect("output pipes should allow no-op flushes before close");

    stderr.close();
    assert_eq!(
        stderr
            .read_all_bytes(None)
            .expect_err("closed pipes should reject read_all_bytes")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        stderr
            .read_line(None, None)
            .expect_err("closed pipes should reject read_line")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        stderr
            .read_bytes(1, None, None)
            .expect_err("closed pipes should reject read_bytes")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        stderr
            .write_bytes(b"x", None, None)
            .expect_err("closed pipes should reject writes before checking pipe direction")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        stderr
            .flush()
            .expect_err("closed pipes should reject flushes")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[cfg(unix)]
#[test]
fn process_pipe_helpers_cover_read_all_and_pipe_direction_errors() {
    let output_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'out'; printf 'err' >&2".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        false,
    )
    .expect("output-producing child should spawn");
    let stdout = output_child
        .stdout()
        .expect("stdout pipe should be captured");
    let stderr = output_child
        .stderr()
        .expect("stderr pipe should be captured");

    assert_eq!(
        stdout
            .write_bytes(b"nope", None, None)
            .expect_err("process stdout pipes should reject writes")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        stdout
            .read_all_bytes(Some(&CancellationContext::default()))
            .expect("stdout read_all_bytes should drain process output"),
        b"out"
    );
    assert_eq!(
        stderr
            .read_all(Some(&CancellationContext::default()))
            .expect("stderr read_all should drain process output"),
        "err"
    );
    let _ = output_child.wait(Some(StdDuration::from_secs(2)), None);
    output_child.close();

    let input_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cat >/dev/null".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("stdin-consuming child should spawn");
    let stdin = input_child.stdin().expect("stdin pipe should be captured");
    assert_eq!(
        stdin
            .read_all(None)
            .expect_err("process stdin pipes should reject read_all")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        stdin
            .read_line(None, None)
            .expect_err("process stdin pipes should reject read_line")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        stdin
            .read_bytes(1, None, None)
            .expect_err("process stdin pipes should reject read_bytes")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    stdin
        .write_all("done", Some(StdDuration::from_secs(2)), None)
        .expect("stdin pipes should accept writes");
    stdin.flush().expect("stdin pipe flush should succeed");
    stdin.close();
    let _ = input_child.wait(Some(StdDuration::from_secs(2)), None);
    input_child.close();
}

#[cfg(unix)]
#[test]
fn unix_error_normalization_helpers_cover_udp_and_websocket_edges() {
    let too_large = super::normalize_udp_send_error(io::Error::from_raw_os_error(libc::EMSGSIZE));
    assert_eq!(too_large.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        too_large.to_string(),
        "UDP datagram exceeds the platform send limit"
    );

    let other = super::normalize_udp_send_error(io::Error::new(io::ErrorKind::Other, "plain"));
    assert_eq!(other.kind(), io::ErrorKind::Other);
    assert_eq!(other.to_string(), "plain");

    let unsupported = super::unsupported_websocket_transport_error();
    assert_eq!(unsupported.kind(), io::ErrorKind::Unsupported);
    assert_eq!(unsupported.to_string(), "unsupported websocket transport");

    super::ensure_rustls_crypto_provider();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should expose a local address");
    let client = std::net::TcpStream::connect(address).expect("client stream should connect");
    let (_server, _) = listener.accept().expect("server stream should accept");
    let raw_fd = client.as_raw_fd();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(super::load_tls_root_store(None).expect("root store should load"))
        .with_no_client_auth();
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost".to_string()).expect("valid DNS name");
    let connection =
        rustls::ClientConnection::new(Arc::new(config), server_name).expect("client config");
    let stream = rustls::StreamOwned::new(connection, client);
    let maybe_tls = tungstenite::stream::MaybeTlsStream::Rustls(stream);
    assert_eq!(
        super::maybe_tls_stream_raw_fd(&maybe_tls).expect("rustls stream fd"),
        raw_fd
    );
    let websocket = tungstenite::WebSocket::from_raw_socket(
        maybe_tls,
        tungstenite::protocol::Role::Client,
        Some(super::websocket_config()),
    );
    let mut socket = super::WebSocketStateKind::MaybeTls(Box::new(websocket));
    assert_eq!(super::websocket_raw_fd(&socket).expect("raw fd"), raw_fd);
    super::websocket_set_nonblocking(&mut socket, true).expect("rustls stream should toggle flags");
    assert!(fd_is_nonblocking(raw_fd));
    super::websocket_set_nonblocking(&mut socket, false)
        .expect("rustls stream should restore blocking mode");
    assert!(!fd_is_nonblocking(raw_fd));
}

#[cfg(unix)]
#[test]
fn unix_listener_bind_rejects_existing_regular_files() {
    let temp = TempDir::new("aura-runtime-unix-bind");
    let path = temp.path().join("existing.txt");
    fs::write(&path, "important-user-data").expect("write regular file");

    let error = UnixListenerValue::bind(path.to_str().expect("path should be valid UTF-8"))
        .expect_err("binding over a regular file should fail");
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists
                | std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::PermissionDenied
        ),
        "unexpected unix bind error kind: {:?}",
        error.kind()
    );
    assert!(
        path.is_file(),
        "failed unix bind should leave the original regular file intact"
    );
}

#[cfg(unix)]
#[test]
fn unix_listener_bind_rejects_existing_live_socket_paths() {
    let path = PathBuf::from(format!("/tmp/aura-live-{}.sock", std::process::id()));
    let _ = fs::remove_file(&path);
    let listener = UnixListenerValue::bind(path.to_str().expect("valid unix socket path"))
        .expect("first unix listener bind should succeed");

    let error = UnixListenerValue::bind(path.to_str().expect("valid unix socket path"))
        .expect_err("binding over a live unix socket should fail");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

    listener.close();
    let _ = fs::remove_file(&path);
}

#[test]
fn supervisor_rejects_zero_backoff_when_restart_is_enabled() {
    let supervisor = ProcessSupervisorValue::new();
    let error = supervisor
        .start(
            "flaky".to_string(),
            vec!["/usr/bin/false".to_string()],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Always,
            StdDuration::ZERO,
            Some(1),
            true,
        )
        .expect_err("zero-backoff restart loops should be rejected");
    let Value::EnumVariant(variant) = error else {
        panic!("process supervisor start should return a process.Error variant");
    };
    assert_eq!(variant.enum_name, "Error");
    assert_eq!(variant.variant_name, "Io");
    assert_eq!(variant.payloads.len(), 1);
    assert_eq!(variant.payloads[0].render(), "io.Error.InvalidInput");
}

#[test]
fn supervisor_start_preserves_spawn_failures_in_the_typed_error_carrier() {
    let supervisor = ProcessSupervisorValue::new();
    let error = supervisor
        .start(
            "missing".to_string(),
            vec!["/definitely/missing/aura-supervisor-child".to_string()],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Never,
            StdDuration::ZERO,
            None,
            false,
        )
        .expect_err("a missing supervisor command should return process.Error.Spawn");
    let Value::EnumVariant(variant) = error else {
        panic!("process supervisor start should return a process.Error variant");
    };
    assert_eq!(variant.enum_name, "Error");
    assert_eq!(variant.variant_name, "Spawn");
    assert_eq!(variant.payloads.len(), 1);
    assert!(matches!(&variant.payloads[0], Value::String(message) if !message.is_empty()));
}

#[test]
fn supervisor_delays_restarts_and_reports_restart_counts() {
    let supervisor = ProcessSupervisorValue::new();
    supervisor
        .start(
            "flaky".to_string(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Always,
            StdDuration::from_millis(10),
            Some(1),
            false,
        )
        .expect("supervisor should start a short failing child");

    let restarted = match supervisor.wait(Some(StdDuration::from_secs(2)), None) {
        ProcessSupervisorWaitStatus::Event(event) => event,
        ProcessSupervisorWaitStatus::TimedOut => panic!("supervisor restart timed out"),
        ProcessSupervisorWaitStatus::Cancelled => panic!("supervisor restart was cancelled"),
    };
    let Value::EnumVariant(restarted) = restarted else {
        panic!("supervisor restart should return an event variant");
    };
    assert_eq!(restarted.enum_name, "SupervisorEvent");
    assert_eq!(restarted.variant_name, "Restarted");
    assert_eq!(restarted.payloads[0], Value::String("flaky".to_string()));
    assert_eq!(
        restarted.payloads[2],
        Value::Int(IntegerValue::from_signed(1))
    );

    let exited = match supervisor.wait(Some(StdDuration::from_secs(2)), None) {
        ProcessSupervisorWaitStatus::Event(event) => event,
        ProcessSupervisorWaitStatus::TimedOut => panic!("supervisor exit timed out"),
        ProcessSupervisorWaitStatus::Cancelled => panic!("supervisor exit was cancelled"),
    };
    let Value::EnumVariant(exited) = exited else {
        panic!("supervisor exit should return an event variant");
    };
    assert_eq!(exited.enum_name, "SupervisorEvent");
    assert_eq!(exited.variant_name, "Exited");
    assert_eq!(exited.payloads[0], Value::String("flaky".to_string()));
    assert_eq!(exited.payloads[2], Value::Int(IntegerValue::from_signed(1)));
    assert!(supervisor.is_empty());
}

#[cfg(unix)]
#[test]
fn supervisor_reports_restart_spawn_failure_after_executable_disappears() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new("aura-supervisor-restart-missing");
    let program = temp.path().join("short-lived-worker.sh");
    fs::write(&program, "#!/bin/sh\nsleep 0.05\nexit 1\n")
        .expect("temporary supervisor worker should be written");
    let mut permissions = fs::metadata(&program)
        .expect("temporary supervisor worker should have metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&program, permissions)
        .expect("temporary supervisor worker should be executable");

    let supervisor = ProcessSupervisorValue::new();
    supervisor
        .start(
            "vanishing".to_string(),
            vec![program.to_string_lossy().into_owned()],
            None,
            Vec::new(),
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessStdioConfig::Null,
            ProcessRestartPolicy::Always,
            StdDuration::from_millis(10),
            Some(1),
            false,
        )
        .expect("the initial supervised worker should start");
    assert!(matches!(
        supervisor.wait(Some(StdDuration::ZERO), None),
        ProcessSupervisorWaitStatus::TimedOut
    ));

    fs::remove_file(&program)
        .expect("the running executable should be unlinkable before its scheduled restart");
    let failed = match supervisor.wait(Some(StdDuration::from_secs(2)), None) {
        ProcessSupervisorWaitStatus::Event(event) => event,
        ProcessSupervisorWaitStatus::TimedOut => panic!("supervisor restart failure timed out"),
        ProcessSupervisorWaitStatus::Cancelled => {
            panic!("supervisor restart failure was unexpectedly cancelled")
        }
    };
    let Value::EnumVariant(failed) = failed else {
        panic!("supervisor restart failure should return an event variant");
    };
    assert_eq!(failed.enum_name, "SupervisorEvent");
    assert_eq!(failed.variant_name, "Failed");
    assert_eq!(failed.payloads[0], Value::String("vanishing".to_string()));
    assert_eq!(
        failed.payloads[1].render().split('(').next(),
        Some("Error.Spawn")
    );
    assert_eq!(failed.payloads[2], Value::Int(IntegerValue::from_signed(1)));
    assert!(supervisor.is_empty());
}

#[test]
fn tcp_udp_http_and_websocket_helpers_cover_timeout_and_protocol_surface() {
    let short_timeout = StdDuration::from_secs(5);
    let cancellation = CancellationContext::default();
    let listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp bind should succeed");
    let address = listener
        .local_addr()
        .expect("listener local addr should succeed");
    let server = listener.clone();
    let tcp_shutdown_barrier = Arc::new(Barrier::new(2));
    let server_shutdown_barrier = tcp_shutdown_barrier.clone();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(Some(short_timeout), Some(&CancellationContext::default()))
            .expect("tcp accept should succeed");
        let line = stream
            .read_line(Some(short_timeout), Some(&CancellationContext::default()))
            .expect("tcp read_line should succeed");
        assert_eq!(line.as_deref(), Some("ping"));
        stream
            .write_bytes(
                b"pong",
                Some(short_timeout),
                Some(&CancellationContext::default()),
            )
            .expect("tcp write_bytes should succeed");
        server_shutdown_barrier.wait();
        stream.close();
    });

    let client = TcpStreamValue::connect(&address, Some(short_timeout), Some(&cancellation))
        .expect("tcp connect should succeed");
    client
        .write_all("ping\n", Some(short_timeout), Some(&cancellation))
        .expect("tcp write_all should succeed");
    let bytes = client
        .read_exact(4, Some(short_timeout), Some(&cancellation))
        .expect("tcp read_exact should succeed");
    assert_eq!(bytes, b"pong");
    client
        .shutdown_both()
        .expect("a connected TCP stream should support a two-way shutdown");
    tcp_shutdown_barrier.wait();
    server_thread.join().expect("tcp server thread should join");

    let udp_server = UdpSocketValue::bind("127.0.0.1:0").expect("udp bind should succeed");
    let udp_address = udp_server
        .local_addr()
        .expect("udp local addr should succeed");
    let udp_thread = {
        let server = udp_server.clone();
        thread::spawn(move || {
            let datagram = server
                .recv_from(
                    64,
                    Some(short_timeout),
                    Some(&CancellationContext::default()),
                )
                .expect("udp recv_from should succeed")
                .expect("udp recv_from should return a datagram");
            assert_eq!(
                datagram.text().expect("udp datagram text should decode"),
                "ping"
            );
            server
                .send_to_bytes(
                    &datagram.address(),
                    b"pong",
                    Some(short_timeout),
                    Some(&CancellationContext::default()),
                )
                .expect("udp send_to_bytes should succeed");
        })
    };
    let udp_client = UdpSocketValue::bind("127.0.0.1:0").expect("udp client bind should succeed");
    assert_eq!(
        udp_client
            .peer_addr()
            .expect_err("a bound but unconnected UDP socket has no peer")
            .kind(),
        io::ErrorKind::NotConnected
    );
    udp_client
        .send_to_text(
            &udp_address,
            "ping",
            Some(short_timeout),
            Some(&cancellation),
        )
        .expect("udp send_to_text should succeed");
    let reply = udp_client
        .recv(64, Some(short_timeout), Some(&cancellation))
        .expect("udp recv should succeed")
        .expect("udp recv should return data");
    assert_eq!(reply, b"pong");
    udp_thread.join().expect("udp thread should join");
    udp_client.close();
    assert_eq!(
        udp_client
            .recv(64, Some(short_timeout), Some(&cancellation))
            .expect_err("a closed UDP socket cannot receive connected datagrams")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        udp_client
            .recv_from(64, Some(short_timeout), Some(&cancellation))
            .expect_err("a closed UDP socket cannot receive addressed datagrams")
            .kind(),
        io::ErrorKind::BrokenPipe
    );

    let http_listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let http_address = http_listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let http_thread = {
        let listener = http_listener.clone();
        thread::spawn(move || {
            let exchange = listener
                .accept(Some(short_timeout), Some(&CancellationContext::default()))
                .expect("http accept should succeed");
            assert_eq!(exchange.method(), "POST");
            assert_eq!(exchange.path(), "/echo");
            assert_eq!(
                exchange.body_text().expect("http body text should decode"),
                "aura"
            );
            exchange
                .respond_text(
                    200,
                    "ok",
                    vec![("content-type".to_string(), "text/plain".to_string())],
                )
                .expect("http respond should succeed");
        })
    };
    let response = HttpResponseValue::request_text(
        "POST",
        &format!("http://{}/echo", http_address),
        "aura",
        vec![("x-test".to_string(), "1".to_string())],
        Some(short_timeout),
        Some(&cancellation),
    )
    .expect("http request should succeed");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().expect("http response text should decode"),
        "ok"
    );
    http_thread.join().expect("http thread should join");

    let ws_listener =
        WebSocketListenerValue::bind("127.0.0.1:0").expect("websocket bind should succeed");
    let ws_address = ws_listener
        .local_addr()
        .expect("websocket listener local addr should succeed");
    let ws_thread = {
        let listener = ws_listener.clone();
        thread::spawn(move || {
            let socket = listener
                .accept(Some(short_timeout))
                .expect("websocket accept should succeed");
            let text = socket
                .recv_text(Some(short_timeout))
                .expect("websocket recv_text should succeed")
                .expect("websocket text message should be present");
            assert_eq!(text, "hello");
            socket
                .send_bytes(b"pong", Some(short_timeout))
                .expect("websocket send_bytes should succeed");
            socket.close().expect("websocket close should succeed");
        })
    };
    let ws_client =
        super::WebSocketValue::connect(&format!("ws://{}", ws_address), Some(short_timeout))
            .expect("websocket connect should succeed");
    ws_client
        .send_text("hello", Some(short_timeout))
        .expect("websocket send_text should succeed");
    let ws_reply = ws_client
        .recv_bytes(Some(short_timeout))
        .expect("websocket recv_bytes should succeed")
        .expect("websocket bytes should be present");
    assert_eq!(ws_reply, b"pong");
    ws_thread.join().expect("websocket thread should join");
}

#[cfg(unix)]
#[test]
fn udp_datagrams_preserve_bytes_and_socket_waits_report_timeout_cancellation_and_close() {
    let timeout = StdDuration::from_secs(2);
    let receiver = UdpSocketValue::bind("127.0.0.1:0").expect("UDP receiver should bind");
    let receiver_address = receiver
        .local_addr()
        .expect("UDP receiver address should be available");

    assert_eq!(
        receiver
            .recv(16, Some(StdDuration::ZERO), None)
            .expect("an expired connected receive should be a timeout"),
        None
    );
    assert!(receiver
        .recv_from(16, Some(StdDuration::ZERO), None)
        .expect("an expired addressed receive should be a timeout")
        .is_none());

    let cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = cancellation_group.child_cancellation();
    cancellation_group.cancel();
    assert_eq!(
        receiver
            .recv(16, Some(timeout), Some(&cancellation))
            .expect_err("a cancelled connected receive should stop waiting")
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        receiver
            .recv_from(16, Some(timeout), Some(&cancellation))
            .expect_err("a cancelled addressed receive should stop waiting")
            .kind(),
        io::ErrorKind::Interrupted
    );

    let sender = UdpSocketValue::bind("127.0.0.1:0").expect("UDP sender should bind");
    sender
        .send_to_bytes(&receiver_address, &[0xff], Some(timeout), None)
        .expect("UDP bytes should send");
    let datagram = receiver
        .recv_from(16, Some(timeout), None)
        .expect("UDP bytes should be received")
        .expect("the UDP datagram should be present");
    assert_eq!(datagram.bytes(), vec![0xff]);
    assert!(datagram.address().starts_with("127.0.0.1:"));
    assert_eq!(
        datagram
            .text()
            .expect_err("invalid UTF-8 remains available as bytes")
            .kind(),
        io::ErrorKind::InvalidData
    );

    receiver.close();
    assert_eq!(
        receiver
            .send_to_text("127.0.0.1:9", "closed", Some(timeout), None)
            .expect_err("a closed UDP socket cannot send")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        receiver
            .local_addr()
            .expect_err("a closed UDP socket has no local address")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        receiver
            .peer_addr()
            .expect_err("a closed UDP socket has no peer address")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[cfg(unix)]
#[test]
fn websocket_cross_type_messages_and_close_state_are_observable() {
    let timeout = StdDuration::from_secs(2);
    let listener =
        WebSocketListenerValue::bind("127.0.0.1:0").expect("websocket listener should bind");
    let address = listener
        .local_addr()
        .expect("websocket listener address should be available");
    let close_barrier = Arc::new(Barrier::new(2));
    let server_close_barrier = close_barrier.clone();
    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let socket = server
            .accept(Some(timeout))
            .expect("websocket server should accept");
        assert_eq!(
            socket
                .recv_text(Some(timeout))
                .expect("binary UTF-8 should be readable as text")
                .as_deref(),
            Some("binary text")
        );
        socket
            .send_text("text bytes", Some(timeout))
            .expect("websocket text should send");
        socket
            .send_bytes(&[0xff], Some(timeout))
            .expect("websocket binary data should send");
        server_close_barrier.wait();
        assert_eq!(
            socket
                .recv_bytes(Some(timeout))
                .expect("a peer close should end receive without an error"),
            None
        );
    });

    let client = super::WebSocketValue::connect(&format!("ws://{address}"), Some(timeout))
        .expect("websocket client should connect");
    client
        .send_bytes(b"binary text", Some(timeout))
        .expect("websocket binary data should send");
    assert_eq!(
        client
            .recv_bytes(Some(timeout))
            .expect("text should be readable as bytes"),
        Some(b"text bytes".to_vec())
    );
    assert_eq!(
        client
            .recv_text(Some(timeout))
            .expect_err("invalid UTF-8 binary data should not become text")
            .kind(),
        io::ErrorKind::InvalidData
    );
    client.close().expect("websocket client should close");
    close_barrier.wait();
    assert_eq!(
        client
            .send_text("after close", Some(timeout))
            .expect_err("a closed websocket cannot send")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        client
            .recv_text(Some(timeout))
            .expect_err("a closed websocket cannot receive")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        client
            .close()
            .expect_err("a websocket cannot be closed twice")
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    server_thread.join().expect("websocket server should join");
}

#[test]
fn tcp_stream_accessors_reads_half_shutdowns_and_closed_errors_are_consistent() {
    let timeout = StdDuration::from_secs(2);
    let cancellation = CancellationContext::default();
    let listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp bind should succeed");
    let listener_address = listener
        .local_addr()
        .expect("listener local address should succeed");
    let server = listener.clone();
    let server_listener_address = listener_address.clone();
    let shutdown_barrier = Arc::new(Barrier::new(2));
    let server_shutdown_barrier = shutdown_barrier.clone();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(Some(timeout), Some(&CancellationContext::default()))
            .expect("tcp accept should succeed");
        assert_eq!(
            stream.local_addr().expect("accepted local address"),
            server_listener_address
        );
        assert!(stream
            .peer_addr()
            .expect("accepted peer address")
            .starts_with("127.0.0.1:"));
        stream
            .write_all(
                "hello",
                Some(timeout),
                Some(&CancellationContext::default()),
            )
            .expect("tcp text write should succeed");
        stream.flush().expect("tcp flush should succeed");
        stream
            .shutdown_write()
            .expect("the server write half should close");
        server_shutdown_barrier.wait();
    });

    let client = TcpStreamValue::connect(&listener_address, Some(timeout), Some(&cancellation))
        .expect("tcp connect should succeed");
    assert!(client
        .local_addr()
        .expect("client local address")
        .starts_with("127.0.0.1:"));
    assert_eq!(
        client.peer_addr().expect("client peer address"),
        listener_address
    );
    client
        .shutdown_write()
        .expect("the client write half should close while reads remain available");
    assert_eq!(
        client
            .read_bytes(2, Some(timeout), Some(&cancellation))
            .expect("partial byte read should succeed"),
        Some(b"he".to_vec())
    );
    assert_eq!(
        client
            .read_all(Some(timeout), Some(&cancellation))
            .expect("remaining text should read through EOF"),
        "llo"
    );
    shutdown_barrier.wait();
    server_thread.join().expect("tcp server thread should join");

    let half_shutdown_barrier = Arc::new(Barrier::new(2));
    let half_server_barrier = half_shutdown_barrier.clone();
    let half_server = listener.clone();
    let half_server_thread = thread::spawn(move || {
        let stream = half_server
            .accept(Some(timeout), Some(&CancellationContext::default()))
            .expect("half-shutdown server should accept");
        half_server_barrier.wait();
        stream.close();
    });
    let half_client =
        TcpStreamValue::connect(&listener_address, Some(timeout), Some(&cancellation))
            .expect("half-shutdown client should connect");
    half_client
        .shutdown_read()
        .expect("the live client read half should close");
    half_client
        .shutdown_write()
        .expect("the live client write half should close");
    half_shutdown_barrier.wait();
    half_server_thread
        .join()
        .expect("half-shutdown server thread should join");
    half_client.close();

    client.close();
    let closed_errors = [
        client
            .read_all(Some(timeout), Some(&cancellation))
            .expect_err("closed text reads must fail"),
        client
            .read_bytes_all(Some(timeout), Some(&cancellation))
            .expect_err("closed byte reads must fail"),
        client
            .read_line(Some(timeout), Some(&cancellation))
            .expect_err("closed line reads must fail"),
        client
            .read_bytes(1, Some(timeout), Some(&cancellation))
            .expect_err("closed partial reads must fail"),
        client
            .read_exact(1, Some(timeout), Some(&cancellation))
            .expect_err("closed exact reads must fail"),
        client
            .write_bytes(b"x", Some(timeout), Some(&cancellation))
            .expect_err("closed writes must fail"),
        client.flush().expect_err("closed flushes must fail"),
        client
            .local_addr()
            .expect_err("closed local-address access must fail"),
        client
            .peer_addr()
            .expect_err("closed peer-address access must fail"),
        client
            .shutdown_read()
            .expect_err("closed read shutdowns must fail"),
        client
            .shutdown_write()
            .expect_err("closed write shutdowns must fail"),
        client
            .shutdown_both()
            .expect_err("closed two-way shutdowns must fail"),
    ];
    for error in closed_errors {
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "resource is closed");
    }
    listener.close();
}

#[test]
fn tcp_and_http_helpers_handle_large_payloads() {
    let timeout = StdDuration::from_secs(5);
    let cancellation = CancellationContext::default();
    let payload = vec![b'x'; 350_000];

    let listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp bind should succeed");
    let address = listener
        .local_addr()
        .expect("listener local addr should succeed");
    let server = listener.clone();
    let expected_len = payload.len();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(Some(timeout), Some(&CancellationContext::default()))
            .expect("tcp accept should succeed");
        let bytes = stream
            .read_exact(
                expected_len,
                Some(timeout),
                Some(&CancellationContext::default()),
            )
            .expect("tcp read_exact should succeed");
        assert_eq!(bytes.len(), expected_len);
        stream.close();
    });

    let client = TcpStreamValue::connect(&address, Some(timeout), Some(&cancellation))
        .expect("tcp connect should succeed");
    client
        .write_bytes(&payload, Some(timeout), Some(&cancellation))
        .expect("tcp write_bytes should succeed for large payloads");
    client.close();
    server_thread
        .join()
        .expect("tcp large-payload server should join");

    let body = "x".repeat(100_000);
    let http_listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let http_address = http_listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let expected_body = body.clone();
    let http_thread = {
        let listener = http_listener.clone();
        thread::spawn(move || {
            let exchange = listener
                .accept(Some(timeout), Some(&CancellationContext::default()))
                .expect("http accept should succeed");
            exchange
                .respond_text(200, &expected_body, Vec::new())
                .expect("http respond should succeed for large payloads");
        })
    };
    let response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{http_address}/large"),
        "",
        Vec::new(),
        Some(timeout),
        Some(&cancellation),
    )
    .expect("http request should succeed for large payloads");
    assert_eq!(
        response.text().expect("http body should decode"),
        body,
        "large HTTP bodies should round-trip"
    );
    http_thread.join().expect("http thread should join");
}

#[test]
fn lightweight_scheduler_handles_large_http_binary_round_trip() {
    let timeout = StdDuration::from_secs(5);
    let body = vec![0x7au8; 50_000];
    let expected = body.clone();
    let result = run_lightweight_root_task(move || {
        let listener = HttpListenerValue::bind("127.0.0.1:0")
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let server_body = expected.clone();
        let server = spawn_lightweight_task(move || {
            let exchange = listener
                .accept(Some(timeout), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            exchange
                .respond_bytes(200, &server_body, Vec::new())
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Unit)
        })?;

        let response = HttpResponseValue::request_bytes(
            "GET",
            &format!("http://{address}/large"),
            &[0],
            Vec::new(),
            Some(timeout),
            None,
        )
        .map_err(|error| Diagnostic::new(error.to_string()))?;
        assert_eq!(response.bytes(), body);
        wait_task_ready(&server)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "lightweight HTTP round-trip should succeed: {result:?}"
    );
}

#[test]
fn isolated_http_protocol_roundtrip_fits_forced_256_kib_callers() {
    // This is intentionally a runtime-boundary regression, not a compiled
    // Aura workload. The Rust closures call the HTTP runtime directly, so
    // success proves the protocol service keeps deep host-library frames off
    // these 256 KiB children. It does not include MIR/direct language-
    // execution frames and therefore does not justify a 256 KiB default.
    const SMALL_STACK: usize = 256 * 1024;
    let timeout = StdDuration::from_secs(5);
    let body = vec![0x6du8; 4 * 1024 * 1024];
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let listener = HttpListenerValue::bind("127.0.0.1:0")
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let server_body = body.clone();
        let server = spawn_lightweight_task_with_stack(SMALL_STACK, move || {
            let exchange = listener
                .accept(Some(timeout), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            exchange
                .respond_bytes(200, &server_body, Vec::new())
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Unit)
        })?;

        let timer_progressed = Arc::new(AtomicBool::new(false));
        let timer_progressed_in_task = timer_progressed.clone();
        let timer = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(1), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            timer_progressed_in_task.store(true, Ordering::SeqCst);
            Ok(Value::Unit)
        })?;

        let timer_progressed_in_client = timer_progressed.clone();
        let client = spawn_lightweight_task_with_stack(SMALL_STACK, move || {
            let response = HttpResponseValue::request_bytes(
                "GET",
                &format!("http://{address}/small-stack"),
                &[],
                Vec::new(),
                Some(timeout),
                None,
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            assert_eq!(response.bytes().len(), 4 * 1024 * 1024);
            assert!(
                timer_progressed_in_client.load(Ordering::SeqCst),
                "deep HTTP build and parse steps must not prevent sibling timer progress"
            );
            Ok(Value::Unit)
        })?;

        wait_task_ready(&client)?;
        wait_task_ready(&server)?;
        wait_task_ready(&timer)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "the isolated HTTP runtime path must fit forced 256 KiB callers: {result:?}"
    );
}

#[cfg(unix)]
#[test]
fn forced_256_kib_websocket_data_serializes_clones_without_blocking_siblings() {
    const SMALL_STACK: usize = 256 * 1024;
    let timeout = StdDuration::from_secs(2);
    let listener =
        WebSocketListenerValue::bind("127.0.0.1:0").expect("websocket listener should bind");
    let address = listener
        .local_addr()
        .expect("websocket listener should expose its address");
    let server = thread::spawn(move || {
        let socket = listener
            .accept(Some(timeout))
            .expect("websocket server should accept");
        thread::sleep(StdDuration::from_millis(100));
        socket
            .send_text("ready", Some(timeout))
            .expect("websocket server should send");
    });
    let socket =
        super::WebSocketValue::connect(&format!("ws://{address}/small-stack"), Some(timeout))
            .expect("websocket client should connect");
    let first_socket = socket.clone();
    let second_socket = socket.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let first = spawn_lightweight_task_with_stack(SMALL_STACK, move || {
            assert_eq!(
                first_socket
                    .recv_text(Some(timeout))
                    .map_err(|error| Diagnostic::new(error.to_string()))?,
                Some("ready".to_string())
            );
            Ok(Value::Unit)
        })?;
        let second = spawn_lightweight_task_with_stack(SMALL_STACK, move || {
            let error = second_socket
                .recv_text(Some(StdDuration::from_millis(20)))
                .expect_err("a cloned receive must time out while the first owns the socket");
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            Ok(Value::Unit)
        })?;
        let timer = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(5), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Unit)
        })?;
        wait_task_ready(&second)?;
        wait_task_ready(&timer)?;
        wait_task_ready(&first)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "cloned websocket contention must preserve scheduler progress: {result:?}"
    );
    socket.close().expect("websocket client should close");
    server.join().expect("websocket server should join");
}

#[cfg(unix)]
#[test]
fn forced_256_kib_tls_handshake_and_data_preserve_sibling_timer_progress() {
    const SMALL_STACK: usize = 256 * 1024;
    let timeout = StdDuration::from_secs(2);
    let temp = TempDir::new("aura-small-stack-tls");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem().as_bytes()).expect("write cert pem");
    fs::write(&key_path, certificate.key_pair.serialize_pem().as_bytes()).expect("write key pem");
    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path
            .to_str()
            .expect("certificate path should be UTF-8"),
        key_path.to_str().expect("key path should be UTF-8"),
    )
    .expect("TLS listener should bind");
    let address = listener.local_addr().expect("TLS address should exist");
    let server = thread::spawn(move || {
        let stream = listener
            .accept(Some(timeout), None)
            .expect("TLS server should accept");
        thread::sleep(StdDuration::from_millis(50));
        stream
            .write_all("ok", Some(timeout), None)
            .expect("TLS server should write");
        stream.close();
    });
    let ca_path = cert_path
        .to_str()
        .expect("certificate path should be UTF-8")
        .to_string();
    let timer_progressed = Arc::new(AtomicBool::new(false));
    let timer_progressed_in_task = timer_progressed.clone();
    let timer_progressed_in_client = timer_progressed.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let timer = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(5), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            timer_progressed_in_task.store(true, Ordering::SeqCst);
            Ok(Value::Unit)
        })?;
        let client = spawn_lightweight_task_with_stack(SMALL_STACK, move || {
            let stream =
                TlsStreamValue::connect(&address, "localhost", Some(&ca_path), Some(timeout), None)
                    .map_err(|error| Diagnostic::new(error.to_string()))?;
            assert_eq!(
                stream
                    .read_exact(2, Some(timeout), None)
                    .map_err(|error| Diagnostic::new(error.to_string()))?,
                b"ok"
            );
            assert!(
                timer_progressed_in_client.load(Ordering::SeqCst),
                "TLS steps must not prevent sibling timer progress"
            );
            stream.close();
            Ok(Value::Unit)
        })?;
        wait_task_ready(&client)?;
        wait_task_ready(&timer)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "TLS handshake/data must fit a forced 256 KiB task stack: {result:?}"
    );
    server.join().expect("TLS server should join");
}

#[cfg(unix)]
#[test]
fn tls_state_is_reusable_after_cancellation_at_a_protocol_step_boundary() {
    let timeout = StdDuration::from_secs(2);
    let temp = TempDir::new("aura-tls-cancel-reuse");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem().as_bytes()).expect("write cert pem");
    fs::write(&key_path, certificate.key_pair.serialize_pem().as_bytes()).expect("write key pem");
    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path
            .to_str()
            .expect("certificate path should be UTF-8"),
        key_path.to_str().expect("key path should be UTF-8"),
    )
    .expect("TLS listener should bind");
    let address = listener.local_addr().expect("TLS address should exist");
    let server = thread::spawn(move || {
        let stream = listener
            .accept(Some(timeout), None)
            .expect("TLS server should accept");
        thread::sleep(StdDuration::from_millis(100));
        stream
            .write_all("ok", Some(timeout), None)
            .expect("TLS server should write after client cancellation");
        stream.close();
    });
    let stream = TlsStreamValue::connect(
        &address,
        "localhost",
        Some(
            cert_path
                .to_str()
                .expect("certificate path should be UTF-8"),
        ),
        Some(timeout),
        None,
    )
    .expect("TLS client should connect");
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let canceller = thread::spawn(move || {
        thread::sleep(StdDuration::from_millis(10));
        group.cancel();
    });
    let error = stream
        .read_exact(2, Some(timeout), Some(&cancellation))
        .expect_err("TLS read should observe cancellation");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(
        stream
            .read_exact(2, Some(timeout), None)
            .expect("TLS state should be restored and reusable after cancellation"),
        b"ok"
    );
    stream.close();
    canceller.join().expect("canceller should join");
    server.join().expect("TLS server should join");
}

#[test]
fn lightweight_scheduler_handles_http_after_blocking_io_server_step() {
    let timeout = StdDuration::from_secs(2);
    let result = run_lightweight_root_task(move || {
        let listener = HttpListenerValue::bind("127.0.0.1:0")
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let server = spawn_lightweight_task(move || {
            let exchange = listener
                .accept(Some(timeout), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            let body = run_blocking_io(
                move || {
                    thread::sleep(StdDuration::from_millis(20));
                    Ok::<_, std::io::Error>("x".repeat(50_000))
                },
                None,
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            exchange
                .respond_text(200, &body, Vec::new())
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Unit)
        })?;

        let response = HttpResponseValue::request_text(
            "GET",
            &format!("http://{address}/large"),
            "x",
            Vec::new(),
            Some(timeout),
            None,
        )
        .map_err(|error| Diagnostic::new(error.to_string()))?;
        assert_eq!(
            response
                .text()
                .map_err(|error| Diagnostic::new(error.to_string()))?,
            "x".repeat(50_000)
        );
        wait_task_ready(&server)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "mixed HTTP/blocking-I/O scheduler path should succeed: {result:?}"
    );
}

#[test]
fn lightweight_tasks_observe_blocking_io_completion_before_parent_timeout() {
    let timeout = crate::hosted_ci_timing_limit(StdDuration::from_millis(250));
    let start = Instant::now();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let task = spawn_lightweight_task(move || {
            let value = run_blocking_io(
                move || {
                    thread::sleep(StdDuration::from_millis(20));
                    Ok::<_, std::io::Error>(41i32)
                },
                None,
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Int(IntegerValue::from_signed(i128::from(value))))
        })?;

        match task
            .wait_result_with_cancellation(Some(timeout), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            super::TaskWaitStatus::Ready(result) => {
                assert_eq!(
                    result?,
                    Value::Int(IntegerValue::from_signed(41)),
                    "blocking I/O completion should resume the waiting task promptly"
                );
                Ok(Value::Unit)
            }
            super::TaskWaitStatus::TimedOut => {
                Err(Diagnostic::new("blocking-I/O child task timed out"))
            }
            super::TaskWaitStatus::Cancelled => {
                Err(Diagnostic::new("blocking-I/O child task was cancelled"))
            }
        }
    });
    assert!(
        result.is_ok(),
        "blocking-I/O child task should finish before the wait timeout: {result:?}"
    );
    assert!(
        start.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(150)),
        "blocking-I/O wake should be prompt; elapsed {:?}",
        start.elapsed()
    );
}

#[test]
fn protocol_steps_run_on_the_dedicated_service_and_resume_siblings() {
    let sibling_progressed = Arc::new(AtomicBool::new(false));
    let sibling_progressed_in_task = sibling_progressed.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let sibling = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(5), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            sibling_progressed_in_task.store(true, Ordering::SeqCst);
            Ok(Value::Unit)
        })?;
        let worker_name = run_protocol_step(|| {
            thread::sleep(StdDuration::from_millis(30));
            Ok::<_, io::Error>(thread::current().name().unwrap_or("<unnamed>").to_string())
        })
        .map_err(|error| Diagnostic::new(error.to_string()))?;
        assert!(
            worker_name.starts_with("aura-protocol-step-"),
            "deep protocol work must run on the dedicated service, got {worker_name:?}"
        );
        assert!(
            sibling_progressed.load(Ordering::SeqCst),
            "waiting for a protocol step must yield the lightweight scheduler"
        );
        wait_task_ready(&sibling)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "dedicated protocol service should preserve scheduler progress: {result:?}"
    );
}

#[test]
fn protocol_steps_do_not_consume_a_forced_256_kib_caller_stack() {
    let join = thread::Builder::new()
        .name("aura-protocol-small-stack-test".to_string())
        .stack_size(256 * 1024)
        .spawn(|| {
            run_protocol_step(|| {
                // This lower-level isolation probe deliberately excludes
                // compiled Aura language-execution frames. Keep a frame
                // larger than the caller stack live while exercising
                // representative URL parsing, HTTP building, and chunk
                // decoding on the service worker.
                let scratch = [0x5au8; 384 * 1024];
                let parsed =
                    url::Url::parse("https://example.test/deep?q=1").map_err(io::Error::other)?;
                let request =
                    super::build_http_request_bytes("POST", &parsed, &scratch[..32], Vec::new())?;
                let chunked = b"4\r\naura\r\n0\r\n\r\n";
                let decoded = super::try_decode_chunked_http_body(chunked, 0)?
                    .ok_or_else(|| io::Error::other("chunked body remained incomplete"))?;
                Ok::<_, io::Error>((scratch[0], request.len(), decoded))
            })
        })
        .expect("256 KiB protocol caller should spawn");
    let (marker, request_len, decoded) = join
        .join()
        .expect("deep protocol frame must stay off the 256 KiB caller stack")
        .expect("representative protocol work should succeed");
    assert_eq!(marker, 0x5a);
    assert!(request_len > 32);
    assert_eq!(decoded, b"aura");
}

#[test]
fn protocol_step_panics_signal_the_waiter_and_preserve_the_service() {
    let error = run_protocol_step(|| -> io::Result<()> {
        panic!("intentional protocol-step panic");
    })
    .expect_err("protocol-step panics must become observable errors");
    assert!(error
        .to_string()
        .contains("protocol step panicked: intentional protocol-step panic"));
    assert_eq!(
        run_protocol_step(|| Ok::<_, io::Error>(42))
            .expect("a worker panic must not poison the protocol service"),
        42
    );
}

#[test]
fn protocol_state_step_panics_return_owned_state_and_preserve_the_service() {
    let pool = super::ProtocolStepPool::start();
    let (state, outcome) = super::run_protocol_state_step_on_pool(
        vec![1],
        |state| -> usize {
            state.push(2);
            panic!("intentional stateful protocol-step panic");
        },
        &pool,
        None,
        None,
    )
    .expect("a stateful protocol panic must still return ownership to the caller");
    assert_eq!(
        state,
        vec![1, 2],
        "mutations completed before the panic must remain in the returned state"
    );
    let error = outcome.expect_err("the contained stateful panic must remain observable");
    assert!(
        error
            .to_string()
            .contains("protocol state step panicked: intentional stateful protocol-step panic"),
        "unexpected stateful panic diagnostic: {error}"
    );

    let (state, outcome) = super::run_protocol_state_step_on_pool(
        vec![3],
        |state| {
            state.push(4);
            state.len()
        },
        &pool,
        None,
        None,
    )
    .expect("the protocol service must remain available after a stateful panic");
    assert_eq!(state, vec![3, 4]);
    assert_eq!(
        outcome.expect("the post-panic stateful operation should succeed"),
        2
    );
}

#[test]
fn protocol_step_deadlines_cover_queue_saturation_and_late_completion() {
    let pool = super::ProtocolStepPool::start();
    let release = Arc::new(AtomicBool::new(false));
    let release_guard = AtomicReleaseGuard(release.clone());
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..super::PROTOCOL_STEP_WORKER_COUNT {
        let release = release.clone();
        let started = started.clone();
        let admitted = pool.try_submit(Box::new(move || {
            started.fetch_add(1, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                thread::yield_now();
            }
        }));
        assert!(
            admitted.is_ok(),
            "worker-blocking protocol job should be admitted"
        );
    }
    let start_wait = Instant::now();
    while started.load(Ordering::SeqCst) < super::PROTOCOL_STEP_WORKER_COUNT {
        if start_wait.elapsed() >= StdDuration::from_secs(1) {
            release.store(true, Ordering::SeqCst);
            panic!("protocol workers should start saturation jobs promptly");
        }
        thread::yield_now();
    }
    let mut saturated = false;
    for _ in 0..=super::PROTOCOL_STEP_QUEUE_CAPACITY {
        let admitted = pool.try_submit(Box::new(|| {}));
        if admitted.is_err() {
            saturated = true;
            break;
        }
    }

    let deadline = Instant::now() + StdDuration::from_millis(10);
    let (state, saturated_result) = super::run_protocol_state_step_on_pool(
        vec![41],
        |state| state.push(42),
        &pool,
        Some(deadline),
        None,
    )
    .expect("stateful admission should always return owned state");
    release.store(true, Ordering::SeqCst);
    drop(release_guard);
    assert!(
        saturated,
        "protocol queue should enforce its declared bound"
    );
    let error =
        saturated_result.expect_err("a saturated stateful queue must honor admission deadlines");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        state,
        vec![41],
        "timed-out admission must return unmodified owned protocol state"
    );

    let completed = Arc::new(AtomicBool::new(false));
    let completed_in_step = completed.clone();
    let deadline = Instant::now() + StdDuration::from_millis(5);
    let error = super::run_protocol_step_before(
        move || {
            thread::sleep(StdDuration::from_millis(20));
            completed_in_step.store(true, Ordering::SeqCst);
            Ok::<_, io::Error>(())
        },
        Some(deadline),
        None,
    )
    .expect_err("late protocol success must become a timeout");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        completed.load(Ordering::SeqCst),
        "late steps must still complete before timeout is reported"
    );
}

#[test]
fn lightweight_blocking_io_observes_pre_cancelled_and_wait_cancelled_contexts() {
    let result = run_lightweight_root_task(move || {
        let pre_cancelled_group = TaskGroupValue::new(&CancellationContext::default());
        let pre_cancelled = pre_cancelled_group.child_cancellation();
        pre_cancelled_group.cancel();
        let error = run_blocking_io(|| Ok::<_, io::Error>(()), Some(&pre_cancelled))
            .expect_err("pre-cancelled blocking I/O should not start");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);

        let wait_cancelled_group = TaskGroupValue::new(&CancellationContext::default());
        let wait_cancelled = wait_cancelled_group.child_cancellation();
        let canceller = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(10), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            wait_cancelled_group.cancel();
            Ok(Value::Unit)
        })?;
        let error = run_blocking_io(
            || {
                thread::sleep(StdDuration::from_millis(100));
                Ok::<_, io::Error>(())
            },
            Some(&wait_cancelled),
        )
        .expect_err("blocking I/O wait should observe cancellation");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        wait_task_ready(&canceller)?;
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "blocking I/O cancellation paths should complete: {result:?}"
    );
}

fn wait_for_count(counter: &AtomicUsize, expected: usize, message: &str) {
    let deadline = Instant::now() + StdDuration::from_secs(2);
    while counter.load(Ordering::SeqCst) < expected {
        assert!(Instant::now() < deadline, "{message}");
        thread::yield_now();
    }
}

fn gated_blocking_job(
    started: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    executions: Arc<AtomicUsize>,
) -> super::BlockingIoJob {
    Box::new(move || {
        started.fetch_add(1, Ordering::SeqCst);
        while !release.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        executions.fetch_add(1, Ordering::SeqCst);
    })
}

#[test]
fn blocking_io_pool_is_lazy_and_starts_the_exact_injected_worker_count_once() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 3,
        queue_capacity: None,
    });
    assert_eq!(pool.worker_start_count(), 0);

    let completed = Arc::new(AtomicUsize::new(0));
    let completed_in_job = completed.clone();
    pool.submit(
        Box::new(move || {
            completed_in_job.fetch_add(1, Ordering::SeqCst);
        }),
        None,
        None,
    )
    .expect("the first job should initialize and enter the pool");
    wait_for_count(&completed, 1, "the first blocking job did not complete");
    assert_eq!(pool.worker_start_count(), 3);

    pool.submit(Box::new(|| {}), None, None)
        .expect("later jobs should reuse the initialized worker set");
    assert_eq!(pool.worker_start_count(), 3);
    pool.shutdown_for_test();
}

#[test]
fn impossible_blocking_io_worker_capacity_fails_before_spawning_or_executing_and_is_cached() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: usize::MAX,
        queue_capacity: None,
    });
    let spawn_attempts = Arc::new(AtomicUsize::new(0));
    let executed = Arc::new(AtomicUsize::new(0));
    let spawner_attempts = spawn_attempts.clone();
    let spawner = move |_: usize,
                        _: String,
                        _: super::BlockingIoWorkerEntry|
          -> io::Result<thread::JoinHandle<()>> {
        spawner_attempts.fetch_add(1, Ordering::SeqCst);
        panic!("worker spawning must not begin after handle reservation fails")
    };
    let executed_in_job = executed.clone();
    let error = pool
        .submit_with_spawner(
            Box::new(move || {
                executed_in_job.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            None,
            &spawner,
        )
        .expect_err("an impossible explicit worker count must fail before pool startup");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(
        error
            .to_string()
            .contains("AU4006: failed to reserve blocking-I/O worker handles"),
        "the reservation failure must retain its stable runtime-configuration diagnostic: {error}"
    );
    assert!(
        error.to_string().contains(&usize::MAX.to_string()),
        "the reservation failure must identify the rejected explicit worker count: {error}"
    );
    assert_eq!(spawn_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(executed.load(Ordering::SeqCst), 0);

    let repeated = pool
        .submit_with_spawner(Box::new(|| {}), None, None, &spawner)
        .expect_err("the impossible-capacity startup failure must be cached");
    assert_eq!(repeated.to_string(), error.to_string());
    assert_eq!(spawn_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(executed.load(Ordering::SeqCst), 0);
}

#[test]
fn blocking_io_pool_worker_creation_failure_is_all_or_nothing_and_cached() {
    for failed_index in 0..4 {
        let pool = BlockingIoPool::new(BlockingIoPoolConfig {
            worker_count: 4,
            queue_capacity: Some(1),
        });
        let spawn_attempts = Arc::new(AtomicUsize::new(0));
        let exited_workers = Arc::new(AtomicUsize::new(0));
        let spawner_attempts = spawn_attempts.clone();
        let spawner_exits = exited_workers.clone();
        let spawner = move |index: usize, name: String, entry: super::BlockingIoWorkerEntry| {
            spawner_attempts.fetch_add(1, Ordering::SeqCst);
            if index == failed_index {
                return Err(io::Error::other("injected worker creation failure"));
            }
            let exited = spawner_exits.clone();
            thread::Builder::new().name(name).spawn(move || {
                entry();
                exited.fetch_add(1, Ordering::SeqCst);
            })
        };
        let executed = Arc::new(AtomicUsize::new(0));
        let executed_in_job = executed.clone();
        let error = pool
            .submit_with_spawner(
                Box::new(move || {
                    executed_in_job.fetch_add(1, Ordering::SeqCst);
                }),
                None,
                None,
                &spawner,
            )
            .expect_err("partial worker creation must reject the first job");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains(&format!(
            "AU4006: failed to create blocking-I/O worker {failed_index}"
        )));
        assert!(error
            .to_string()
            .contains("injected worker creation failure"));
        wait_for_count(
            &exited_workers,
            failed_index,
            "workers created before the failure did not shut down",
        );
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        assert_eq!(spawn_attempts.load(Ordering::SeqCst), failed_index + 1);

        let second = pool
            .submit_with_spawner(Box::new(|| {}), None, None, &spawner)
            .expect_err("the initialization failure must be cached");
        assert_eq!(second.to_string(), error.to_string());
        assert_eq!(spawn_attempts.load(Ordering::SeqCst), failed_index + 1);
    }
}

#[test]
fn blocking_io_pool_startup_failure_crosses_runtime_boundaries_as_fatal_au4006() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: None,
    });
    let diagnostic = super::catch_lightweight_task_failure(|| -> Result<(), Diagnostic> {
        let failed_submission = pool.submit_with_spawner(
            Box::new(|| panic!("a rejected job must never execute")),
            None,
            None,
            &|_, _, _| Err(io::Error::other("injected host failure")),
        );
        super::preserve_blocking_io_submission_error(failed_submission)
            .map_err(|error| Diagnostic::new(error.to_string()))
    })
    .expect_err("pool startup failure must terminate the runtime invocation");
    assert_eq!(diagnostic.code, "AU4006");
    assert_eq!(
        diagnostic.message,
        "failed to create blocking-I/O worker 0: injected host failure"
    );

    let unrelated = std::panic::catch_unwind(|| {
        let _ = super::catch_lightweight_task_failure(|| -> Result<(), Diagnostic> {
            panic!("unrelated runtime panic")
        });
    })
    .expect_err("unrelated panics must not be rewritten as configuration failures");
    assert_eq!(
        unrelated.downcast_ref::<&str>(),
        Some(&"unrelated runtime panic")
    );
}

#[test]
fn blocking_io_startup_fallback_wraps_plain_host_errors_as_fatal_au4006() {
    let diagnostic = super::catch_lightweight_task_failure(|| -> Result<(), Diagnostic> {
        super::raise_blocking_io_pool_startup_failure(io::Error::other(
            "plain host startup failure",
        ))
    })
    .expect_err("a plain host startup failure must terminate the runtime invocation");
    assert_eq!(diagnostic.code, "AU4006");
    assert_eq!(
        diagnostic.message,
        "failed to initialize the blocking-I/O pool: plain host startup failure"
    );
}

#[test]
fn non_lightweight_blocking_io_callers_use_the_shared_dedicated_pool() {
    assert!(
        super::current_lightweight_task_id().is_none(),
        "this probe must begin outside an Aura lightweight task"
    );
    let worker_name = run_blocking_io(
        || Ok::<_, io::Error>(thread::current().name().unwrap_or("<unnamed>").to_string()),
        None,
    )
    .expect("a host-side caller should block until its shared-pool job completes");
    assert!(
        worker_name.starts_with("aura-blocking-io-"),
        "host-side callers must not fall back to synchronous execution: {worker_name:?}"
    );
}

#[test]
fn bounded_blocking_io_admission_times_out_and_cancels_before_execution() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(started.clone(), release.clone(), completed.clone()),
        None,
        None,
    )
    .unwrap();
    wait_for_count(&started, 1, "the worker saturation job did not start");
    pool.submit(Box::new(|| {}), None, None)
        .expect("one pending job should fill the bounded queue");

    let timed_out_executions = Arc::new(AtomicUsize::new(0));
    let timed_out_in_job = timed_out_executions.clone();
    let error = pool
        .submit(
            Box::new(move || {
                timed_out_in_job.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Instant::now() + StdDuration::from_millis(20)),
            None,
        )
        .expect_err("full-queue admission should honor its deadline");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);

    let cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = cancellation_group.child_cancellation();
    cancellation_group.cancel();
    let cancelled_executions = Arc::new(AtomicUsize::new(0));
    let cancelled_in_job = cancelled_executions.clone();
    let error = pool
        .submit(
            Box::new(move || {
                cancelled_in_job.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            Some(&cancellation),
        )
        .expect_err("pre-cancelled admission should not submit a job");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);

    release.store(true, Ordering::SeqCst);
    wait_for_count(&completed, 1, "the saturated worker did not drain");
    pool.wait_until_idle_for_test();
    assert_eq!(timed_out_executions.load(Ordering::SeqCst), 0);
    assert_eq!(cancelled_executions.load(Ordering::SeqCst), 0);
    assert_eq!(pool.admission_waiter_count(), 0);
    pool.shutdown_for_test();
}

#[test]
fn released_blocking_io_slot_skips_expired_oldest_waiter_and_accepts_next_in_fifo() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let expired_executions = Arc::new(AtomicUsize::new(0));
    let expired_executions_in_job = expired_executions.clone();
    let expired_outcome = Arc::new(Mutex::new(None));
    let expired_completion = ChannelValue::new();
    let live_executions = Arc::new(AtomicUsize::new(0));
    let live_executions_in_job = live_executions.clone();
    let live_outcome = Arc::new(Mutex::new(None));
    let live_completion = ChannelValue::new();

    let (mut finished, live_job) = {
        let mut state = lock_mutex(&pool.state);
        state
            .queue
            .push_back(Box::new(|| panic!("the occupying job must not execute")));
        state
            .admission_waiters
            .push_back(super::BlockingIoAdmissionWaiter {
                id: 0,
                job: Some(Box::new(move || {
                    expired_executions_in_job.fetch_add(1, Ordering::SeqCst);
                })),
                deadline: Some(
                    Instant::now()
                        .checked_sub(StdDuration::from_secs(1))
                        .unwrap_or_else(Instant::now),
                ),
                cancellation: None,
                outcome: expired_outcome.clone(),
                completion: expired_completion.clone(),
            });
        state
            .admission_waiters
            .push_back(super::BlockingIoAdmissionWaiter {
                id: 1,
                job: Some(Box::new(move || {
                    live_executions_in_job.fetch_add(1, Ordering::SeqCst);
                })),
                deadline: None,
                cancellation: None,
                outcome: live_outcome.clone(),
                completion: live_completion.clone(),
            });

        drop(
            state
                .queue
                .pop_front()
                .expect("removing the occupying job should open one queue slot"),
        );
        let finished = pool.fill_available_admission_slots_locked(&mut state);
        assert_eq!(
            state.queue.len(),
            1,
            "the live successor must consume the slot skipped by the expired waiter"
        );
        assert!(
            state.admission_waiters.is_empty(),
            "both decided waiters must leave the admission queue without leaking capacity"
        );
        let live_job = state
            .queue
            .pop_front()
            .expect("the live successor's accepted job must occupy the released slot");
        (finished, live_job)
    };

    assert_eq!(finished.len(), 2);
    assert_eq!(
        *lock_mutex(&expired_outcome),
        Some(super::BlockingIoAdmissionOutcome::TimedOut)
    );
    assert_eq!(
        *lock_mutex(&live_outcome),
        Some(super::BlockingIoAdmissionOutcome::Accepted)
    );
    assert_eq!(
        expired_completion.try_recv(),
        TryRecvResult::Empty,
        "the timeout outcome must be committed before its completion is signalled"
    );
    assert_eq!(
        live_completion.try_recv(),
        TryRecvResult::Empty,
        "the acceptance outcome must be committed before its completion is signalled"
    );
    let expired_waiter = finished.remove(0);
    assert!(
        expired_waiter.job.is_some(),
        "timing out must retain and ultimately drop the unexecuted job"
    );
    let live_waiter = finished
        .pop()
        .expect("the live successor should be returned for completion signalling");
    assert!(
        live_waiter.job.is_none(),
        "acceptance must transfer the live successor's job into the released slot"
    );
    BlockingIoPool::signal_admission_waiter(expired_waiter);
    BlockingIoPool::signal_admission_waiter(live_waiter);
    assert_eq!(
        expired_completion.try_recv(),
        TryRecvResult::Value(Value::Unit)
    );
    assert_eq!(expired_completion.try_recv(), TryRecvResult::Closed);
    assert_eq!(
        live_completion.try_recv(),
        TryRecvResult::Value(Value::Unit)
    );
    assert_eq!(live_completion.try_recv(), TryRecvResult::Closed);

    live_job();
    assert_eq!(expired_executions.load(Ordering::SeqCst), 0);
    assert_eq!(live_executions.load(Ordering::SeqCst), 1);
    assert_eq!(pool.pending_job_count(), 0);
    assert_eq!(pool.admission_waiter_count(), 0);
}

#[test]
fn bounded_blocking_io_capacity_counts_pending_jobs_but_excludes_running_jobs_and_waiters() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 2,
        queue_capacity: Some(3),
    });
    let release = Arc::new(AtomicBool::new(false));
    let _release_on_unwind = AtomicReleaseGuard(release.clone());
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        pool.submit(
            gated_blocking_job(
                started.clone(),
                release.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
            None,
            None,
        )
        .expect("both workers should accept a running saturation job");
    }
    wait_for_count(
        &started,
        2,
        "both capacity-accounting saturation jobs should start",
    );

    let executed = Arc::new(Mutex::new(Vec::new()));
    for job_id in 0..3 {
        let executed_in_job = executed.clone();
        pool.submit(
            Box::new(move || lock_mutex(&executed_in_job).push(job_id)),
            None,
            None,
        )
        .expect("each configured pending slot should accept one job");
    }

    let first_waiter_pool = pool.clone();
    let first_waiter_executed = executed.clone();
    let first_waiter = thread::spawn(move || {
        first_waiter_pool.submit(
            Box::new(move || lock_mutex(&first_waiter_executed).push(3)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(1);
    let second_waiter_pool = pool.clone();
    let second_waiter_executed = executed.clone();
    let second_waiter = thread::spawn(move || {
        second_waiter_pool.submit(
            Box::new(move || lock_mutex(&second_waiter_executed).push(4)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(2);

    assert_eq!(
        pool.active_job_count(),
        2,
        "running jobs must not consume configured pending-queue capacity"
    );
    assert_eq!(
        pool.pending_job_count(),
        3,
        "all three configured pending slots should remain occupied"
    );
    assert_eq!(
        pool.admission_waiter_count(),
        2,
        "parked admission waiters must remain outside pending capacity"
    );

    release.store(true, Ordering::SeqCst);
    first_waiter
        .join()
        .expect("the first capacity waiter should not panic")
        .expect("the first capacity waiter should eventually be admitted");
    second_waiter
        .join()
        .expect("the second capacity waiter should not panic")
        .expect("the second capacity waiter should eventually be admitted");
    pool.wait_until_idle_for_test();
    let mut executed = lock_mutex(&executed).clone();
    executed.sort_unstable();
    assert_eq!(executed, vec![0, 1, 2, 3, 4]);
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_admission_register_recheck_has_no_missed_cancel_or_slot_wake() {
    let cancelled_pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let cancelled_release = Arc::new(AtomicBool::new(false));
    let cancelled_started = Arc::new(AtomicUsize::new(0));
    cancelled_pool
        .submit(
            gated_blocking_job(
                cancelled_started.clone(),
                cancelled_release.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
            None,
            None,
        )
        .unwrap();
    wait_for_count(
        &cancelled_started,
        1,
        "the register-recheck cancellation gate did not start",
    );
    cancelled_pool
        .submit(Box::new(|| {}), None, None)
        .expect("the cancellation recheck should begin with a full queue");
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    super::install_after_blocking_io_admission_register_hook({
        let group = group.clone();
        move || group.cancel()
    });
    let rejected_executions = Arc::new(AtomicUsize::new(0));
    let rejected_counter = rejected_executions.clone();
    let error = cancelled_pool
        .submit(
            Box::new(move || {
                rejected_counter.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            Some(&cancellation),
        )
        .expect_err("cancellation after registration must be observed before parking");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    cancelled_release.store(true, Ordering::SeqCst);
    cancelled_pool.wait_until_idle_for_test();
    assert_eq!(rejected_executions.load(Ordering::SeqCst), 0);
    assert_eq!(cancelled_pool.admission_waiter_count(), 0);
    cancelled_pool.shutdown_for_test();

    let released_pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let released_gate = Arc::new(AtomicBool::new(false));
    let released_started = Arc::new(AtomicUsize::new(0));
    released_pool
        .submit(
            gated_blocking_job(
                released_started.clone(),
                released_gate.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
            None,
            None,
        )
        .unwrap();
    wait_for_count(
        &released_started,
        1,
        "the register-recheck slot gate did not start",
    );
    released_pool
        .submit(Box::new(|| {}), None, None)
        .expect("the slot recheck should begin with a full queue");
    super::install_after_blocking_io_admission_register_hook({
        let released_gate = released_gate.clone();
        move || released_gate.store(true, Ordering::SeqCst)
    });
    let admitted_executions = Arc::new(AtomicUsize::new(0));
    let admitted_counter = admitted_executions.clone();
    released_pool
        .submit(
            Box::new(move || {
                admitted_counter.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Instant::now() + StdDuration::from_secs(1)),
            None,
        )
        .expect("a slot released after registration must wake and admit the waiter");
    released_pool.wait_until_idle_for_test();
    assert_eq!(admitted_executions.load(Ordering::SeqCst), 1);
    assert_eq!(released_pool.admission_waiter_count(), 0);
    released_pool.shutdown_for_test();
}

#[test]
fn bounded_blocking_io_admission_is_fifo_after_a_cancelled_waiter_is_removed() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(started.clone(), release.clone(), completed),
        None,
        None,
    )
    .unwrap();
    wait_for_count(&started, 1, "the FIFO saturation job did not start");

    let order = Arc::new(Mutex::new(Vec::new()));
    let pending_order = order.clone();
    pool.submit(
        Box::new(move || lock_mutex(&pending_order).push(2)),
        None,
        None,
    )
    .unwrap();

    let cancelled_group = TaskGroupValue::new(&CancellationContext::default());
    let cancelled = cancelled_group.child_cancellation();
    let cancelled_pool = pool.clone();
    let cancelled_order = order.clone();
    let cancelled_join = thread::spawn(move || {
        cancelled_pool.submit(
            Box::new(move || lock_mutex(&cancelled_order).push(3)),
            None,
            Some(&cancelled),
        )
    });
    pool.wait_for_admission_waiters_for_test(1);

    let live_pool = pool.clone();
    let live_order = order.clone();
    let live_join = thread::spawn(move || {
        live_pool.submit(
            Box::new(move || lock_mutex(&live_order).push(4)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(2);
    cancelled_group.cancel();
    let error = cancelled_join
        .join()
        .expect("cancelled submitter should not panic")
        .expect_err("the oldest admission waiter should cancel");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);

    release.store(true, Ordering::SeqCst);
    live_join
        .join()
        .expect("live submitter should not panic")
        .expect("the remaining waiter should consume the released slot");
    pool.wait_until_idle_for_test();
    assert_eq!(*lock_mutex(&order), vec![2, 4]);
    assert_eq!(pool.admission_waiter_count(), 0);
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_admission_preserves_surviving_fifo_when_the_middle_deadline_expires() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let _release_on_unwind = AtomicReleaseGuard(release.clone());
    let started = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(
            started.clone(),
            release.clone(),
            Arc::new(AtomicUsize::new(0)),
        ),
        None,
        None,
    )
    .expect("the FIFO saturation job should be accepted");
    wait_for_count(
        &started,
        1,
        "the middle-deadline FIFO saturation job did not start",
    );

    let order = Arc::new(Mutex::new(Vec::new()));
    let pending_order = order.clone();
    pool.submit(
        Box::new(move || lock_mutex(&pending_order).push(1)),
        None,
        None,
    )
    .expect("the FIFO control job should occupy the pending slot");

    let oldest_pool = pool.clone();
    let oldest_order = order.clone();
    let oldest = thread::spawn(move || {
        oldest_pool.submit(
            Box::new(move || lock_mutex(&oldest_order).push(2)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(1);

    let middle_executions = Arc::new(AtomicUsize::new(0));
    let middle_counter = middle_executions.clone();
    let middle_pool = pool.clone();
    let middle = thread::spawn(move || {
        middle_pool.submit(
            Box::new(move || {
                middle_counter.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Instant::now() + StdDuration::from_millis(500)),
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(2);

    let youngest_pool = pool.clone();
    let youngest_order = order.clone();
    let youngest = thread::spawn(move || {
        youngest_pool.submit(
            Box::new(move || lock_mutex(&youngest_order).push(4)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(3);

    let error = middle
        .join()
        .expect("the middle admission waiter should not panic")
        .expect_err("the middle admission waiter should expire before acceptance");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(middle_executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        pool.admission_waiter_count(),
        2,
        "both live waiters should survive middle removal"
    );

    let barger_pool = pool.clone();
    let barger_order = order.clone();
    let barger = thread::spawn(move || {
        barger_pool.submit(
            Box::new(move || lock_mutex(&barger_order).push(5)),
            None,
            None,
        )
    });
    pool.wait_for_admission_waiters_for_test(3);
    release.store(true, Ordering::SeqCst);

    for (name, waiter) in [
        ("oldest", oldest),
        ("youngest", youngest),
        ("barger", barger),
    ] {
        waiter
            .join()
            .unwrap_or_else(|_| panic!("{name} admission waiter should not panic"))
            .unwrap_or_else(|error| panic!("{name} admission waiter should succeed: {error}"));
    }
    pool.wait_until_idle_for_test();
    assert_eq!(
        *lock_mutex(&order),
        vec![1, 2, 4, 5],
        "middle removal must preserve both surviving waiters ahead of a later barger"
    );
    assert_eq!(pool.admission_waiter_count(), 0);
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_operation_panics_are_observable_and_do_not_reduce_pool_size() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let error = super::run_blocking_io_with_deadline_on_pool(
        || -> io::Result<()> {
            panic!("injected blocking operation panic");
        },
        None,
        None,
        &pool,
    )
    .expect_err("a blocking operation panic must become an operation error");
    assert!(error
        .to_string()
        .contains("blocking I/O operation panicked: injected blocking operation panic"));

    pool.submit(
        Box::new(|| panic!("injected raw blocking job panic")),
        None,
        None,
    )
    .expect("the raw panic probe should enter the pool");
    assert_eq!(
        super::run_blocking_io_with_deadline_on_pool(|| Ok::<_, io::Error>(42), None, None, &pool,)
            .expect("the same worker should survive wrapped and raw job panics"),
        42
    );
    assert_eq!(pool.worker_start_count(), 1);
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_pool_shutdown_drains_accepted_jobs_and_cancels_parked_admission() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(started.clone(), release.clone(), completed.clone()),
        None,
        None,
    )
    .unwrap();
    wait_for_count(&started, 1, "the shutdown saturation job did not start");

    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_in_job = accepted.clone();
    pool.submit(
        Box::new(move || {
            accepted_in_job.fetch_add(1, Ordering::SeqCst);
        }),
        None,
        None,
    )
    .expect("the queued job should be accepted before shutdown");

    let waiting_pool = pool.clone();
    let waiting = thread::spawn(move || waiting_pool.submit(Box::new(|| {}), None, None));
    pool.wait_for_admission_waiters_for_test(1);
    let shutdown_pool = pool.clone();
    let shutdown = thread::spawn(move || shutdown_pool.shutdown_for_test());

    let error = waiting
        .join()
        .expect("the parked submitter should not panic")
        .expect_err("shutdown must cancel unaccepted admission");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    release.store(true, Ordering::SeqCst);
    shutdown.join().expect("pool shutdown should finish");
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    assert_eq!(pool.admission_waiter_count(), 0);
}

#[test]
fn blocking_io_submission_rejects_bounded_and_unbounded_pools_once_shutdown_begins() {
    for queue_capacity in [None, Some(1)] {
        let pool = BlockingIoPool::new(BlockingIoPoolConfig {
            worker_count: 1,
            queue_capacity,
        });
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_in_job = completed.clone();
        pool.submit(
            Box::new(move || {
                completed_in_job.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            None,
        )
        .expect("the lifecycle probe should start its worker before shutdown");
        wait_for_count(
            &completed,
            1,
            "the lifecycle probe did not complete before shutdown",
        );

        {
            let mut state = lock_mutex(&pool.state);
            assert!(!state.shutting_down);
            state.shutting_down = true;
        }
        let rejected_executions = Arc::new(AtomicUsize::new(0));
        let rejected_counter = rejected_executions.clone();
        let error = pool
            .submit(
                Box::new(move || {
                    rejected_counter.fetch_add(1, Ordering::SeqCst);
                }),
                None,
                None,
            )
            .expect_err("admission must close as soon as shutdown begins");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "blocking-I/O pool is shutting down");
        assert_eq!(rejected_executions.load(Ordering::SeqCst), 0);
        pool.shutdown_for_test();
    }

    let never_started = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    never_started.shutdown_for_test();
    never_started.shutdown_for_test();
    assert_eq!(never_started.worker_start_count(), 0);
}

#[test]
fn lightweight_blocking_io_admission_parks_without_starving_a_sibling_timer() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(started.clone(), release.clone(), completed),
        None,
        None,
    )
    .unwrap();
    wait_for_count(&started, 1, "the scheduler saturation job did not start");
    pool.submit(Box::new(|| {}), None, None)
        .expect("the one-slot pending queue should fill");

    let sibling_progressed = Arc::new(AtomicBool::new(false));
    let root_pool = pool.clone();
    let root_release = release.clone();
    let root_sibling_progressed = sibling_progressed.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let parked_pool = root_pool.clone();
        let parked = spawn_lightweight_task(move || {
            let result = super::run_blocking_io_with_deadline_on_pool(
                || Ok::<_, io::Error>(42),
                Some(Instant::now() + StdDuration::from_secs(1)),
                None,
                &parked_pool,
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            Ok(Value::Int(IntegerValue::from_signed(result)))
        })?;
        while root_pool.admission_waiter_count() == 0 {
            super::yield_now_with_runtime_scheduler();
        }

        let sibling_progress = root_sibling_progressed.clone();
        let sibling = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(5), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            sibling_progress.store(true, Ordering::SeqCst);
            Ok(Value::Unit)
        })?;
        wait_task_ready(&sibling)?;
        assert!(
            root_sibling_progressed.load(Ordering::SeqCst),
            "a full blocking-I/O queue must park admission and free the pinned worker"
        );
        root_release.store(true, Ordering::SeqCst);
        assert_eq!(
            wait_task_ready(&parked)?,
            Value::Int(IntegerValue::from_signed(42))
        );
        Ok(Value::Unit)
    });
    assert!(
        result.is_ok(),
        "scheduler-aware blocking-I/O admission should preserve sibling progress: {result:?}"
    );
    pool.wait_until_idle_for_test();
    pool.shutdown_for_test();
}

#[test]
fn unbounded_blocking_io_queue_accepts_without_admission_waiters() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: None,
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    pool.submit(
        gated_blocking_job(started.clone(), release.clone(), completed.clone()),
        None,
        None,
    )
    .unwrap();
    wait_for_count(&started, 1, "the unbounded control job did not start");

    for _ in 0..32 {
        let completed = completed.clone();
        pool.submit(
            Box::new(move || {
                completed.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Instant::now() + StdDuration::from_secs(1)),
            None,
        )
        .expect("an unbounded queue should not park a live submission");
    }
    assert_eq!(pool.admission_waiter_count(), 0);
    release.store(true, Ordering::SeqCst);
    wait_for_count(&completed, 33, "the unbounded control queue did not drain");
    pool.wait_until_idle_for_test();
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_slot_deadline_races_have_one_outcome_and_lose_no_capacity() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let saturation_starts = Arc::new(AtomicUsize::new(0));
    for iteration in 0..24 {
        let release = Arc::new(AtomicBool::new(false));
        pool.submit(
            gated_blocking_job(
                saturation_starts.clone(),
                release.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
            None,
            None,
        )
        .unwrap();
        wait_for_count(
            &saturation_starts,
            iteration + 1,
            "the race saturation job did not start",
        );
        pool.submit(Box::new(|| {}), None, None)
            .expect("the race control job should fill the queue");

        let contender_executions = Arc::new(AtomicUsize::new(0));
        let contender_counter = contender_executions.clone();
        let contender_pool = pool.clone();
        let deadline = Instant::now() + StdDuration::from_millis(3);
        let contender = thread::spawn(move || {
            contender_pool.submit(
                Box::new(move || {
                    contender_counter.fetch_add(1, Ordering::SeqCst);
                }),
                Some(deadline),
                None,
            )
        });
        let observation_deadline = Instant::now() + StdDuration::from_secs(2);
        while pool.admission_waiter_count() == 0 && !contender.is_finished() {
            assert!(
                Instant::now() < observation_deadline,
                "the deadline-race submitter neither registered nor completed"
            );
            thread::yield_now();
        }
        if pool.admission_waiter_count() != 0 {
            match iteration % 3 {
                0 => thread::sleep(StdDuration::from_millis(1)),
                1 => thread::sleep(StdDuration::from_millis(3)),
                _ => thread::sleep(StdDuration::from_millis(5)),
            }
        }
        release.store(true, Ordering::SeqCst);
        let admission = contender.join().expect("race submitter should not panic");
        pool.wait_until_idle_for_test();
        match admission {
            Ok(()) => assert_eq!(
                contender_executions.load(Ordering::SeqCst),
                1,
                "accepted race jobs must execute exactly once"
            ),
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                assert_eq!(
                    contender_executions.load(Ordering::SeqCst),
                    0,
                    "timed-out pre-acceptance race jobs must never execute"
                );
            }
        }
        assert_eq!(pool.admission_waiter_count(), 0);

        assert_eq!(
            super::run_blocking_io_with_deadline_on_pool(
                move || Ok::<_, io::Error>(iteration),
                None,
                None,
                &pool,
            )
            .expect("every race must restore capacity for unrelated work"),
            iteration
        );
    }
    pool.shutdown_for_test();
}

#[test]
fn blocking_io_slot_cancellation_races_have_one_outcome_and_lose_no_capacity() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let saturation_starts = Arc::new(AtomicUsize::new(0));
    for iteration in 0..24 {
        let release = Arc::new(AtomicBool::new(false));
        pool.submit(
            gated_blocking_job(
                saturation_starts.clone(),
                release.clone(),
                Arc::new(AtomicUsize::new(0)),
            ),
            None,
            None,
        )
        .unwrap();
        wait_for_count(
            &saturation_starts,
            iteration + 1,
            "the cancellation-race saturation job did not start",
        );
        pool.submit(Box::new(|| {}), None, None)
            .expect("the cancellation-race control job should fill the queue");

        let group = TaskGroupValue::new(&CancellationContext::default());
        let cancellation = group.child_cancellation();
        let contender_executions = Arc::new(AtomicUsize::new(0));
        let contender_counter = contender_executions.clone();
        let contender_pool = pool.clone();
        let contender = thread::spawn(move || {
            contender_pool.submit(
                Box::new(move || {
                    contender_counter.fetch_add(1, Ordering::SeqCst);
                }),
                None,
                Some(&cancellation),
            )
        });
        pool.wait_for_admission_waiters_for_test(1);
        match iteration % 3 {
            0 => {
                group.cancel();
                release.store(true, Ordering::SeqCst);
            }
            1 => {
                release.store(true, Ordering::SeqCst);
                thread::sleep(StdDuration::from_millis(1));
                group.cancel();
            }
            _ => {
                release.store(true, Ordering::SeqCst);
                group.cancel();
            }
        }
        let admission = contender.join().expect("race submitter should not panic");
        pool.wait_until_idle_for_test();
        match admission {
            Ok(()) => assert_eq!(
                contender_executions.load(Ordering::SeqCst),
                1,
                "accepted cancellation-race jobs must execute exactly once"
            ),
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                assert_eq!(
                    contender_executions.load(Ordering::SeqCst),
                    0,
                    "cancelled pre-acceptance race jobs must never execute"
                );
            }
        }
        assert_eq!(pool.admission_waiter_count(), 0);
        assert_eq!(
            super::run_blocking_io_with_deadline_on_pool(
                move || Ok::<_, io::Error>(iteration),
                None,
                None,
                &pool,
            )
            .expect("every cancellation race must restore unrelated capacity"),
            iteration
        );
    }
    pool.shutdown_for_test();
}

#[test]
fn injected_resolver_outage_saturation_recovers_without_executing_rejected_jobs() {
    let resolved: std::net::SocketAddr = "127.0.0.1:443"
        .parse()
        .expect("the injected resolver address should parse");
    for _ in 0..4 {
        let pool = BlockingIoPool::new(BlockingIoPoolConfig {
            worker_count: 2,
            queue_capacity: Some(1),
        });
        let release = Arc::new(AtomicBool::new(false));
        let running_started = Arc::new(AtomicUsize::new(0));
        let running_executions = Arc::new(AtomicUsize::new(0));
        let mut running = Vec::new();
        for _ in 0..2 {
            let resolver_pool = pool.clone();
            let resolver_release = release.clone();
            let resolver_started = running_started.clone();
            let resolver_executions = running_executions.clone();
            running.push(thread::spawn(move || {
                super::resolve_socket_addresses_before_on_pool_with(
                    "outage.injected:443",
                    None,
                    None,
                    &resolver_pool,
                    move |address| {
                        assert_eq!(address, "outage.injected:443");
                        resolver_started.fetch_add(1, Ordering::SeqCst);
                        while !resolver_release.load(Ordering::SeqCst) {
                            thread::yield_now();
                        }
                        resolver_executions.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![resolved])
                    },
                )
            }));
        }
        wait_for_count(
            &running_started,
            2,
            "both injected resolver workers should become occupied",
        );

        let accepted_late_executions = Arc::new(AtomicUsize::new(0));
        let accepted_late_counter = accepted_late_executions.clone();
        let accepted_pool = pool.clone();
        let accepted_late = thread::spawn(move || {
            super::resolve_socket_addresses_before_on_pool_with(
                "accepted-late.injected:443",
                Some(Instant::now() + StdDuration::from_millis(15)),
                None,
                &accepted_pool,
                move |_| {
                    accepted_late_counter.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![resolved])
                },
            )
        });
        let pending_deadline = Instant::now() + StdDuration::from_secs(2);
        while pool.pending_job_count() != 1 {
            assert!(
                Instant::now() < pending_deadline,
                "the accepted late resolver job should fill the pending queue"
            );
            thread::yield_now();
        }
        let error = accepted_late
            .join()
            .expect("the accepted resolver caller should not panic")
            .expect_err("the accepted pending resolver wait should time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(accepted_late_executions.load(Ordering::SeqCst), 0);

        let rejected_timeout_executions = Arc::new(AtomicUsize::new(0));
        let rejected_timeout_counter = rejected_timeout_executions.clone();
        let error = super::resolve_socket_addresses_before_on_pool_with(
            "rejected-timeout.injected:443",
            Some(Instant::now() + StdDuration::from_millis(10)),
            None,
            &pool,
            move |_| {
                rejected_timeout_counter.fetch_add(1, Ordering::SeqCst);
                Ok(vec![resolved])
            },
        )
        .expect_err("a resolver request outside the full queue should time out before acceptance");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(rejected_timeout_executions.load(Ordering::SeqCst), 0);

        let group = TaskGroupValue::new(&CancellationContext::default());
        let cancellation = group.child_cancellation();
        let rejected_cancel_executions = Arc::new(AtomicUsize::new(0));
        let rejected_cancel_counter = rejected_cancel_executions.clone();
        let cancelled_pool = pool.clone();
        let cancelled = thread::spawn(move || {
            super::resolve_socket_addresses_before_on_pool_with(
                "rejected-cancel.injected:443",
                None,
                Some(&cancellation),
                &cancelled_pool,
                move |_| {
                    rejected_cancel_counter.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![resolved])
                },
            )
        });
        pool.wait_for_admission_waiters_for_test(1);
        group.cancel();
        let error = cancelled
            .join()
            .expect("the cancelled resolver caller should not panic")
            .expect_err("full-queue resolver admission should observe cancellation");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(rejected_cancel_executions.load(Ordering::SeqCst), 0);

        let unrelated_pool = pool.clone();
        let unrelated = thread::spawn(move || {
            super::run_blocking_io_with_deadline_on_pool(
                || {
                    std::fs::metadata(".")?;
                    Ok::<_, io::Error>(())
                },
                Some(Instant::now() + StdDuration::from_secs(1)),
                None,
                &unrelated_pool,
            )
        });
        pool.wait_for_admission_waiters_for_test(1);
        release.store(true, Ordering::SeqCst);
        for resolver in running {
            assert_eq!(
                resolver
                    .join()
                    .expect("running resolver should not panic")
                    .expect("running resolver should drain"),
                vec![resolved]
            );
        }
        unrelated
            .join()
            .expect("unrelated filesystem caller should not panic")
            .expect("unrelated filesystem work should complete after resolver release");
        pool.wait_until_idle_for_test();

        assert_eq!(running_executions.load(Ordering::SeqCst), 2);
        assert_eq!(accepted_late_executions.load(Ordering::SeqCst), 1);
        assert_eq!(rejected_timeout_executions.load(Ordering::SeqCst), 0);
        assert_eq!(rejected_cancel_executions.load(Ordering::SeqCst), 0);
        assert_eq!(pool.admission_waiter_count(), 0);
        pool.shutdown_for_test();
    }
}

#[test]
fn tcp_combined_resolve_connect_adapter_recovers_queued_work_after_resolver_saturation() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the TCP saturation probe should bind a loopback listener");
    let candidate = listener
        .local_addr()
        .expect("the TCP saturation listener should expose its address");
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 2,
        queue_capacity: Some(1),
    });
    let outage_gates = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let _release_on_unwind = outage_gates
        .iter()
        .cloned()
        .map(AtomicReleaseGuard)
        .collect::<Vec<_>>();
    let outage_started = Arc::new(AtomicUsize::new(0));
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
    let mut callers = Vec::new();

    for (caller_id, gate) in outage_gates.iter().cloned().enumerate() {
        let caller_pool = pool.clone();
        let caller_started = outage_started.clone();
        let caller_outcome = outcome_tx.clone();
        callers.push(thread::spawn(move || {
            let result = TcpStreamValue::connect_with_operations_on_pool(
                "resolver-outage.injected:443",
                None,
                None,
                &caller_pool,
                move |address| {
                    assert_eq!(address, "resolver-outage.injected:443");
                    caller_started.fetch_add(1, Ordering::SeqCst);
                    while !gate.load(Ordering::SeqCst) {
                        thread::yield_now();
                    }
                    Ok(vec![candidate])
                },
                |candidate, timeout| match timeout {
                    Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                    None => std::net::TcpStream::connect(candidate),
                },
            );
            caller_outcome
                .send((caller_id, result))
                .expect("the TCP saturation outcome receiver should remain live");
        }));
    }
    wait_for_count(
        &outage_started,
        2,
        "both blocking workers should be occupied by injected TCP resolution",
    );

    let queued_resolver_executions = Arc::new(AtomicUsize::new(0));
    let queued_counter = queued_resolver_executions.clone();
    let queued_pool = pool.clone();
    let queued_outcome = outcome_tx.clone();
    callers.push(thread::spawn(move || {
        let result = TcpStreamValue::connect_with_operations_on_pool(
            "queued-combined.injected:443",
            None,
            None,
            &queued_pool,
            move |address| {
                assert_eq!(address, "queued-combined.injected:443");
                queued_counter.fetch_add(1, Ordering::SeqCst);
                Ok(vec![candidate])
            },
            |candidate, timeout| match timeout {
                Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                None => std::net::TcpStream::connect(candidate),
            },
        );
        queued_outcome
            .send((2, result))
            .expect("the queued TCP outcome receiver should remain live");
    }));
    let pending_deadline = Instant::now() + StdDuration::from_secs(2);
    while pool.pending_job_count() != 1 {
        assert!(
            Instant::now() < pending_deadline,
            "the third combined TCP adapter should occupy the pending slot"
        );
        thread::yield_now();
    }

    let unrelated_resolver_executions = Arc::new(AtomicUsize::new(0));
    let unrelated_counter = unrelated_resolver_executions.clone();
    let unrelated_pool = pool.clone();
    let unrelated_outcome = outcome_tx.clone();
    callers.push(thread::spawn(move || {
        let result = TcpStreamValue::connect_with_operations_on_pool(
            "unrelated-combined.injected:443",
            None,
            None,
            &unrelated_pool,
            move |address| {
                assert_eq!(address, "unrelated-combined.injected:443");
                unrelated_counter.fetch_add(1, Ordering::SeqCst);
                Ok(vec![candidate])
            },
            |candidate, timeout| match timeout {
                Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                None => std::net::TcpStream::connect(candidate),
            },
        );
        unrelated_outcome
            .send((3, result))
            .expect("the unrelated TCP outcome receiver should remain live");
    }));
    pool.wait_for_admission_waiters_for_test(1);
    assert_eq!(pool.active_job_count(), 2);
    assert_eq!(pool.pending_job_count(), 1);

    outage_gates[0].store(true, Ordering::SeqCst);
    let mut recovered_callers = Vec::new();
    for _ in 0..3 {
        let (caller_id, result) = outcome_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("one released worker should drain queued and unrelated TCP adapters");
        let stream = result.expect("every recovered loopback TCP adapter should connect");
        stream.close();
        recovered_callers.push(caller_id);
    }
    recovered_callers.sort_unstable();
    assert_eq!(
        recovered_callers,
        vec![0, 2, 3],
        "queued and unrelated combined adapters must recover while one resolver remains stuck"
    );
    assert_eq!(queued_resolver_executions.load(Ordering::SeqCst), 1);
    assert_eq!(unrelated_resolver_executions.load(Ordering::SeqCst), 1);
    assert!(
        matches!(
            outcome_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "the unreleased resolver must still occupy its worker"
    );

    outage_gates[1].store(true, Ordering::SeqCst);
    let (caller_id, result) = outcome_rx
        .recv_timeout(StdDuration::from_secs(2))
        .expect("the final TCP resolver should complete after release");
    assert_eq!(caller_id, 1);
    result
        .expect("the final loopback TCP adapter should connect")
        .close();
    for caller in callers {
        caller
            .join()
            .expect("a combined TCP saturation caller should not panic");
    }
    pool.wait_until_idle_for_test();
    assert_eq!(pool.admission_waiter_count(), 0);
    pool.shutdown_for_test();
}

#[test]
fn accepted_blocking_io_timeout_discards_the_late_result_and_pool_recovers() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let run_release = release.clone();
    let run_started = started.clone();
    let run_executions = executions.clone();
    let error = super::run_blocking_io_with_deadline_on_pool(
        move || {
            run_started.fetch_add(1, Ordering::SeqCst);
            while !run_release.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            run_executions.fetch_add(1, Ordering::SeqCst);
            Ok::<_, io::Error>(41)
        },
        Some(Instant::now() + StdDuration::from_millis(20)),
        None,
        &pool,
    )
    .expect_err("an accepted operation may outlive its caller deadline");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    wait_for_count(&started, 1, "the accepted operation never started");
    release.store(true, Ordering::SeqCst);
    wait_for_count(
        &executions,
        1,
        "the abandoned operation did not execute exactly once",
    );

    let recovered =
        super::run_blocking_io_with_deadline_on_pool(|| Ok::<_, io::Error>(42), None, None, &pool)
            .expect("unrelated work should complete after the accepted outage job drains");
    assert_eq!(recovered, 42);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    pool.shutdown_for_test();
}

#[test]
fn accepted_blocking_io_cancellation_discards_the_late_result_and_pool_recovers() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let caller_pool = pool.clone();
    let caller_release = release.clone();
    let caller_started = started.clone();
    let caller_executions = executions.clone();
    let caller = thread::spawn(move || {
        super::run_blocking_io_with_deadline_on_pool(
            move || {
                caller_started.fetch_add(1, Ordering::SeqCst);
                while !caller_release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                caller_executions.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(41)
            },
            None,
            Some(&cancellation),
            &caller_pool,
        )
    });
    wait_for_count(&started, 1, "the cancellation operation never started");
    group.cancel();
    let error = caller
        .join()
        .expect("the cancelled caller should not panic")
        .expect_err("an accepted operation may outlive caller cancellation");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    release.store(true, Ordering::SeqCst);
    wait_for_count(
        &executions,
        1,
        "the cancelled accepted operation did not execute exactly once",
    );

    assert_eq!(
        super::run_blocking_io_with_deadline_on_pool(|| Ok::<_, io::Error>(42), None, None, &pool,)
            .expect("unrelated work should complete after cancelled accepted work drains"),
        42
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    pool.shutdown_for_test();
}

#[test]
fn abandoned_blocking_io_discards_late_host_errors_and_panics_then_recovers() {
    let pool = BlockingIoPool::new(BlockingIoPoolConfig {
        worker_count: 1,
        queue_capacity: Some(1),
    });

    let error_release = Arc::new(AtomicBool::new(false));
    let error_started = Arc::new(AtomicUsize::new(0));
    let error_finished = Arc::new(AtomicUsize::new(0));
    let operation_release = error_release.clone();
    let operation_started = error_started.clone();
    let operation_finished = error_finished.clone();
    let error = super::run_blocking_io_with_deadline_on_pool(
        move || {
            operation_started.fetch_add(1, Ordering::SeqCst);
            while !operation_release.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            operation_finished.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(io::Error::other("late injected host error"))
        },
        Some(Instant::now() + StdDuration::from_millis(15)),
        None,
        &pool,
    )
    .expect_err("the caller should observe its deadline, not a later host error");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    wait_for_count(&error_started, 1, "the late-error operation never started");
    error_release.store(true, Ordering::SeqCst);
    wait_for_count(
        &error_finished,
        1,
        "the abandoned late-error operation did not finish",
    );
    assert_eq!(
        super::run_blocking_io_with_deadline_on_pool(
            || Ok::<_, io::Error>("after-error"),
            None,
            None,
            &pool,
        )
        .expect("late host errors must not poison the pool"),
        "after-error"
    );

    let panic_release = Arc::new(AtomicBool::new(false));
    let panic_started = Arc::new(AtomicUsize::new(0));
    let panic_finished = Arc::new(AtomicUsize::new(0));
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    let caller_pool = pool.clone();
    let operation_release = panic_release.clone();
    let operation_started = panic_started.clone();
    let operation_finished = panic_finished.clone();
    let caller = thread::spawn(move || {
        super::run_blocking_io_with_deadline_on_pool(
            move || -> io::Result<()> {
                operation_started.fetch_add(1, Ordering::SeqCst);
                while !operation_release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                operation_finished.fetch_add(1, Ordering::SeqCst);
                panic!("late injected host panic");
            },
            None,
            Some(&cancellation),
            &caller_pool,
        )
    });
    wait_for_count(&panic_started, 1, "the late-panic operation never started");
    group.cancel();
    let error = caller
        .join()
        .expect("the cancelled late-panic caller should not panic")
        .expect_err("the caller should observe cancellation, not a later panic");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    panic_release.store(true, Ordering::SeqCst);
    wait_for_count(
        &panic_finished,
        1,
        "the abandoned late-panic operation did not finish",
    );
    assert_eq!(
        super::run_blocking_io_with_deadline_on_pool(
            || Ok::<_, io::Error>("after-panic"),
            None,
            None,
            &pool,
        )
        .expect("late host panics must not poison the pool"),
        "after-panic"
    );
    pool.shutdown_for_test();
}

#[test]
fn tcp_connect_offloads_slow_resolution_without_starving_a_sibling_timer() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("loopback listener should bind for the injected resolver");
    let candidate = listener
        .local_addr()
        .expect("loopback listener should report its address");
    let resolver_finished = Arc::new(AtomicBool::new(false));
    let task_resolver_finished = resolver_finished.clone();

    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let connect = spawn_lightweight_task(move || {
            let resolver_finished = task_resolver_finished.clone();
            let stream = TcpStreamValue::connect_with_operations(
                "slow.injected.test:443",
                Some(StdDuration::from_secs(1)),
                None,
                move |_address| {
                    thread::sleep(crate::hosted_ci_timing_limit(StdDuration::from_millis(100)));
                    resolver_finished.store(true, Ordering::SeqCst);
                    Ok(vec![candidate])
                },
                |candidate, timeout| match timeout {
                    Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                    None => std::net::TcpStream::connect(candidate),
                },
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            stream.close();
            Ok(Value::Unit)
        })?;

        let sibling_resolver_finished = resolver_finished.clone();
        let sibling = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(10), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            if sibling_resolver_finished.load(Ordering::SeqCst) {
                return Err(Diagnostic::new(
                    "slow DNS resolution blocked the sibling timer",
                ));
            }
            Ok(Value::Unit)
        })?;

        wait_task_ready(&sibling)?;
        wait_task_ready(&connect)?;
        Ok(Value::Unit)
    });

    assert!(
        result.is_ok(),
        "slow resolution should yield the lightweight scheduler: {result:?}"
    );
}

#[test]
fn tcp_connect_timeout_budget_includes_resolution_wait() {
    let started = Instant::now();
    let result = run_lightweight_root_task(move || {
        let error = TcpStreamValue::connect_with_operations(
            "slow.injected.test:443",
            Some(StdDuration::from_millis(20)),
            None,
            |_address| {
                thread::sleep(crate::hosted_ci_timing_limit(StdDuration::from_millis(150)));
                Ok(vec!["127.0.0.1:9"
                    .parse()
                    .expect("test address should parse")])
            },
            |candidate, timeout| match timeout {
                Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                None => std::net::TcpStream::connect(candidate),
            },
        )
        .expect_err("resolution should consume the whole connect timeout budget");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        Ok(Value::Unit)
    });

    assert!(result.is_ok(), "timeout path should complete: {result:?}");
    assert!(
        started.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(100)),
        "connect timeout must not restart after DNS; elapsed {:?}",
        started.elapsed()
    );
}

#[test]
fn tcp_connect_timeout_offloads_resolution_without_a_lightweight_task_context() {
    let started = Instant::now();
    let error = TcpStreamValue::connect_with_operations(
        "slow.host-entry.test:443",
        Some(StdDuration::from_millis(20)),
        None,
        |_address| {
            thread::sleep(crate::hosted_ci_timing_limit(StdDuration::from_millis(150)));
            Ok(vec!["127.0.0.1:9"
                .parse()
                .expect("test address should parse")])
        },
        |candidate, timeout| match timeout {
            Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
            None => std::net::TcpStream::connect(candidate),
        },
    )
    .expect_err("host-entry resolution should honor its timeout");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        started.elapsed() < crate::hosted_ci_timing_limit(StdDuration::from_millis(100)),
        "host-entry DNS must use the blocking service; elapsed {:?}",
        started.elapsed()
    );
}

#[test]
fn tcp_connect_reports_empty_resolution_with_the_original_address() {
    let error = TcpStreamValue::connect_with_operations(
        "empty.injected.test:443",
        Some(StdDuration::from_secs(1)),
        None,
        |_address| Ok(Vec::new()),
        |_candidate, _timeout| -> io::Result<std::net::TcpStream> {
            panic!("an empty resolution must not attempt a connection")
        },
    )
    .expect_err("empty DNS results should be rejected");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(
        error
            .to_string()
            .contains("`empty.injected.test:443` did not resolve"),
        "empty-resolution diagnostics should retain the requested address: {error}"
    );
}

#[test]
fn tcp_connect_cancellation_stops_waiting_for_an_inflight_resolver() {
    let result = run_lightweight_root_task(move || {
        let group = TaskGroupValue::new(&CancellationContext::default());
        let cancellation = group.child_cancellation();
        let task_cancellation = cancellation.clone();
        let resolver_started = ChannelValue::new();
        let task_resolver_started = resolver_started.clone();
        let (release_resolver, wait_for_release) = std::sync::mpsc::channel();

        let connect = spawn_lightweight_task_with_cancellation(cancellation, move || {
            let error = TcpStreamValue::connect_with_operations(
                "cancelled.injected.test:443",
                None,
                Some(&task_cancellation),
                move |_address| {
                    task_resolver_started
                        .send(Value::Unit)
                        .expect("resolver-start signal should remain open");
                    wait_for_release
                        .recv()
                        .expect("test should release the resolver worker");
                    Err(io::Error::other("resolver released after cancellation"))
                },
                |candidate, timeout| match timeout {
                    Some(timeout) => std::net::TcpStream::connect_timeout(&candidate, timeout),
                    None => std::net::TcpStream::connect(candidate),
                },
            )
            .expect_err("cancellation should end the scheduler wait");
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(Diagnostic::new(format!(
                    "expected Interrupted, found {:?}",
                    error.kind()
                )));
            }
            Ok(Value::Unit)
        })?;

        resolver_started
            .recv_with_cancellation(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
            .ok_or_else(|| Diagnostic::new("resolver worker did not start"))?;
        group.cancel();
        let connect_result = wait_task_ready(&connect);
        release_resolver
            .send(())
            .map_err(|_| Diagnostic::new("resolver worker stopped before release"))?;
        connect_result?;
        Ok(Value::Unit)
    });

    assert!(
        result.is_ok(),
        "connect cancellation should be prompt and memory-safe: {result:?}"
    );
}

#[test]
fn blocking_service_cancellation_drops_late_results_safely() {
    #[derive(Debug)]
    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let result = run_lightweight_root_task({
        let dropped = dropped.clone();
        move || {
            let group = TaskGroupValue::new(&CancellationContext::default());
            let cancellation = group.child_cancellation();
            let task_cancellation = cancellation.clone();
            let operation_started = ChannelValue::new();
            let task_operation_started = operation_started.clone();
            let (release_operation, wait_for_release) = std::sync::mpsc::channel();

            let operation = spawn_lightweight_task_with_cancellation(cancellation, move || {
                let error = super::run_blocking_io_with_deadline(
                    move || {
                        task_operation_started
                            .send(Value::Unit)
                            .expect("operation-start signal should remain open");
                        wait_for_release
                            .recv()
                            .expect("test should release the blocking operation");
                        Ok(DropProbe(dropped))
                    },
                    None,
                    Some(&task_cancellation),
                )
                .expect_err("cancellation should abandon the blocking result");
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(Diagnostic::new(format!(
                        "expected Interrupted, found {:?}",
                        error.kind()
                    )));
                }
                Ok(Value::Unit)
            })?;

            operation_started
                .recv_with_cancellation(Some(StdDuration::from_secs(1)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
                .ok_or_else(|| Diagnostic::new("blocking operation did not start"))?;
            group.cancel();
            wait_task_ready(&operation)?;
            release_operation
                .send(())
                .map_err(|_| Diagnostic::new("blocking operation stopped before release"))?;
            Ok(Value::Unit)
        }
    });

    assert!(
        result.is_ok(),
        "late-result cancellation path should complete: {result:?}"
    );
    let deadline = Instant::now() + StdDuration::from_secs(1);
    while !dropped.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(StdDuration::from_millis(1));
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "a result produced after cancellation must be dropped by the worker"
    );
}

#[test]
fn tcp_connect_candidates_share_one_timeout_budget() {
    let started = Instant::now();
    let deadline = started + StdDuration::from_millis(100);
    let first: std::net::SocketAddr = "127.0.0.1:1".parse().expect("address should parse");
    let second: std::net::SocketAddr = "127.0.0.1:2".parse().expect("address should parse");
    let mut clock = [
        started,
        started + StdDuration::from_millis(40),
        started + StdDuration::from_millis(40),
    ]
    .into_iter();
    let mut observed_budgets = Vec::new();
    let mut attempts = 0;

    let connected = super::connect_resolved_tcp_candidates_with_clock(
        "injected.test:443",
        vec![first, second],
        Some(deadline),
        || {
            clock
                .next()
                .expect("test clock should cover every observation")
        },
        |_candidate, timeout| {
            observed_budgets.push(timeout.expect("deadline should produce a candidate budget"));
            attempts += 1;
            if attempts == 1 {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "first candidate refused",
                ))
            } else {
                Ok("connected")
            }
        },
    )
    .expect("the second candidate should connect within the shared budget");

    assert_eq!(connected, "connected");
    assert_eq!(
        observed_budgets,
        vec![StdDuration::from_millis(100), StdDuration::from_millis(60)],
        "each candidate must receive only the timeout budget that remains"
    );
}

#[cfg(unix)]
#[test]
fn unix_connect_offloads_a_slow_connect_without_starving_a_sibling_timer() {
    let connect_finished = Arc::new(AtomicBool::new(false));
    let task_connect_finished = connect_finished.clone();
    let result = super::run_lightweight_root_task_with_worker_count(1, move || {
        let connect = spawn_lightweight_task(move || {
            let connect_finished = task_connect_finished.clone();
            let stream = UnixStreamValue::connect_with_operation(
                "/tmp/injected-slow-connect.sock",
                Some(StdDuration::from_secs(1)),
                None,
                move |_path| {
                    thread::sleep(StdDuration::from_millis(100));
                    let (stream, peer) = std::os::unix::net::UnixStream::pair()?;
                    drop(peer);
                    connect_finished.store(true, Ordering::SeqCst);
                    Ok(stream)
                },
            )
            .map_err(|error| Diagnostic::new(error.to_string()))?;
            stream.close();
            Ok(Value::Unit)
        })?;

        let sibling_connect_finished = connect_finished.clone();
        let sibling = spawn_lightweight_task(move || {
            sleep_with_runtime_scheduler(StdDuration::from_millis(10), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?;
            if sibling_connect_finished.load(Ordering::SeqCst) {
                return Err(Diagnostic::new(
                    "slow Unix connect blocked the sibling timer",
                ));
            }
            Ok(Value::Unit)
        })?;

        wait_task_ready(&sibling)?;
        wait_task_ready(&connect)?;
        Ok(Value::Unit)
    });

    assert!(
        result.is_ok(),
        "Unix connect should yield the lightweight scheduler: {result:?}"
    );
}

#[test]
fn fixed_resource_caps_are_distinct_and_enforced_without_large_allocations() {
    assert_eq!(MAX_FILESYSTEM_READ_BYTES, 256 * 1024 * 1024);
    assert_eq!(MAX_STREAM_READ_BYTES, 64 * 1024 * 1024);
    assert_eq!(super::MAX_HTTP_MESSAGE_BYTES, 16 * 1024 * 1024);

    let mut at_limit = io::Cursor::new(b"abc".to_vec());
    assert_eq!(
        super::read_all_from_reader_with_limit(&mut at_limit, "test stream", 3)
            .expect("a read exactly at an injected limit should succeed"),
        b"abc"
    );
    let mut over_limit = io::Cursor::new(b"abcd".to_vec());
    let error = super::read_all_from_reader_with_limit(&mut over_limit, "test stream", 3)
        .expect_err("an injected read limit should reject one extra byte");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("3 bytes"));

    let mut pushed = b"ab".to_vec();
    super::push_limited_bytes_with_limit(&mut pushed, b"c", "test stream", 3)
        .expect("appending exactly to an injected stream limit should succeed");
    assert_eq!(pushed, b"abc");
    let error = super::push_limited_bytes_with_limit(&mut pushed, b"d", "test stream", 3)
        .expect_err("appending one byte beyond an injected stream limit should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("3 bytes"));

    let temp = TempDir::new("aura-read-all-limit");
    let remaining_path = temp.path().join("remaining.txt");
    fs::write(&remaining_path, b"abcd").expect("remaining-content test file should be written");
    let mut remaining_file = fs::File::open(&remaining_path).expect("test file should open");
    let error = super::validate_regular_file_remaining_size(&mut remaining_file, "file read", 3)
        .expect_err("four remaining bytes should exceed an injected three-byte limit");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    remaining_file
        .seek(SeekFrom::Start(1))
        .expect("test cursor should seek");
    super::validate_regular_file_remaining_size(&mut remaining_file, "file read", 3)
        .expect("three remaining bytes should meet an injected three-byte limit");

    let raised_cap_path = temp.path().join("above-stream-cap.txt");
    fs::File::create(&raised_cap_path)
        .expect("raised-cap test file should be created")
        .set_len((MAX_STREAM_READ_BYTES + 1) as u64)
        .expect("raised-cap test file should be extended");
    let mut raised_cap_file = fs::File::open(&raised_cap_path).expect("test file should open");
    super::validate_regular_file_remaining_size(
        &mut raised_cap_file,
        "filesystem read",
        MAX_FILESYSTEM_READ_BYTES,
    )
    .expect("filesystem reads above 64 MiB but below 256 MiB should pass preflight");

    let file_path = temp.path().join("above-filesystem-cap.txt");
    fs::File::create(&file_path)
        .expect("oversized test file should be created")
        .set_len((MAX_FILESYSTEM_READ_BYTES + 1) as u64)
        .expect("oversized test file should be extended");

    let file = FileValue::open(file_path.to_str().expect("utf-8 path")).expect("file should open");
    let error = file.read_all().expect_err("oversized read_all should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&MAX_FILESYSTEM_READ_BYTES.to_string()));

    let tls_path = temp.path().join("above-tls-config-cap.pem");
    fs::File::create(&tls_path)
        .expect("oversized TLS test file should be created")
        .set_len((MAX_STREAM_READ_BYTES + 1) as u64)
        .expect("oversized TLS test file should be extended");
    let error = super::read_tls_config_file(
        tls_path.to_str().expect("TLS test path should be UTF-8"),
        "TLS test PEM",
    )
    .expect_err("TLS configuration files must retain the 64 MiB cap");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&MAX_STREAM_READ_BYTES.to_string()));
}

#[test]
fn http_request_rejects_control_characters_in_headers() {
    let error = HttpResponseValue::request_text(
        "GET",
        "http://127.0.0.1:1/test",
        "",
        vec![("X-Test".to_string(), "safe\r\nX-Evil: injected".to_string())],
        Some(StdDuration::from_secs(1)),
        Some(&CancellationContext::default()),
    )
    .expect_err("request headers with CRLF should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn http_request_rejects_invalid_header_names_and_non_ascii_values() {
    let bad_name = HttpResponseValue::request_text(
        "GET",
        "http://127.0.0.1:1/test",
        "",
        vec![("Bad(Name)".to_string(), "value".to_string())],
        Some(StdDuration::from_secs(1)),
        Some(&CancellationContext::default()),
    )
    .expect_err("request headers with invalid token characters should be rejected");
    assert_eq!(bad_name.kind(), std::io::ErrorKind::InvalidInput);

    let bad_value = HttpResponseValue::request_text(
        "GET",
        "http://127.0.0.1:1/test",
        "",
        vec![("X-Test".to_string(), "caf\u{00e9}".to_string())],
        Some(StdDuration::from_secs(1)),
        Some(&CancellationContext::default()),
    )
    .expect_err("request headers with non-ASCII values should be rejected");
    assert_eq!(bad_value.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn http_content_length_validation_rejects_non_identity_transfer_coding() {
    let error = super::parse_http_content_length(&[(
        "Transfer-Encoding".to_string(),
        "chunked".to_string(),
    )])
    .expect_err("content-length validation must reject unsupported transfer coding");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "Aura HTTP currently does not support transfer-encoding other than identity"
    );
}

#[cfg(unix)]
#[test]
fn tls_and_websocket_validation_fail_before_remote_protocol_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the local TLS validation listener should bind");
    let address = listener
        .local_addr()
        .expect("the local TLS validation listener should have an address");
    let accepted = thread::spawn(move || {
        let (_stream, _) = listener
            .accept()
            .expect("the validation connection should reach the local listener");
    });
    let tls_error = TlsStreamValue::connect(
        &address.to_string(),
        "not a valid TLS server name",
        None,
        Some(StdDuration::from_secs(2)),
        None,
    )
    .expect_err("an invalid TLS server name must fail before a handshake is attempted");
    accepted
        .join()
        .expect("the local TLS validation listener should finish");
    assert_eq!(tls_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(tls_error.to_string(), "invalid TLS server name");

    let websocket_error = super::WebSocketValue::connect(
        "custom://example.com/socket",
        Some(StdDuration::from_secs(1)),
    )
    .expect_err("a websocket URL without a known scheme port must fail before resolution");
    assert_eq!(websocket_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        websocket_error.to_string(),
        "websocket URL is missing a known port"
    );
}

#[cfg(unix)]
#[test]
fn pending_tls_listener_progress_uses_a_bounded_default_poll() {
    let (reader, _writer) = std::os::unix::net::UnixStream::pair()
        .expect("the local readiness probe should create a socket pair");
    super::wait_for_tls_listener_progress(reader.as_raw_fd(), false, None, None)
        .expect("a pending handshake without a caller deadline should yield after a bounded poll");
}

#[test]
fn http_helper_parsing_covers_reason_phrases_and_header_errors() {
    let reason_cases = [
        (100, "Continue"),
        (101, "Switching Protocols"),
        (200, "OK"),
        (201, "Created"),
        (202, "Accepted"),
        (204, "No Content"),
        (301, "Moved Permanently"),
        (302, "Found"),
        (304, "Not Modified"),
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (405, "Method Not Allowed"),
        (408, "Request Timeout"),
        (409, "Conflict"),
        (413, "Payload Too Large"),
        (415, "Unsupported Media Type"),
        (426, "Upgrade Required"),
        (429, "Too Many Requests"),
        (431, "Request Header Fields Too Large"),
        (500, "Internal Server Error"),
        (501, "Not Implemented"),
        (502, "Bad Gateway"),
        (503, "Service Unavailable"),
        (504, "Gateway Timeout"),
        (599, ""),
    ];
    for (status, expected) in reason_cases {
        assert_eq!(super::http_reason_phrase(status), expected);
    }

    assert!(super::parse_http_response_head(b"HTTP/1.1 200")
        .expect("partial response head should parse")
        .is_none());
    assert!(super::parse_http_request_head(b"GET / HTTP/1.1")
        .expect("partial request head should parse")
        .is_none());

    let (_, status, reason, headers, framing) =
        super::parse_http_response_head(b"HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\n\r\n")
            .expect("response head should parse")
            .expect("response head should be complete");
    assert_eq!(status, 202);
    assert_eq!(reason, "Accepted");
    assert_eq!(
        headers,
        vec![("Content-Length".to_string(), "2".to_string())]
    );
    assert_eq!(framing, super::HttpBodyFraming::ContentLength(2));

    let (_, status, reason, headers, framing) =
        super::parse_http_response_head(b"HTTP/1.1 204\r\n\r\n")
            .expect("response head without explicit reason should parse")
            .expect("response head should be complete");
    assert_eq!(status, 204);
    assert_eq!(reason, "");
    assert!(headers.is_empty());
    assert_eq!(framing, super::HttpBodyFraming::UntilClose);

    let (_, method, path, headers, framing) = super::parse_http_request_head(
        b"POST /submit HTTP/1.1\r\nHost: example.test\r\nContent-Length: 0\r\n\r\n",
    )
    .expect("request head should parse")
    .expect("request head should be complete");
    assert_eq!(method, "POST");
    assert_eq!(path, "/submit");
    assert_eq!(headers[0], ("Host".to_string(), "example.test".to_string()));
    assert_eq!(framing, super::HttpBodyFraming::ContentLength(0));

    let error = super::parse_http_response_head(b"HTTP/1.1 200 OK\r\nX-Bad: \xff\r\n\r\n")
        .expect_err("non-UTF-8 header values should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let error = super::parse_http_response_head(b"HTTP/1.1 nope\r\n\r\n")
        .expect_err("invalid response heads should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let error = super::parse_http_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: nope\r\n\r\n")
        .expect_err("invalid content-length should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    assert!(super::is_http_bad_request_error(&io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed request"
    )));
    assert!(super::is_http_bad_request_error(&io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "truncated request"
    )));
    assert!(!super::is_http_bad_request_error(&io::Error::new(
        io::ErrorKind::Other,
        "network failure"
    )));
    assert!(!super::is_http_bad_request_error(
        &super::http_message_too_large_error()
    ));
    assert!(!super::is_http_bad_request_error(
        &super::http_headers_too_large_error()
    ));

    let conflict = vec![
        ("content-length".to_string(), "1".to_string()),
        ("Content-Length".to_string(), "2".to_string()),
    ];
    let error = super::parse_http_content_length(&conflict)
        .expect_err("direct conflicting content-length values should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let invalid = vec![("Content-Length".to_string(), "not-a-number".to_string())];
    let error = super::parse_http_content_length(&invalid)
        .expect_err("direct invalid content-length values should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("invalid HTTP content length `not-a-number`"),
        "the parser should preserve the rejected header value: {error}"
    );

    let error = super::parse_http_response_head(
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
    )
    .expect_err("conflicting content-length values should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let error =
        super::parse_http_request_head(b"GET / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n")
            .expect_err("unsupported transfer-encoding should fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let equivalent_lengths = vec![
        ("Content-Length".to_string(), "2".to_string()),
        ("content-length".to_string(), "2".to_string()),
        ("Transfer-Encoding".to_string(), "identity".to_string()),
    ];
    assert_eq!(
        super::parse_http_content_length(&equivalent_lengths)
            .expect("matching content-length headers should be accepted"),
        Some(2)
    );

    let mut oversized = vec![0; 8];
    let error = super::push_http_chunk_with_limit(&mut oversized, &[1], 8)
        .expect_err("oversized HTTP buffers should be rejected before extending");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn chunked_http_framing_rejects_ambiguous_malformed_and_oversized_inputs() {
    use super::HttpBodyFraming;

    const TEST_HTTP_LIMIT: usize = 32;

    assert_eq!(super::find_http_crlf(b"aa\r\nbb", 0), Some(2));
    assert_eq!(super::find_http_crlf(b"aa\r\nbb", 4), None);
    assert_eq!(super::find_http_crlf(b"aa", 3), None);

    let matching_lengths = vec![
        ("Content-Length".to_string(), "2".to_string()),
        ("content-length".to_string(), "2".to_string()),
    ];
    assert_eq!(
        super::parse_http_body_framing(&matching_lengths, HttpBodyFraming::UntilClose)
            .expect("matching content lengths should be accepted"),
        HttpBodyFraming::ContentLength(2)
    );
    let invalid_length = vec![("Content-Length".to_string(), "nope".to_string())];
    assert_eq!(
        super::parse_http_body_framing(&invalid_length, HttpBodyFraming::UntilClose)
            .expect_err("invalid content lengths should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let conflicting_lengths = vec![
        ("Content-Length".to_string(), "1".to_string()),
        ("Content-Length".to_string(), "2".to_string()),
    ];
    assert_eq!(
        super::parse_http_body_framing(&conflicting_lengths, HttpBodyFraming::UntilClose)
            .expect_err("conflicting content lengths should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let chunked_with_length = vec![
        ("Transfer-Encoding".to_string(), "chunked".to_string()),
        ("Content-Length".to_string(), "2".to_string()),
    ];
    assert_eq!(
        super::parse_http_body_framing(&chunked_with_length, HttpBodyFraming::UntilClose)
            .expect_err("chunked plus content-length should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let unsupported = vec![("Transfer-Encoding".to_string(), "gzip, chunked".to_string())];
    assert_eq!(
        super::parse_http_body_framing(&unsupported, HttpBodyFraming::UntilClose)
            .expect_err("stacked transfer codings should fail")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let identity = vec![("Transfer-Encoding".to_string(), " identity,  ".to_string())];
    assert_eq!(
        super::parse_http_body_framing(&identity, HttpBodyFraming::UntilClose)
            .expect("identity coding should preserve the default"),
        HttpBodyFraming::UntilClose
    );

    assert!(super::try_decode_chunked_http_body(b"", 0)
        .expect("incomplete chunk header should not fail")
        .is_none());
    assert_eq!(
        super::try_decode_chunked_http_body(b"\xff\r\n", 0)
            .expect_err("non-UTF-8 chunk sizes should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        super::try_decode_chunked_http_body(b"nope\r\n", 0)
            .expect_err("non-hex chunk sizes should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        super::try_decode_chunked_http_body(b"0\r\n\r\n", 0)
            .expect("empty chunked bodies should decode"),
        Some(Vec::new())
    );
    assert!(
        super::try_decode_chunked_http_body(b"0\r\nX-Test: yes\r\n", 0)
            .expect("incomplete trailers should not fail")
            .is_none()
    );
    assert_eq!(
        super::try_decode_chunked_http_body(b"4;kind=text\r\ntest\r\n0\r\nX-Test: yes\r\n\r\n", 0)
            .expect("extensions and trailers should decode"),
        Some(b"test".to_vec())
    );
    let oversized_size = format!("{:x}\r\n", TEST_HTTP_LIMIT + 1);
    assert_eq!(
        super::try_decode_chunked_http_body_with_limit(
            oversized_size.as_bytes(),
            0,
            TEST_HTTP_LIMIT,
        )
        .expect_err("oversized chunk declarations should fail")
        .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(super::try_decode_chunked_http_body(b"4\r\nabc", 0)
        .expect("incomplete chunk data should not fail")
        .is_none());
    assert_eq!(
        super::try_decode_chunked_http_body(b"3\r\nabcXX", 0)
            .expect_err("chunk data without CRLF should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let mut oversized_trailer = b"0\r\n".to_vec();
    oversized_trailer.extend(std::iter::repeat_n(b'a', TEST_HTTP_LIMIT + 1));
    oversized_trailer.extend_from_slice(b"\r\n\r\n");
    assert_eq!(
        super::try_decode_chunked_http_body_with_limit(&oversized_trailer, 0, TEST_HTTP_LIMIT,)
            .expect_err("oversized trailers should fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn http_request_builder_covers_host_variants_and_header_overrides() {
    fn request(url: &str, headers: Vec<(String, String)>) -> String {
        let url = url::Url::parse(url).expect("test URL should parse");
        String::from_utf8(
            super::build_http_request_bytes("POST", &url, b"ok", headers)
                .expect("request bytes should render"),
        )
        .expect("HTTP request bytes should be UTF-8")
    }

    assert!(request("http://[::1]/path", Vec::new()).contains("Host: [::1]\r\n"));
    assert!(request("http://[::1]:8080/path", Vec::new()).contains("Host: [::1]:8080\r\n"));
    assert!(request("http://127.0.0.1/path", Vec::new()).contains("Host: 127.0.0.1\r\n"));
    assert!(request("http://127.0.0.1:8080/path", Vec::new()).contains("Host: 127.0.0.1:8080\r\n"));
    assert!(request("http://example.com/path", Vec::new()).contains("Host: example.com\r\n"));
    assert!(
        request("http://example.com:8080/path", Vec::new()).contains("Host: example.com:8080\r\n")
    );
    assert!(request("https://example.com/path", Vec::new()).contains("Host: example.com\r\n"));
    assert!(request("wss://example.com:443/path", Vec::new()).contains("Host: example.com\r\n"));
    assert!(request("file:///tmp/aura", Vec::new()).contains("Host: \r\n"));

    let ws_ipv6 = url::Url::parse("ws://[::1]:9000/socket").expect("websocket URL should parse");
    assert_eq!(
        super::websocket_host_header(&ws_ipv6).expect("websocket host should render"),
        "[::1]:9000"
    );
    let ws_ipv6_default = url::Url::parse("ws://[::1]/socket").expect("websocket URL should parse");
    assert_eq!(
        super::websocket_host_header(&ws_ipv6_default).expect("websocket host should render"),
        "[::1]"
    );
    let ws_domain_default =
        url::Url::parse("ws://example.com/socket").expect("websocket URL should parse");
    assert_eq!(
        super::websocket_host_header(&ws_domain_default).expect("websocket host should render"),
        "example.com"
    );
    let ws_missing_host =
        url::Url::parse("mailto:aura@example.com").expect("hostless URL should parse");
    let missing_host = super::websocket_host_header(&ws_missing_host)
        .expect_err("hostless websocket URLs should fail host rendering");
    assert_eq!(missing_host.kind(), io::ErrorKind::InvalidInput);

    let with_query = request("http://example.com/search?q=aura", Vec::new());
    assert!(with_query.starts_with("POST /search?q=aura HTTP/1.1\r\n"));
    let root_path = request("http://example.com", Vec::new());
    assert!(root_path.starts_with("POST / HTTP/1.1\r\n"));

    let overridden = request(
        "http://example.com/path",
        vec![
            ("Host".to_string(), "custom.local".to_string()),
            ("Content-Length".to_string(), "2".to_string()),
            ("Connection".to_string(), "keep-alive".to_string()),
        ],
    );
    assert!(overridden.contains("Host: custom.local\r\n"));
    assert!(overridden.contains("Content-Length: 2\r\n"));
    assert!(overridden.contains("Connection: keep-alive\r\n"));
    assert!(!overridden.contains("Host: example.com\r\n"));
    assert!(!overridden.contains("Connection: close\r\n"));

    let error = HttpResponseValue::request_text(
        "GET",
        "ftp://example.com/",
        "",
        Vec::new(),
        Some(StdDuration::from_secs(1)),
        Some(&CancellationContext::default()),
    )
    .expect_err("unsupported URL schemes should fail before connecting");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn http_stream_helpers_cover_response_without_content_length_and_custom_headers() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\nbody")
            .expect("server response should write");
    });

    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    let response = super::read_http_response_from_stream(
        &mut client,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("response without content-length should read until close");
    assert_eq!(response.status(), 202);
    assert_eq!(response.reason(), "Accepted");
    assert_eq!(
        response.headers(),
        vec![("Connection".to_string(), "close".to_string())]
    );
    assert_eq!(response.bytes(), b"body");
    assert_eq!(
        response.text().expect("response body should decode"),
        "body".to_string()
    );
    server.join().expect("server thread should join");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let client = thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).expect("client should connect");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("client should read response");
        response
    });
    let (mut stream, _) = listener.accept().expect("server should accept");
    super::write_http_response_to_stream(
        &mut stream,
        201,
        vec![
            ("Content-Length".to_string(), "2".to_string()),
            ("Connection".to_string(), "close".to_string()),
        ],
        b"ok",
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("response should write");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("server write side should close");
    let response = client.join().expect("client thread should join");
    assert!(response.starts_with("HTTP/1.1 201 Created\r\n"));
    assert!(response.contains("Content-Length: 2\r\n"));
    assert!(response.contains("Connection: close\r\n"));
    assert!(response.ends_with("\r\n\r\nok"));
}

#[test]
fn http_response_limit_rejects_declared_and_close_delimited_overflow_early() {
    const TEST_HTTP_LIMIT: usize = 64;

    fn response_error(response: Vec<u8>) -> io::Error {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let address = listener.local_addr().expect("server address should exist");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server should accept");
            stream
                .write_all(&response)
                .expect("test response should write");
        });
        let mut client = std::net::TcpStream::connect(address).expect("client should connect");
        let error = super::read_http_response_from_stream_with_limit(
            &mut client,
            Some(Instant::now() + StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
            TEST_HTTP_LIMIT,
        )
        .expect_err("response above the injected HTTP limit should fail");
        server.join().expect("server thread should join");
        error
    }

    let declared = response_error(
        b"HTTP/1.1 200 OK\r\nContent-Length: 65\r\nConnection: close\r\n\r\n".to_vec(),
    );
    assert_eq!(declared.kind(), io::ErrorKind::InvalidData);
    assert!(declared.to_string().contains("64 bytes"));

    let mut close_delimited = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    close_delimited.extend(std::iter::repeat_n(b'x', TEST_HTTP_LIMIT));
    let close_delimited = response_error(close_delimited);
    assert_eq!(close_delimited.kind(), io::ErrorKind::InvalidData);
    assert!(close_delimited.to_string().contains("64 bytes"));
}

#[test]
fn http_streams_report_truncated_chunked_messages_and_incremental_limit_overflow() {
    struct ScriptedHttpReader {
        chunks: std::collections::VecDeque<Vec<u8>>,
    }

    impl Read for ScriptedHttpReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            buffer[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }

    impl super::HttpDeadlineReader for ScriptedHttpReader {
        fn read_http_some(
            &mut self,
            max_bytes: usize,
            _deadline: Option<Instant>,
            _cancellation: Option<&CancellationContext>,
        ) -> io::Result<Option<Vec<u8>>> {
            let mut buffer = vec![0; max_bytes];
            let count = self.read(&mut buffer)?;
            if count == 0 {
                Ok(None)
            } else {
                buffer.truncate(count);
                Ok(Some(buffer))
            }
        }
    }

    let mut truncated_response = ScriptedHttpReader {
        chunks: std::collections::VecDeque::from([
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nab".to_vec(),
        ]),
    };
    let error =
        super::read_http_response_from_stream_with_limit(&mut truncated_response, None, None, 128)
            .expect_err("EOF in a declared chunk must be reported, not accepted as a short body");
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        error.to_string(),
        "stream closed before the chunked HTTP response body was fully received"
    );

    let mut malformed_response = ScriptedHttpReader {
        chunks: std::collections::VecDeque::from([
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nnope\r\n".to_vec(),
        ]),
    };
    let error =
        super::read_http_response_from_stream_with_limit(&mut malformed_response, None, None, 128)
            .expect_err("a malformed response chunk size must be rejected at the stream boundary");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("invalid HTTP chunk size `nope`"));

    let mut incremental_overflow = ScriptedHttpReader {
        chunks: std::collections::VecDeque::from([
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(),
            vec![b'a'; 20],
            vec![b'b'; 10],
        ]),
    };
    let error =
        super::read_http_response_from_stream_with_limit(&mut incremental_overflow, None, None, 64)
            .expect_err("a close-delimited body must retain the cap across incremental reads");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("64 bytes"));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let client = thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).expect("client should connect");
        stream
            .write_all(
                b"POST /upload HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nab",
            )
            .expect("truncated request prefix should write");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("the client write side should close");
    });
    let (mut stream, _) = listener.accept().expect("server should accept");
    let error = super::read_http_request_from_stream_with_limit(
        &mut stream,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
        128,
    )
    .expect_err("EOF in a chunked request must be reported, not accepted as a short body");
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        error.to_string(),
        "stream closed before the chunked HTTP request body was fully received"
    );
    client.join().expect("client thread should join");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let client = thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).expect("client should connect");
        stream
            .write_all(
                b"POST /upload HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\nnope\r\n",
            )
            .expect("malformed request should write");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("the client write side should close");
    });
    let (mut stream, _) = listener.accept().expect("server should accept");
    let error = super::read_http_request_from_stream_with_limit(
        &mut stream,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
        128,
    )
    .expect_err("a malformed request chunk size must be rejected at the stream boundary");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("invalid HTTP chunk size `nope`"));
    client.join().expect("client thread should join");
}

#[test]
fn http_response_writer_rejects_header_injection_before_writing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let client = std::net::TcpStream::connect(address).expect("client should connect");
    let (mut server, _) = listener.accept().expect("server should accept");

    let error = super::write_http_response_to_stream(
        &mut server,
        200,
        vec![("X-Test".to_string(), "safe\r\nX-Injected: true".to_string())],
        b"body",
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect_err("response header injection must be rejected before bytes are written");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "HTTP header values may not contain control characters"
    );

    client
        .set_nonblocking(true)
        .expect("client test socket should become nonblocking");
    let mut byte = [0u8; 1];
    let read_error = (&client)
        .read(&mut byte)
        .expect_err("the rejected response must not write any bytes");
    assert_eq!(read_error.kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn http_stream_helpers_decode_chunked_requests_and_responses() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nWiki\r\n5;kind=text\r\npedia\r\n0\r\nX-Finished: yes\r\n\r\n",
            )
            .expect("chunked response should write");
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    let response = super::read_http_response_from_stream(
        &mut client,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("chunked response should decode");
    assert_eq!(
        response.text().expect("chunked body should be UTF-8"),
        "Wikipedia"
    );
    server.join().expect("server thread should join");

    let listener = HttpListenerValue::bind("127.0.0.1:0").expect("HTTP listener should bind");
    let address = listener
        .local_addr()
        .expect("listener address should exist");
    let server_listener = listener.clone();
    let server = thread::spawn(move || {
        let exchange = server_listener
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("chunked request should be accepted");
        assert_eq!(
            exchange.body_text().expect("request body should decode"),
            "aura"
        );
        exchange
            .respond_text(204, "", Vec::new())
            .expect("response should write");
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    client
        .write_all(
            b"POST /chunked HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nau\r\n2\r\nra\r\n0\r\n\r\n",
        )
        .expect("chunked request should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("request write side should close");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("server response should read");
    assert!(response.starts_with("HTTP/1.1 204 No Content"));
    server.join().expect("server thread should join");
    listener.close();
}

#[cfg(unix)]
#[test]
fn https_client_uses_tls_validation_and_decodes_chunked_responses() {
    let temp = TempDir::new("aura-https-client");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem()).expect("certificate should write");
    fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("key should write");
    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("UTF-8 certificate path"),
        key_path.to_str().expect("UTF-8 key path"),
    )
    .expect("TLS listener should bind");
    let address = listener.local_addr().expect("TLS address should exist");
    let server_listener = listener.clone();
    let server = thread::spawn(move || {
        let stream = server_listener
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("TLS server should accept");
        loop {
            let line = stream
                .read_line(
                    Some(StdDuration::from_secs(2)),
                    Some(&CancellationContext::default()),
                )
                .expect("HTTPS request line should read")
                .expect("HTTPS client should not close before headers");
            if line.is_empty() {
                break;
            }
        }
        stream
            .write_all(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nsecure\r\n0\r\n\r\n",
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("HTTPS response should write");
        stream.close();
    });
    let response = HttpResponseValue::request_text_with_ca(
        "GET",
        &format!("https://localhost:{}/", address.rsplit_once(':').unwrap().1),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
        cert_path.to_str().expect("UTF-8 certificate path"),
    )
    .expect("HTTPS request should validate the configured CA and succeed");
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().expect("HTTPS body should decode"), "secure");
    server.join().expect("HTTPS server should join");
    listener.close();

    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("UTF-8 certificate path"),
        key_path.to_str().expect("UTF-8 key path"),
    )
    .expect("second TLS listener should bind");
    let address = listener
        .local_addr()
        .expect("second TLS address should exist");
    let server_listener = listener.clone();
    let server = thread::spawn(move || {
        let stream = server_listener
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("second TLS server should accept");
        loop {
            let line = stream
                .read_line(
                    Some(StdDuration::from_secs(2)),
                    Some(&CancellationContext::default()),
                )
                .expect("HTTPS request line should read")
                .expect("HTTPS client should not close before headers");
            if line.is_empty() {
                break;
            }
        }
        stream
            .write_all(
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nsecure-close",
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("HTTPS response without content length should write");
        stream.close();
    });
    let response = HttpResponseValue::request_text_with_ca(
        "GET",
        &format!("https://localhost:{}/", address.rsplit_once(':').unwrap().1),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
        cert_path.to_str().expect("UTF-8 certificate path"),
    )
    .expect("HTTPS response without content length should read until close");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().expect("HTTPS body should decode"),
        "secure-close"
    );
    server.join().expect("second HTTPS server should join");
    listener.close();
}

#[cfg(unix)]
#[test]
fn https_client_reports_incomplete_and_oversized_response_framing() {
    let timeout = StdDuration::from_secs(2);
    let temp = TempDir::new("aura-https-response-framing");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem()).expect("certificate should write");
    fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("key should write");
    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("UTF-8 certificate path"),
        key_path.to_str().expect("UTF-8 key path"),
    )
    .expect("TLS listener should bind");
    let address = listener.local_addr().expect("TLS address should exist");
    let port = address
        .rsplit_once(':')
        .expect("TLS address should contain a port")
        .1;
    let oversized_length = super::MAX_HTTP_MESSAGE_BYTES + 1;
    let responses = vec![
        "HTTP/1.1 200 OK\r\n".to_string(),
        format!("HTTP/1.1 200 OK\r\nContent-Length: {oversized_length}\r\n\r\n"),
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab".to_string(),
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nab".to_string(),
    ];
    let server_listener = listener.clone();
    let server = thread::spawn(move || {
        for response in responses {
            let stream = server_listener
                .accept(Some(timeout), Some(&CancellationContext::default()))
                .expect("TLS server should accept each request");
            loop {
                let line = stream
                    .read_line(Some(timeout), Some(&CancellationContext::default()))
                    .expect("HTTPS request line should read")
                    .expect("HTTPS client should not close before its headers");
                if line.is_empty() {
                    break;
                }
            }
            stream
                .write_all(
                    &response,
                    Some(timeout),
                    Some(&CancellationContext::default()),
                )
                .expect("malformed HTTPS response should write");
            stream.close();
        }
    });

    let url = format!("https://localhost:{port}/");
    let request = || {
        HttpResponseValue::request_text_with_ca(
            "GET",
            &url,
            "",
            Vec::new(),
            Some(timeout),
            Some(&CancellationContext::default()),
            cert_path.to_str().expect("UTF-8 certificate path"),
        )
    };

    let incomplete_head = request().expect_err("an incomplete HTTPS response head must fail");
    assert_eq!(incomplete_head.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        incomplete_head.to_string(),
        "stream closed before a complete HTTP response was received"
    );

    let oversized = request().expect_err("an oversized HTTPS response must fail from its head");
    assert_eq!(oversized.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        oversized.to_string(),
        format!(
            "HTTP message exceeds the supported size limit of {} bytes",
            super::MAX_HTTP_MESSAGE_BYTES
        )
    );

    let truncated_fixed = request().expect_err("a truncated fixed HTTPS body must fail");
    assert_eq!(truncated_fixed.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        truncated_fixed.to_string(),
        "stream closed before the HTTP response body was fully received"
    );

    let truncated_chunked = request().expect_err("a truncated chunked HTTPS body must fail");
    assert_eq!(truncated_chunked.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        truncated_chunked.to_string(),
        "stream closed before the chunked HTTP response body was fully received"
    );

    server.join().expect("HTTPS framing server should join");
    listener.close();
}

#[test]
fn http_stream_helpers_report_unexpected_eof_for_incomplete_messages() {
    fn assert_unexpected_eof(error: io::Error) {
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        super::read_http_request_from_stream(
            &mut stream,
            Some(Instant::now() + StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect_err("incomplete request head should fail with EOF")
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    client
        .write_all(b"GET /partial HTTP/1.1\r\nHost: example.test")
        .expect("partial request head should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client write side should close");
    assert_unexpected_eof(server.join().expect("server thread should join"));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        super::read_http_request_from_stream(
            &mut stream,
            Some(Instant::now() + StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect_err("short request body should fail with EOF")
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    client
        .write_all(b"POST /body HTTP/1.1\r\nHost: example.test\r\nContent-Length: 4\r\n\r\nok")
        .expect("short request body should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client write side should close");
    assert_unexpected_eof(server.join().expect("server thread should join"));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        stream
            .write_all(b"HTTP/1.1 200")
            .expect("partial response head should write");
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    let error = super::read_http_response_from_stream(
        &mut client,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect_err("incomplete response head should fail with EOF");
    assert_unexpected_eof(error);
    server.join().expect("server thread should join");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nok")
            .expect("short response body should write");
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    let error = super::read_http_response_from_stream(
        &mut client,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect_err("short response body should fail with EOF");
    assert_unexpected_eof(error);
    server.join().expect("server thread should join");
}

#[test]
fn http_stream_helpers_read_split_request_and_response_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        super::read_http_request_from_stream(
            &mut stream,
            Some(Instant::now() + StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("split request body should read")
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    client
        .write_all(b"POST /split HTTP/1.1\r\nHost: example.test\r\nContent-Length: 4\r\n\r\n")
        .expect("request head should write");
    client.flush().expect("request head should flush");
    thread::sleep(StdDuration::from_millis(10));
    client
        .write_all(b"body")
        .expect("request body should write later");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client write side should close");
    let (_, path, _, body) = server.join().expect("server thread should join");
    assert_eq!(path, "/split");
    assert_eq!(body, b"body");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let address = listener.local_addr().expect("server address should exist");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
            .expect("response head should write");
        stream.flush().expect("response head should flush");
        thread::sleep(StdDuration::from_millis(10));
        stream
            .write_all(b"pong")
            .expect("response body should write");
    });
    let mut client = std::net::TcpStream::connect(address).expect("client should connect");
    let response = super::read_http_response_from_stream(
        &mut client,
        Some(Instant::now() + StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("split response body should read");
    assert_eq!(
        response.text().expect("response body should decode"),
        "pong"
    );
    server.join().expect("server thread should join");
}

#[test]
fn http_listener_replies_with_413_for_oversized_requests_and_continues_accepting() {
    let listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let server = listener.clone();
    let oversized_len = super::MAX_HTTP_MESSAGE_BYTES + 1;
    let server_thread = thread::spawn(move || {
        let exchange = server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("server should skip the oversized request and accept the next client");
        assert_eq!(exchange.method(), "GET");
        assert_eq!(exchange.path(), "/ok");
        exchange
            .respond_text(200, "ok", Vec::new())
            .expect("server should reply to the valid request");
    });

    let mut client =
        std::net::TcpStream::connect(&address).expect("http client should connect to listener");
    client
        .write_all(
            format!(
                "POST /upload HTTP/1.1\r\nHost: {address}\r\nContent-Length: {oversized_len}\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("http request head should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client shutdown should succeed");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("client should receive an HTTP response");
    assert!(
        response.starts_with("HTTP/1.1 413 Payload Too Large\r\n"),
        "expected a 413 response, got: {response:?}"
    );

    let response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{}/ok", address),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("listener should continue accepting after a 413");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().expect("response body should decode"),
        "ok".to_string()
    );

    server_thread
        .join()
        .expect("oversized http server thread should join");
}

#[test]
fn http_listener_replies_with_431_for_too_many_headers_and_continues_accepting() {
    let listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let exchange = server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("server should skip the invalid request and accept the next client");
        assert_eq!(
            Value::HttpExchange(exchange.clone()).render(),
            "<http-exchange>"
        );
        assert_value_equals_clone(Value::HttpExchange(exchange.clone()));
        assert_cast_source_type(Value::HttpExchange(exchange.clone()), "net.HttpExchange");
        assert_eq!(exchange.method(), "GET");
        assert_eq!(exchange.path(), "/ok");
        exchange
            .respond_text(200, "ok", Vec::new())
            .expect("server should reply to the valid request");
    });

    let mut client =
        std::net::TcpStream::connect(&address).expect("http client should connect to listener");
    let mut request = format!("GET /headers HTTP/1.1\r\nHost: {address}\r\n");
    for index in 0..=super::MAX_HTTP_HEADERS {
        request.push_str(&format!("X-Test-{index}: value\r\n"));
    }
    request.push_str("\r\n");
    client
        .write_all(request.as_bytes())
        .expect("request with too many headers should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client shutdown should succeed");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("client should receive an HTTP response");
    assert!(
        response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "expected a 431 response, got: {response:?}"
    );

    let response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{}/ok", address),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("listener should continue accepting after a 431");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().expect("response body should decode"),
        "ok".to_string()
    );

    server_thread
        .join()
        .expect("too-many-headers server thread should join");
}

#[test]
fn http_listener_replies_with_400_for_malformed_requests_and_continues_accepting() {
    let listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let exchange = server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("server should skip the malformed request and accept the next client");
        assert_eq!(exchange.method(), "GET");
        assert_eq!(exchange.path(), "/ok");
        exchange
            .respond_text(200, "ok", Vec::new())
            .expect("server should reply to the valid request");
    });

    let mut client =
        std::net::TcpStream::connect(&address).expect("http client should connect to listener");
    client
        .write_all(b"GE T /oops HTTP/1.1\r\nHost: malformed\r\n\r\n")
        .expect("malformed request should write");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client shutdown should succeed");

    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("client should receive an HTTP response");
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "expected a 400 response, got: {response:?}"
    );

    let response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{}/ok", address),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("listener should continue accepting after a malformed request");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().expect("response body should decode"),
        "ok".to_string()
    );

    server_thread
        .join()
        .expect("malformed-request server thread should join");
}

#[cfg(unix)]
#[test]
fn http_resources_use_nonblocking_descriptors_internally() {
    let short_timeout = StdDuration::from_secs(5);
    let listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener bind should succeed");
    let listener_fd = lock_mutex(&listener.inner.listener)
        .as_ref()
        .expect("http listener should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(listener_fd),
        "http listeners should stay in nonblocking mode internally"
    );

    let address = listener
        .local_addr()
        .expect("http listener local addr should succeed");
    let server_thread = {
        let server = listener.clone();
        thread::spawn(move || {
            let exchange = server
                .accept(Some(short_timeout), Some(&CancellationContext::default()))
                .expect("http accept should succeed");
            assert_eq!(format!("{exchange:?}"), "HttpExchangeValue(..)");
            assert_eq!(exchange, exchange.clone());
            {
                let stream_guard = lock_mutex(&exchange.inner.stream);
                let stream = stream_guard
                    .as_ref()
                    .expect("http exchange stream should still be open");
                let stream_fd = lock_mutex(&stream.inner.stream)
                    .as_ref()
                    .expect("http exchange stream should still be open")
                    .as_raw_fd();
                assert!(
                    fd_is_nonblocking(stream_fd),
                    "http exchange streams should stay in nonblocking mode internally"
                );
            }
            exchange
                .respond_text(200, "ok", Vec::new())
                .expect("http respond should succeed");
        })
    };

    let response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{}/nonblocking", address),
        "",
        Vec::new(),
        Some(short_timeout),
        Some(&CancellationContext::default()),
    )
    .expect("http request should succeed");
    assert_eq!(response.status(), 200);
    server_thread
        .join()
        .expect("http nonblocking server thread should join");
}

#[cfg(unix)]
#[test]
fn network_resources_use_nonblocking_descriptors_internally() {
    let short_timeout = StdDuration::from_secs(5);
    let cancellation = CancellationContext::default();

    let tcp_listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp bind should succeed");
    let tcp_listener_fd = lock_mutex(&tcp_listener.inner.listener)
        .as_ref()
        .expect("listener should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(tcp_listener_fd),
        "tcp listeners should stay in nonblocking mode internally"
    );
    let tcp_address = tcp_listener
        .local_addr()
        .expect("listener local addr should succeed");
    let tcp_server = tcp_listener.clone();
    let tcp_thread = thread::spawn(move || {
        let accepted = tcp_server
            .accept(Some(short_timeout), Some(&CancellationContext::default()))
            .expect("tcp accept should succeed");
        let accepted_fd = lock_mutex(&accepted.inner.stream)
            .as_ref()
            .expect("accepted tcp stream should still be open")
            .as_raw_fd();
        assert!(
            fd_is_nonblocking(accepted_fd),
            "accepted tcp streams should stay in nonblocking mode internally"
        );
        accepted.close();
    });
    let tcp_client =
        TcpStreamValue::connect(&tcp_address, Some(short_timeout), Some(&cancellation))
            .expect("tcp connect should succeed");
    let tcp_client_fd = lock_mutex(&tcp_client.inner.stream)
        .as_ref()
        .expect("tcp client stream should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(tcp_client_fd),
        "tcp client streams should stay in nonblocking mode internally"
    );
    tcp_client.close();
    tcp_thread.join().expect("tcp server thread should join");

    let udp_socket = UdpSocketValue::bind("127.0.0.1:0").expect("udp bind should succeed");
    let udp_fd = lock_mutex(&udp_socket.inner.socket)
        .as_ref()
        .expect("udp socket should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(udp_fd),
        "udp sockets should stay in nonblocking mode internally"
    );
}

#[cfg(unix)]
#[test]
fn socket_timeouts_honor_the_requested_budget() {
    let listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp bind should succeed");
    let started = Instant::now();
    let error = listener
        .accept(
            Some(StdDuration::from_millis(200)),
            Some(&CancellationContext::default()),
        )
        .expect_err("accept without a peer should time out");
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        elapsed >= StdDuration::from_millis(120),
        "timeouts should honor the caller's budget instead of returning after the first poll slice; elapsed: {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn unix_and_tls_helpers_cover_local_socket_and_tls_surface() {
    let temp = TempDir::new("aura-runtime-unix-tls");
    let socket_path = unique_unix_socket_path("a-ut");
    let listener = UnixListenerValue::bind(
        socket_path
            .to_str()
            .expect("unix socket path should be valid UTF-8"),
    )
    .expect("unix listener bind should succeed");
    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(
                Some(StdDuration::from_secs(1)),
                Some(&CancellationContext::default()),
            )
            .expect("unix accept should succeed");
        let line = stream
            .read_line(
                Some(StdDuration::from_secs(1)),
                Some(&CancellationContext::default()),
            )
            .expect("unix read_line should succeed");
        assert_eq!(line.as_deref(), Some("ping"));
        stream
            .write_all(
                "pong",
                Some(StdDuration::from_secs(1)),
                Some(&CancellationContext::default()),
            )
            .expect("unix write_all should succeed");
        stream.close();
    });
    let client = UnixStreamValue::connect(
        socket_path
            .to_str()
            .expect("unix socket path should be valid UTF-8"),
        Some(StdDuration::from_secs(1)),
        Some(&CancellationContext::default()),
    )
    .expect("unix connect should succeed");
    client
        .write_all(
            "ping\n",
            Some(StdDuration::from_secs(1)),
            Some(&CancellationContext::default()),
        )
        .expect("unix write should succeed");
    let reply = client
        .read_exact(
            4,
            Some(StdDuration::from_secs(1)),
            Some(&CancellationContext::default()),
        )
        .expect("unix read_exact should succeed");
    assert_eq!(reply, b"pong");
    server_thread
        .join()
        .expect("unix server thread should join");
    let _ = fs::remove_file(&socket_path);

    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_pem = certificate.cert.pem();
    let key_pem = certificate.key_pair.serialize_pem();
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, cert_pem.as_bytes()).expect("write cert pem");
    fs::write(&key_path, key_pem.as_bytes()).expect("write key pem");
    let missing_key_path = temp.path().join("missing-key.pem");
    fs::write(&missing_key_path, b"").expect("write empty key pem");
    let missing_key_error = super::load_tls_server_config(
        cert_path.to_str().expect("cert path should be UTF-8"),
        missing_key_path
            .to_str()
            .expect("missing key path should be UTF-8"),
    )
    .expect_err("TLS server config should reject PEM files without private keys");
    assert_eq!(missing_key_error.kind(), io::ErrorKind::InvalidInput);
    assert!(missing_key_error
        .to_string()
        .contains("did not contain a key"));
    super::load_tls_root_store(Some(cert_path.to_str().expect("cert path should be UTF-8")))
        .expect("custom CA PEM should extend the TLS root store");

    let tls_listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("cert path should be valid UTF-8"),
        key_path.to_str().expect("key path should be valid UTF-8"),
    )
    .expect("tls listener bind should succeed");
    assert_eq!(
        Value::TlsListener(tls_listener.clone()).render(),
        "<tls-listener>"
    );
    assert_value_equals_clone(Value::TlsListener(tls_listener.clone()));
    assert_cast_source_type(Value::TlsListener(tls_listener.clone()), "net.TlsListener");
    assert_eq!(format!("{tls_listener:?}"), "TlsListenerValue(..)");
    assert_eq!(tls_listener, tls_listener.clone());
    let tls_address = tls_listener
        .local_addr()
        .expect("tls listener local addr should succeed");
    let tls_server = tls_listener.clone();
    let tls_thread = thread::spawn(move || {
        let stream = tls_server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls accept should succeed");
        let line = stream
            .read_line(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls read_line should succeed");
        assert_eq!(line.as_deref(), Some("secure"));
        stream
            .write_all(
                "ok",
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls write_all should succeed");
        stream.close();
    });

    let tls_client = TlsStreamValue::connect(
        &tls_address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be valid UTF-8")),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("tls connect should succeed");
    assert_eq!(
        Value::TlsStream(tls_client.clone()).render(),
        "<tls-stream>"
    );
    assert_value_equals_clone(Value::TlsStream(tls_client.clone()));
    assert_cast_source_type(Value::TlsStream(tls_client.clone()), "net.TlsStream");
    assert_eq!(format!("{tls_client:?}"), "TlsStreamValue(..)");
    assert_eq!(tls_client, tls_client.clone());
    tls_client
        .write_all(
            "secure\n",
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls write_all should succeed");
    let tls_reply = tls_client
        .read_exact(
            2,
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls read_exact should succeed");
    assert_eq!(tls_reply, b"ok");
    tls_thread.join().expect("tls server thread should join");
}

#[cfg(unix)]
#[test]
fn tls_listener_accept_requires_a_completed_handshake() {
    let temp = TempDir::new("aura-runtime-tls-timeout");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem()).expect("write cert");
    fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("write key");

    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("valid cert path"),
        key_path.to_str().expect("valid key path"),
    )
    .expect("tls listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("tls listener addr should succeed");

    let silent_client = thread::spawn(move || {
        let _client =
            std::net::TcpStream::connect(address).expect("plain tcp client should connect");
        thread::sleep(StdDuration::from_millis(300));
    });

    let error = listener
        .accept(
            Some(StdDuration::from_millis(200)),
            Some(&CancellationContext::default()),
        )
        .expect_err("tls accept should fail when the peer never handshakes");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

    silent_client
        .join()
        .expect("silent tls client thread should join");
    listener.close();
}

#[cfg(unix)]
#[test]
fn tls_listener_accept_skips_timed_out_handshakes_and_accepts_the_next_peer() {
    let temp = TempDir::new("aura-runtime-tls-slowloris");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem()).expect("write cert");
    fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("write key");

    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("valid cert path"),
        key_path.to_str().expect("valid key path"),
    )
    .expect("tls listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("tls listener addr should succeed");

    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(
                Some(StdDuration::from_secs(11)),
                Some(&CancellationContext::default()),
            )
            .expect("tls listener should skip the stalled client");
        let line = stream
            .read_line(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls read_line should succeed");
        assert_eq!(line.as_deref(), Some("ready"));
        stream
            .write_all(
                "ok",
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls write_all should succeed");
    });

    let stalled_address = address.clone();
    let stalled_client = thread::spawn(move || {
        let _client =
            std::net::TcpStream::connect(stalled_address).expect("plain tcp client should connect");
        thread::sleep(StdDuration::from_secs(11));
    });

    thread::sleep(StdDuration::from_millis(100));
    let tls_client = TlsStreamValue::connect(
        &address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be valid UTF-8")),
        Some(StdDuration::from_secs(12)),
        Some(&CancellationContext::default()),
    )
    .expect("tls connect should succeed after the stalled peer is discarded");
    tls_client
        .write_all(
            "ready\n",
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls write_all should succeed");
    let reply = tls_client
        .read_exact(
            2,
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls read_exact should succeed");
    assert_eq!(reply, b"ok");

    stalled_client
        .join()
        .expect("stalled tls client thread should join");
    server_thread
        .join()
        .expect("tls slowloris server thread should join");
}

#[cfg(unix)]
#[test]
fn tls_listener_accept_is_not_linearly_delayed_by_multiple_stalled_peers() {
    let temp = TempDir::new("aura-runtime-tls-multi-slowloris");
    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem()).expect("write cert");
    fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("write key");

    let listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("valid cert path"),
        key_path.to_str().expect("valid key path"),
    )
    .expect("tls listener bind should succeed");
    let address = listener
        .local_addr()
        .expect("tls listener addr should succeed");

    let server = listener.clone();
    let server_thread = thread::spawn(move || {
        let stream = server
            .accept(
                Some(StdDuration::from_secs(25)),
                Some(&CancellationContext::default()),
            )
            .expect("tls listener should accept the legitimate peer without linear delay");
        let line = stream
            .read_line(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls read_line should succeed");
        assert_eq!(line.as_deref(), Some("ready"));
        stream
            .write_all(
                "ok",
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls write_all should succeed");
    });

    let stalled_a = {
        let address = address.clone();
        thread::spawn(move || {
            let _client =
                std::net::TcpStream::connect(address).expect("plain tcp client should connect");
            thread::sleep(StdDuration::from_secs(11));
        })
    };
    let stalled_b = {
        let address = address.clone();
        thread::spawn(move || {
            let _client =
                std::net::TcpStream::connect(address).expect("plain tcp client should connect");
            thread::sleep(StdDuration::from_secs(11));
        })
    };

    thread::sleep(StdDuration::from_millis(100));
    let start = Instant::now();
    let tls_client = TlsStreamValue::connect(
        &address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be valid UTF-8")),
        Some(StdDuration::from_secs(25)),
        Some(&CancellationContext::default()),
    )
    .expect("tls connect should succeed after the stalled peers are queued");
    let elapsed = start.elapsed();
    assert!(
        elapsed < crate::hosted_ci_timing_limit(StdDuration::from_secs(5)),
        "legitimate tls clients should not be delayed linearly by stalled peers; elapsed {:?}",
        elapsed
    );
    tls_client
        .write_all(
            "ready\n",
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls client write_all should succeed");
    let reply = tls_client
        .read_exact(
            2,
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls client read_exact should succeed");
    assert_eq!(reply, b"ok");

    stalled_a.join().expect("stalled tls client should join");
    stalled_b.join().expect("stalled tls client should join");
    server_thread.join().expect("tls server thread should join");
}

#[test]
fn tls_handshake_deadline_caps_requested_timeout_to_default_budget() {
    let deadline = super::tls_handshake_deadline(Some(
        Instant::now()
            .checked_add(StdDuration::from_secs(60))
            .expect("future deadline should exist"),
    ))
    .expect("the TLS handshake cap should fit the host deadline range")
    .expect("handshake deadline should exist");
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(
        remaining <= super::DEFAULT_TLS_HANDSHAKE_TIMEOUT + StdDuration::from_millis(250),
        "handshake deadline should cap user timeouts to the default budget; remaining {remaining:?}"
    );
}

#[test]
fn websocket_error_mapping_preserves_io_error_kinds() {
    let error = super::websocket_error_to_io(tungstenite::Error::Io(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "broken pipe",
    )));
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);

    let other = super::websocket_error_to_io(tungstenite::Error::ConnectionClosed);
    assert_eq!(other.kind(), io::ErrorKind::Other);
}

#[cfg(unix)]
#[test]
fn websocket_handshake_rejections_are_observable_on_both_transport_roles() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind websocket test");
    let address = listener.local_addr().expect("websocket test address");
    let accepting = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept malformed client");
        let error = match super::accept_websocket_stream(
            stream,
            Some(Instant::now() + StdDuration::from_secs(2)),
            None,
        ) {
            Ok(_) => panic!("a malformed client handshake must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::Other);
    });
    let mut malformed_client =
        std::net::TcpStream::connect(address).expect("connect malformed client");
    thread::sleep(StdDuration::from_millis(20));
    malformed_client
        .write_all(b"not an HTTP websocket request\r\n\r\n")
        .expect("write malformed handshake");
    malformed_client
        .shutdown(std::net::Shutdown::Both)
        .expect("close malformed client");
    accepting.join().expect("malformed accept worker");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind rejecting server");
    let address = listener.local_addr().expect("rejecting server address");
    let rejecting = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept websocket client");
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .expect("write handshake rejection");
        stream.flush().expect("flush handshake rejection");
    });
    let error = super::WebSocketValue::connect(
        &format!("ws://{address}/denied"),
        Some(StdDuration::from_secs(2)),
    )
    .expect_err("an HTTP rejection must fail the websocket connect operation");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    rejecting.join().expect("rejecting server worker");
}

#[cfg(unix)]
#[test]
fn websocket_ipv6_host_header_retains_brackets_and_explicit_port() {
    let parsed = url::Url::parse("ws://[::1]:8042/events").expect("valid IPv6 websocket URL");
    assert_eq!(
        super::websocket_host_header(&parsed).expect("IPv6 host header"),
        "[::1]:8042"
    );
}

#[cfg(unix)]
#[test]
fn unix_tls_and_websocket_resources_use_nonblocking_descriptors_internally() {
    let temp = TempDir::new("aura-runtime-evented-network");

    let socket_path = unique_unix_socket_path("a-ev");
    let unix_listener = UnixListenerValue::bind(
        socket_path
            .to_str()
            .expect("unix socket path should be valid UTF-8"),
    )
    .expect("unix listener bind should succeed");
    let unix_listener_fd = lock_mutex(&unix_listener.inner.listener)
        .as_ref()
        .expect("unix listener should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(unix_listener_fd),
        "unix listeners should stay in nonblocking mode internally"
    );
    let unix_server = unix_listener.clone();
    let unix_thread = thread::spawn(move || {
        let accepted = unix_server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("unix accept should succeed");
        let accepted_fd = lock_mutex(&accepted.inner.stream)
            .as_ref()
            .expect("unix accepted stream should still be open")
            .as_raw_fd();
        assert!(
            fd_is_nonblocking(accepted_fd),
            "accepted unix streams should stay in nonblocking mode internally"
        );
        accepted.close();
    });
    let unix_client = UnixStreamValue::connect(
        socket_path
            .to_str()
            .expect("unix socket path should be valid UTF-8"),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("unix connect should succeed");
    let unix_client_fd = lock_mutex(&unix_client.inner.stream)
        .as_ref()
        .expect("unix client stream should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(unix_client_fd),
        "unix client streams should stay in nonblocking mode internally"
    );
    unix_client.close();
    unix_thread.join().expect("unix server thread should join");
    let _ = fs::remove_file(&socket_path);

    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    fs::write(&cert_path, certificate.cert.pem().as_bytes()).expect("write cert pem");
    fs::write(&key_path, certificate.key_pair.serialize_pem().as_bytes()).expect("write key pem");

    let tls_listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("cert path should be valid UTF-8"),
        key_path.to_str().expect("key path should be valid UTF-8"),
    )
    .expect("tls listener bind should succeed");
    let tls_listener_fd = lock_mutex(&tls_listener.inner.listener)
        .as_ref()
        .expect("tls listener should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(tls_listener_fd),
        "tls listeners should stay in nonblocking mode internally"
    );
    let tls_address = tls_listener
        .local_addr()
        .expect("tls listener local addr should succeed");
    let tls_server = tls_listener.clone();
    let tls_thread = thread::spawn(move || {
        let accepted = tls_server
            .accept(
                Some(StdDuration::from_secs(2)),
                Some(&CancellationContext::default()),
            )
            .expect("tls accept should succeed");
        let accepted_fd = match lock_mutex(&accepted.inner.stream)
            .as_ref()
            .expect("tls accepted stream should still be open")
        {
            super::TlsStreamKind::Client(stream) => stream.sock.as_raw_fd(),
            super::TlsStreamKind::Server(stream) => stream.sock.as_raw_fd(),
        };
        assert!(
            fd_is_nonblocking(accepted_fd),
            "accepted tls streams should stay in nonblocking mode internally"
        );
        assert_eq!(
            accepted
                .read_exact(
                    1,
                    Some(StdDuration::from_secs(2)),
                    Some(&CancellationContext::default()),
                )
                .expect("tls handshake read should succeed"),
            b"x"
        );
        accepted.close();
    });
    let tls_client = TlsStreamValue::connect(
        &tls_address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be valid UTF-8")),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("tls connect should succeed");
    let tls_client_fd = match lock_mutex(&tls_client.inner.stream)
        .as_ref()
        .expect("tls client stream should still be open")
    {
        super::TlsStreamKind::Client(stream) => stream.sock.as_raw_fd(),
        super::TlsStreamKind::Server(stream) => stream.sock.as_raw_fd(),
    };
    assert!(
        fd_is_nonblocking(tls_client_fd),
        "tls client streams should stay in nonblocking mode internally"
    );
    tls_client
        .write_all(
            "x",
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("tls handshake write should succeed");
    tls_client.close();
    tls_thread.join().expect("tls server thread should join");

    let ws_listener = WebSocketListenerValue::bind("127.0.0.1:0")
        .expect("websocket listener bind should succeed");
    let ws_listener_fd = lock_mutex(&ws_listener.inner.listener)
        .as_ref()
        .expect("websocket listener should still be open")
        .as_raw_fd();
    assert!(
        fd_is_nonblocking(ws_listener_fd),
        "websocket listeners should stay in nonblocking mode internally"
    );
    let ws_address = ws_listener
        .local_addr()
        .expect("websocket listener local addr should succeed");
    let ws_server = ws_listener.clone();
    let ws_thread = thread::spawn(move || {
        let socket = ws_server
            .accept(Some(StdDuration::from_secs(2)))
            .expect("websocket accept should succeed");
        assert_eq!(format!("{socket:?}"), "WebSocketValue(..)");
        assert_eq!(socket, socket.clone());
        assert_eq!(Value::WebSocket(socket.clone()).render(), "<websocket>");
        assert_value_equals_clone(Value::WebSocket(socket.clone()));
        assert_cast_source_type(Value::WebSocket(socket.clone()), "net.WebSocket");
        let accepted_fd = match lock_mutex(&socket.inner.socket)
            .as_ref()
            .expect("accepted websocket should still be open")
        {
            super::WebSocketStateKind::Plain(socket) => socket.get_ref().as_raw_fd(),
            super::WebSocketStateKind::MaybeTls(socket) => match socket.get_ref() {
                tungstenite::stream::MaybeTlsStream::Plain(stream) => stream.as_raw_fd(),
                tungstenite::stream::MaybeTlsStream::Rustls(stream) => stream.get_ref().as_raw_fd(),
                _ => unreachable!("unexpected websocket transport"),
            },
        };
        assert!(
            fd_is_nonblocking(accepted_fd),
            "accepted websocket streams should stay in nonblocking mode internally"
        );
        socket.close().expect("websocket close should succeed");
    });
    let ws_client = super::WebSocketValue::connect(
        &format!("ws://{}", ws_address),
        Some(StdDuration::from_secs(2)),
    )
    .expect("websocket connect should succeed");
    assert_eq!(format!("{ws_client:?}"), "WebSocketValue(..)");
    assert_eq!(ws_client, ws_client.clone());
    assert_eq!(Value::WebSocket(ws_client.clone()).render(), "<websocket>");
    assert_value_equals_clone(Value::WebSocket(ws_client.clone()));
    assert_cast_source_type(Value::WebSocket(ws_client.clone()), "net.WebSocket");
    let ws_client_fd = match lock_mutex(&ws_client.inner.socket)
        .as_ref()
        .expect("websocket client should still be open")
    {
        super::WebSocketStateKind::Plain(socket) => socket.get_ref().as_raw_fd(),
        super::WebSocketStateKind::MaybeTls(socket) => match socket.get_ref() {
            tungstenite::stream::MaybeTlsStream::Plain(stream) => stream.as_raw_fd(),
            tungstenite::stream::MaybeTlsStream::Rustls(stream) => stream.get_ref().as_raw_fd(),
            _ => unreachable!("unexpected websocket transport"),
        },
    };
    assert!(
        fd_is_nonblocking(ws_client_fd),
        "websocket clients should stay in nonblocking mode internally"
    );
    ws_client
        .close()
        .expect("websocket client close should succeed");
    ws_thread
        .join()
        .expect("websocket server thread should join");
}
