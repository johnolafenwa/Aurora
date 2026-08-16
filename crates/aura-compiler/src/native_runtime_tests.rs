#![cfg(test)]

use super::{
    boxed_value, compare_values, current_cancellation, decode_bytes, eval_binary_value,
    eval_unary_value, extract_duration_nanoseconds, inferred_collection_type,
    int32_overflow_message, normalize_vec_index, render_bool, render_float, render_float32,
    render_runtime_diagnostic, runtime_span, runtime_type_from_name,
    runtime_type_pattern_from_name, runtime_type_pattern_matches, value_mut, value_ref,
    value_type_name, with_cancellation_scope, OpaqueValue,
};
use crate::ast::{BinaryOp, ReceiverKind, UnaryOp};
use crate::diag::{
    Diagnostic, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame, Span, StructuredDiagnostic,
};
use crate::integer::{IntegerBounds, IntegerKind, IntegerRepresentation, IntegerValue};
use crate::randomness::SecureRandomError;
use crate::runtime_value::{
    run_lightweight_root_task, spawn_lightweight_task, ArrayDType, ArrayStorage, ArrayValue,
    CancellationContext, ChannelValue, ClosureCaptureValue, ClosureEnvironment, EnumVariantValue,
    FileValue, FunctionValue, HttpListenerValue, HttpResponseValue, InstanceValue,
    LightweightTaskFailureSignal, MapValue, ModuleNamespaceValue, ProcessChildValue,
    ProcessCompletedValue, ProcessStdioConfig, ProcessSupervisorValue, RangeValue, RngValue,
    SetValue, TaskCancelledSignal, TaskGroupValue, TaskValue, TaskWaitStatus, TcpListenerValue,
    TcpStreamValue, TlsListenerValue, TlsStreamValue, TupleValue, UdpDatagramValue, UdpSocketValue,
    UnixListenerValue, UnixStreamValue, Value, VecValue, WebSocketListenerValue, WebSocketValue,
};
use crate::sema::{
    ClosureCallKind, ClosureCapture, ClosureCaptureMode, FunctionParamContract, Type,
};
#[cfg(unix)]
use corosensei::stack::Stack;
use rcgen::generate_simple_self_signed;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::panic;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

fn string_value(text: &str) -> *mut OpaqueValue {
    super::aura_direct_string_literal(text.as_ptr(), text.len())
}

fn int_value(value: i64) -> *mut OpaqueValue {
    super::aura_direct_box_i64(value)
}

fn float_value(value: f64) -> *mut OpaqueValue {
    super::aura_direct_box_f64(value)
}

fn bool_value(value: bool) -> *mut OpaqueValue {
    super::aura_direct_box_bool(i64::from(value))
}

#[test]
fn adr0038_direct_returned_view_projection_handoff_is_exact_and_consuming() {
    let selected = b"right";
    super::aura_direct_set_returned_view_projection(selected.as_ptr(), selected.len());
    let projections = b"left\0right";
    assert_eq!(
        super::aura_direct_take_returned_view_projection(projections.as_ptr(), projections.len()),
        1
    );
    assert!(super::with_direct_task_runtime_state(|state| state
        .returned_view_projection
        .is_none()));
}

#[test]
fn adr0038_direct_returned_view_projection_handoff_reports_invalid_state() {
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let projections = b"left\0right";
            super::aura_direct_take_returned_view_projection(
                projections.as_ptr(),
                projections.len(),
            );
        }),
        "direct returned view has no transferred projection"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let selected = b"middle";
            super::aura_direct_set_returned_view_projection(selected.as_ptr(), selected.len());
            let projections = b"left\0right";
            super::aura_direct_take_returned_view_projection(
                projections.as_ptr(),
                projections.len(),
            );
        }),
        "direct returned view selected an undeclared projection"
    );
}

#[test]
fn adr0038_direct_nested_place_assignment_helper_covers_tuple_and_class_paths() {
    let mut nested = Value::Instance(InstanceValue {
        class_name: "Outer".to_string(),
        fields: BTreeMap::from([(
            "inner".to_string(),
            Value::Tuple(TupleValue {
                element_types: vec![Type::named("int64")],
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            }),
        )]),
    });
    super::set_direct_instance_field_owned(
        &mut nested,
        &["inner", "0"],
        "inner.0",
        Value::Int(IntegerValue::from_signed(7)),
    )
    .expect("nested instance and tuple assignment should succeed");
    assert!(nested.render().contains("(7,)"));
    let mut tuple_then_instance = Value::Tuple(TupleValue {
        element_types: vec![Type::named("Inner")],
        elements: vec![Value::Instance(InstanceValue {
            class_name: "Inner".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(1)),
            )]),
        })],
    });
    super::set_direct_instance_field_owned(
        &mut tuple_then_instance,
        &["0", "value"],
        "0.value",
        Value::Int(IntegerValue::from_signed(8)),
    )
    .expect("tuple-to-instance assignment should recurse through both aggregate kinds");
    assert!(tuple_then_instance.render().contains("value=8"));

    assert_eq!(
        super::set_direct_instance_field_owned(&mut nested, &[], "", Value::Unit)
            .expect_err("empty paths must be rejected"),
        "direct runtime received an empty instance assignment path"
    );
    assert!(super::set_direct_instance_field_owned(
        &mut nested,
        &["missing", "field"],
        "missing.field",
        Value::Unit,
    )
    .expect_err("unknown nested fields must be diagnosed")
    .contains("has no field `missing`"));

    let mut tuple = Value::Tuple(TupleValue {
        element_types: vec![Type::named("int64")],
        elements: vec![Value::Int(IntegerValue::from_signed(1))],
    });
    assert!(
        super::set_direct_instance_field_owned(&mut tuple, &["field"], "field", Value::Unit,)
            .expect_err("tuple projections must be fixed positions")
            .contains("is not a fixed position")
    );
    assert!(
        super::set_direct_instance_field_owned(&mut tuple, &["4"], "4", Value::Unit,)
            .expect_err("tuple projections must be in bounds")
            .contains("has no element at index 4")
    );
    let mut scalar = Value::Int(IntegerValue::from_signed(1));
    assert!(
        super::set_direct_instance_field_owned(&mut scalar, &["value"], "value", Value::Unit,)
            .expect_err("scalar values cannot receive projected assignment")
            .contains("cannot assign field `value` on non-instance")
    );
}

#[test]
fn adr0038_direct_closure_capture_abi_covers_mutability_and_errors() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let base = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_capture".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(Vec::new()),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: None,
                entry_span: Span::new(1, 1),
                direct_thunk: Some(direct_zero_capture_closure as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: None,
            })));
            let captures = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store_owned(captures, 0, int_value(17) as i64);
            let capture_modes = [1_i64];
            let closure =
                super::aura_direct_closure_value(base, captures, 1, capture_modes.as_ptr(), 0);
            let captured = super::aura_direct_closure_capture(closure, 0);
            assert_eq!(expect_int(captured), 17);
            let Value::Function(function) = (unsafe { value_ref(closure) }) else {
                panic!("closure construction must preserve its function value");
            };
            assert!(
                function
                    .closure_environment
                    .as_ref()
                    .expect("closure environment must exist")
                    .arguments("main::__lambda_capture")
                    .expect("capture arguments should remain repeatable")[0]
                    .mutable
            );
            unsafe {
                release_value(captured);
                release_value(closure);
            }
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("capture ABI probe should complete"),
        Value::Unit
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_closure_capture(int_value(1), 0);
        }),
        "closure capture access expected a function value"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let plain = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "plain".to_string(),
                signature: Type::Function {
                    params: Vec::new(),
                    return_type: Box::new(Type::Unit),
                },
                source_path: None,
                entry_span: Span::new(1, 1),
                direct_thunk: None,
                direct_default_binder: None,
                closure_environment: None,
            })));
            super::aura_direct_closure_capture(plain, 0);
        }),
        "function has no closure environment"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_closure_capture(int_value(1), -1);
        }),
        "invalid closure capture index"
    );
}

fn integer_vector(kind: IntegerKind, values: &[i64]) -> *mut OpaqueValue {
    boxed_value(Value::Vec(VecValue {
        element_type: Type::named(kind.runtime_type_name()),
        elements: values
            .iter()
            .map(|value| {
                Value::Int(
                    IntegerValue::from_typed_signed(i128::from(*value), kind)
                        .expect("test vector element should fit"),
                )
            })
            .collect(),
    }))
}

unsafe extern "C-unwind" fn direct_array_double_thunk(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert_eq!(len, 1);
    let argument = unsafe { *args } as *mut OpaqueValue;
    let doubled = match unsafe { value_ref(argument) } {
        Value::Int(value) => {
            value
                .as_i128()
                .expect("array callback should receive a signed integer")
                * 2
        }
        other => panic!("array callback expected an integer, found {other:?}"),
    };
    unsafe {
        release_value(argument);
    }
    boxed_value(Value::Int(
        IntegerValue::from_typed_signed(doubled, IntegerKind::Int32)
            .expect("doubled test value should fit int32"),
    ))
}

#[test]
fn direct_array_abi_uses_typed_storage_kernels_and_callback_thunks() {
    let shape = integer_vector(IntegerKind::Int64, &[2, 2]);
    let values = integer_vector(IntegerKind::Int32, &[1, 2, 3, 4]);
    let array = super::aura_direct_array_from_vec(0, values, shape, 3, 5);
    assert_eq!(super::aura_direct_array_len(array), 4);

    let shape_value = super::aura_direct_array_shape(array);
    assert_eq!(
        unsafe { take_value(shape_value) },
        Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements: vec![
                Value::Int(IntegerValue::from_i64(2)),
                Value::Int(IntegerValue::from_i64(2)),
            ],
        })
    );

    let index = integer_vector(IntegerKind::Int64, &[0, -1]);
    let indexed = super::aura_direct_array_index(array, index, 7, 9);
    assert_eq!(
        unsafe { take_value(indexed) },
        Value::Int(IntegerValue::from_i32(2))
    );

    let vector_shape = integer_vector(IntegerKind::Int64, &[4]);
    let vector_values = integer_vector(IntegerKind::Int32, &[5, 6, 7, 8]);
    let vector = super::aura_direct_array_from_vec(0, vector_values, vector_shape, 7, 9);
    let scalar_index = super::aura_direct_box_i64(-1);
    let scalar_indexed = super::aura_direct_array_index(vector, scalar_index, 7, 9);
    assert_eq!(
        unsafe { take_value(scalar_indexed) },
        Value::Int(IntegerValue::from_i32(8)),
        "rank-one Array indexing lowers its single coordinate as a scalar int64"
    );
    let missing_index = super::aura_direct_box_i64(9);
    let missing = super::aura_direct_array_get(vector, missing_index, 7, 9);
    expect_option_none(missing);

    let clone_count = super::direct_value_clone_count();
    let scalar = super::aura_direct_box_i32(10);
    let added = super::aura_direct_array_binary(array, scalar, 0, 0, 0, 11, 13);
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "typed array arithmetic must borrow opaque operands instead of cloning dense buffers"
    );
    let self_added = super::aura_direct_array_binary(array, array, 0, 0, 0, 11, 13);
    assert_eq!(
        unsafe { take_value(self_added) },
        Value::Array(
            ArrayValue::new(
                vec![2, 2].into_boxed_slice(),
                ArrayStorage::Int32(vec![2, 4, 6, 8].into_boxed_slice()),
            )
            .unwrap()
        ),
        "the same Array handle must be readable as both operands without recursive lock failure"
    );
    let left_scalar = super::aura_direct_box_i32(10);
    let scalar_minus_array = super::aura_direct_array_binary(left_scalar, array, 1, 1, 0, 11, 13);
    assert_eq!(
        unsafe { take_value(scalar_minus_array) },
        Value::Array(
            ArrayValue::new(
                vec![2, 2].into_boxed_slice(),
                ArrayStorage::Int32(vec![9, 8, 7, 6].into_boxed_slice()),
            )
            .unwrap()
        ),
        "scalar-left subtraction must preserve operand order through the native ABI"
    );
    assert_eq!(
        unsafe { take_value(added) },
        Value::Array(
            ArrayValue::new(
                vec![2, 2].into_boxed_slice(),
                ArrayStorage::Int32(vec![11, 12, 13, 14].into_boxed_slice()),
            )
            .unwrap()
        )
    );

    let sum = super::aura_direct_array_reduce(array, 0, 15, 17);
    assert_eq!(
        unsafe { take_value(sum) },
        Value::Int(IntegerValue::from_i32(10))
    );

    let signature = Type::Function {
        params: vec![FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("int32"),
            passing: ReceiverKind::Value,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(Type::named("int32")),
    };
    let callback = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "double".to_string(),
        signature,
        source_path: Some("/workspace/array.au".to_string()),
        entry_span: Span::new(1, 1),
        direct_thunk: Some(direct_array_double_thunk as *const () as usize as i64),
        direct_default_binder: Some(1),
        closure_environment: None,
    })));
    let mapped = super::aura_direct_array_map(array, callback, 0, 19, 21);
    assert_eq!(
        unsafe { take_value(mapped) },
        Value::Array(
            ArrayValue::new(
                vec![2, 2].into_boxed_slice(),
                ArrayStorage::Int32(vec![2, 4, 6, 8].into_boxed_slice()),
            )
            .unwrap()
        )
    );

    for value in [
        shape,
        values,
        array,
        shape_value,
        index,
        indexed,
        vector_shape,
        vector_values,
        vector,
        scalar_index,
        scalar_indexed,
        missing_index,
        missing,
        scalar,
        added,
        self_added,
        left_scalar,
        scalar_minus_array,
        sum,
        callback,
        mapped,
    ] {
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn direct_array_abi_accepts_positive_literal_representations_at_signed_boundaries() {
    let tagged_literal = |value: u128, kind| {
        Value::Int(
            IntegerValue::from_literal(value)
                .with_runtime_kind(kind)
                .expect("the positive literal fits the requested signed runtime kind"),
        )
    };
    let shape = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![tagged_literal(2, IntegerKind::Int64)],
    }));
    let values = integer_vector(IntegerKind::Int32, &[7, 11]);
    let array = super::aura_direct_array_from_vec(0, values, shape, 17, 19);

    let scalar_coordinate = boxed_value(tagged_literal(1, IntegerKind::Int64));
    let scalar_result = super::aura_direct_array_index(array, scalar_coordinate, 23, 29);
    assert_eq!(
        unsafe { take_value(scalar_result) },
        Value::Int(IntegerValue::from_i32(11))
    );

    let vector_coordinate = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![tagged_literal(0, IntegerKind::Int64)],
    }));
    let vector_result = super::aura_direct_array_index(array, vector_coordinate, 31, 37);
    assert_eq!(
        unsafe { take_value(vector_result) },
        Value::Int(IntegerValue::from_i32(7))
    );

    for value in [
        shape,
        values,
        array,
        scalar_coordinate,
        scalar_result,
        vector_coordinate,
        vector_result,
    ] {
        unsafe { release_value(value) };
    }
}

#[test]
fn direct_array_public_abi_rejects_invalid_codes_and_runtime_values_exactly() {
    fn capture_diagnostic(invoke: impl FnOnce() + Send + 'static) -> Diagnostic {
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                invoke();
                Ok(Value::Unit)
            })
        })
        .expect_err("invalid direct Array ABI input should trap")
    }

    fn assert_diagnostic(diagnostic: &Diagnostic, code: &str, message: &str, span: Option<Span>) {
        assert_eq!(diagnostic.code, code);
        assert_eq!(diagnostic.message, message);
        assert_eq!(diagnostic.span, span);
    }

    let shape = integer_vector(IntegerKind::Int64, &[1]);
    let shape_address = shape as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_zeros(-1, shape_address as *mut OpaqueValue, 71, 73);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "direct Array ABI received invalid dtype code `-1`",
        Some(Span::new(71, 73)),
    );

    let wrong_shape_type = integer_vector(IntegerKind::Int32, &[1]);
    let wrong_shape_type_address = wrong_shape_type as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ =
            super::aura_direct_array_zeros(0, wrong_shape_type_address as *mut OpaqueValue, 75, 77);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array shape requires `list[int64]`, found `list[int32]`",
        Some(Span::new(75, 77)),
    );

    let malformed_shape = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![Value::String("one".to_string())],
    }));
    let malformed_shape_address = malformed_shape as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ =
            super::aura_direct_array_zeros(0, malformed_shape_address as *mut OpaqueValue, 79, 83);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array shape axis 0 is not an int64 value",
        Some(Span::new(79, 83)),
    );

    let wrong_kind_shape = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![Value::Int(IntegerValue::from_i32(1))],
    }));
    let wrong_kind_shape_address = wrong_kind_shape as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ =
            super::aura_direct_array_zeros(0, wrong_kind_shape_address as *mut OpaqueValue, 85, 87);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array shape axis 0 is not an int64 value",
        Some(Span::new(85, 87)),
    );

    let negative_shape = integer_vector(IntegerKind::Int64, &[-1]);
    let negative_shape_address = negative_shape as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ =
            super::aura_direct_array_zeros(0, negative_shape_address as *mut OpaqueValue, 88, 89);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "Array shape axis 0 cannot be negative, found -1",
        Some(Span::new(88, 89)),
    );

    let values = integer_vector(IntegerKind::Int64, &[1]);
    let values_address = values as usize;
    let shape_address = shape as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_from_vec(
            0,
            values_address as *mut OpaqueValue,
            shape_address as *mut OpaqueValue,
            89,
            97,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "Array[int32].from_list requires `list[int32]`, found `list[int64]`",
        Some(Span::new(89, 97)),
    );

    let source_values = integer_vector(IntegerKind::Int32, &[5]);
    let array = super::aura_direct_array_from_vec(0, source_values, shape, 1, 1);
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_reduce(array_address as *mut OpaqueValue, 9, 101, 103);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "direct Array ABI received invalid reduction code `9`",
        Some(Span::new(101, 103)),
    );

    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_binary(
            array_address as *mut OpaqueValue,
            array_address as *mut OpaqueValue,
            0,
            9,
            0,
            107,
            109,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "direct Array ABI received invalid binary operation code `9`",
        Some(Span::new(107, 109)),
    );

    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_binary(
            array_address as *mut OpaqueValue,
            array_address as *mut OpaqueValue,
            0,
            0,
            9,
            113,
            127,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "direct Array ABI received invalid arithmetic mode code `9`",
        Some(Span::new(113, 127)),
    );

    let left = super::aura_direct_box_i32(1);
    let right = super::aura_direct_box_i32(2);
    let left_address = left as usize;
    let right_address = right as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_binary(
            left_address as *mut OpaqueValue,
            right_address as *mut OpaqueValue,
            0,
            0,
            0,
            131,
            137,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "direct Array ABI received inconsistent operands `integer` and `integer` with scalar-left flag `0`",
        Some(Span::new(131, 137)),
    );

    let scalar_coordinate = super::aura_direct_box_i32(0);
    let scalar_coordinate_address = scalar_coordinate as usize;
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_index(
            array_address as *mut OpaqueValue,
            scalar_coordinate_address as *mut OpaqueValue,
            139,
            149,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array coordinates require int64 values",
        Some(Span::new(139, 149)),
    );

    let wrong_tuple_coordinate = boxed_value(Value::Tuple(TupleValue {
        element_types: vec![Type::named("int32")],
        elements: vec![Value::Int(IntegerValue::from_i32(0))],
    }));
    let wrong_tuple_coordinate_address = wrong_tuple_coordinate as usize;
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_index(
            array_address as *mut OpaqueValue,
            wrong_tuple_coordinate_address as *mut OpaqueValue,
            150,
            151,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array coordinates require int64 values",
        Some(Span::new(150, 151)),
    );

    let tuple_coordinate = boxed_value(Value::Tuple(TupleValue {
        element_types: vec![Type::named("int64")],
        elements: vec![Value::Int(IntegerValue::from_i64(0))],
    }));
    let indexed = super::aura_direct_array_index(array, tuple_coordinate, 151, 157);
    assert_eq!(
        unsafe { take_value(indexed) },
        Value::Int(IntegerValue::from_i32(5))
    );

    let invalid_coordinate = string_value("zero");
    let invalid_coordinate_address = invalid_coordinate as usize;
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_index(
            array_address as *mut OpaqueValue,
            invalid_coordinate_address as *mut OpaqueValue,
            163,
            167,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array coordinates require `list[int64]` or an int64 tuple, found `str`",
        Some(Span::new(163, 167)),
    );

    let malformed_coordinates = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![Value::String("zero".to_string())],
    }));
    let malformed_coordinates_address = malformed_coordinates as usize;
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_index(
            array_address as *mut OpaqueValue,
            malformed_coordinates_address as *mut OpaqueValue,
            173,
            179,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array coordinate on axis 0 is not an int64 value",
        Some(Span::new(173, 179)),
    );

    let wrong_kind_coordinates = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("int64"),
        elements: vec![Value::Int(IntegerValue::from_i32(0))],
    }));
    let wrong_kind_coordinates_address = wrong_kind_coordinates as usize;
    let array_address = array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_index(
            array_address as *mut OpaqueValue,
            wrong_kind_coordinates_address as *mut OpaqueValue,
            181,
            191,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4007",
        "array coordinate on axis 0 is not an int64 value",
        Some(Span::new(181, 191)),
    );

    let wrong_array = super::aura_direct_box_i32(1);
    let wrong_array_address = wrong_array as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_len(wrong_array_address as *mut OpaqueValue);
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "expected `Array`, found `integer`",
        None,
    );

    let wrong_array_address = wrong_array as usize;
    let fill_value = super::aura_direct_box_i32(7);
    let fill_value_address = fill_value as usize;
    let diagnostic = capture_diagnostic(move || {
        let _ = super::aura_direct_array_fill_in_place(
            wrong_array_address as *mut OpaqueValue,
            fill_value_address as *mut OpaqueValue,
            193,
            197,
        );
    });
    assert_diagnostic(
        &diagnostic,
        "AU4001",
        "expected `Array`, found `integer`",
        None,
    );

    let array_type = b"Array[int32]";
    assert_eq!(
        super::aura_direct_value_type_matches(array, array_type.as_ptr(), array_type.len()),
        1
    );

    for value in [
        wrong_shape_type,
        malformed_shape,
        wrong_kind_shape,
        negative_shape,
        values,
        source_values,
        array,
        left,
        right,
        scalar_coordinate,
        wrong_tuple_coordinate,
        tuple_coordinate,
        indexed,
        invalid_coordinate,
        malformed_coordinates,
        wrong_kind_coordinates,
        wrong_array,
        fill_value,
    ] {
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn direct_integer_width_public_abi_preserves_validation_diagnostics() {
    fn capture(
        left: *mut OpaqueValue,
        right: *mut OpaqueValue,
        operation: i64,
        mode: i64,
        line: i64,
        column: i64,
    ) -> Diagnostic {
        let left = left as usize;
        let right = right as usize;
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                let _ = super::aura_direct_integer_width_binary(
                    left as *mut OpaqueValue,
                    right as *mut OpaqueValue,
                    operation,
                    mode,
                    line,
                    column,
                );
                Ok(Value::Unit)
            })
        })
        .expect_err("invalid direct integer width ABI input should trap")
    }

    let int32 = super::aura_direct_box_i32(1);
    let other_int32 = super::aura_direct_box_i32(2);
    let int64 = super::aura_direct_box_i64(2);
    let int32_width = super::aura_direct_box_i32(32);
    let boolean = bool_value(true);

    let diagnostic = capture(int32, other_int32, 9, 1, 181, 191);
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "direct integer width-arithmetic ABI received invalid operation code `9`"
    );
    assert_eq!(diagnostic.span, Some(Span::new(181, 191)));

    let diagnostic = capture(int32, other_int32, 0, 9, 193, 197);
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "direct integer width-arithmetic ABI received invalid mode code `9`"
    );
    assert_eq!(diagnostic.span, Some(Span::new(193, 197)));

    let diagnostic = capture(boolean, other_int32, 0, 1, 199, 211);
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "direct integer width-arithmetic ABI expected an integer left operand, found `bool`"
    );
    assert_eq!(diagnostic.span, Some(Span::new(199, 211)));

    let diagnostic = capture(int32, int64, 0, 1, 223, 227);
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "`wrapping_add` expects matching fixed-width integer operands"
    );
    assert_eq!(diagnostic.span, Some(Span::new(223, 227)));

    let diagnostic = capture(int32, int32_width, 3, 1, 229, 233);
    assert_eq!(diagnostic.code, "AU4002");
    assert_eq!(
        diagnostic.message,
        "integer shift count `32` is outside the required range `0..32`"
    );
    assert_eq!(diagnostic.span, Some(Span::new(229, 233)));

    for value in [int32, other_int32, int64, int32_width, boolean] {
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn direct_array_kernel_failures_preserve_codes_and_source_spans() {
    let shape = integer_vector(IntegerKind::Int64, &[2]);
    let left_values = integer_vector(IntegerKind::Int32, &[1, 2]);
    let left = super::aura_direct_array_from_vec(0, left_values, shape, 1, 1);
    let other_shape = integer_vector(IntegerKind::Int64, &[1, 2]);
    let right_values = integer_vector(IntegerKind::Int32, &[1, 2]);
    let right = super::aura_direct_array_from_vec(0, right_values, other_shape, 1, 1);

    let left_address = left as usize;
    let right_address = right as usize;
    let diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_array_binary(
                left_address as *mut OpaqueValue,
                right_address as *mut OpaqueValue,
                0,
                1,
                0,
                23,
                29,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("mismatched Array shapes should fail the direct task");
    assert_eq!(diagnostic.code, "AU4007");
    assert_eq!(diagnostic.span, Some(Span::new(23, 29)));

    for value in [shape, left_values, left, other_shape, right_values, right] {
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn direct_array_map_allocation_failure_is_au4005_with_source_span() {
    let diagnostic = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::direct_array_map_result_buffer(usize::MAX, 31, 37);
            Ok(Value::Unit)
        })
    })
    .expect_err("an impossible Array.map result allocation should trap");

    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(diagnostic.span, Some(Span::new(31, 37)));
    assert!(
        diagnostic
            .message
            .contains("Array.map result could not allocate storage"),
        "unexpected allocation diagnostic: {}",
        diagnostic.message
    );
}

#[test]
fn direct_array_clone_uses_fallible_storage_copy_and_preserves_span() {
    let shape = integer_vector(IntegerKind::Int64, &[2]);
    let values = integer_vector(IntegerKind::Int64, &[5, 6]);
    let array = super::aura_direct_array_from_vec(1, values, shape, 1, 1);
    let cloned = super::aura_direct_array_clone(array, 41, 43);
    assert_eq!(
        unsafe { take_value(cloned) },
        unsafe { take_value(array) },
        "the direct clone kernel must preserve Array dtype, shape, and values"
    );
    let source_storage = super::with_array(array, |array| match &array.storage {
        ArrayStorage::Int64(values) => values.as_ptr(),
        other => panic!("expected int64 source storage, found {other:?}"),
    });
    let cloned_storage = super::with_array(cloned, |array| match &array.storage {
        ArrayStorage::Int64(values) => values.as_ptr(),
        other => panic!("expected int64 cloned storage, found {other:?}"),
    });
    assert_ne!(
        source_storage, cloned_storage,
        "explicit Array.clone must own an independent contiguous buffer"
    );

    let array_address = array as usize;
    let diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            crate::runtime_value::with_array_allocation_budget(0, || {
                let _ = super::aura_direct_array_clone(array_address as *mut OpaqueValue, 41, 43);
            });
            Ok(Value::Unit)
        })
    })
    .expect_err("injected direct Array.clone allocation failure should trap");
    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(diagnostic.span, Some(Span::new(41, 43)));
    assert!(diagnostic
        .message
        .contains("Array shape could not allocate"));

    let shape_diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            crate::runtime_value::with_array_allocation_budget(0, || {
                let _ = super::aura_direct_array_shape(array_address as *mut OpaqueValue);
            });
            Ok(Value::Unit)
        })
    })
    .expect_err("injected direct Array.shape allocation failure should trap");
    assert_eq!(shape_diagnostic.code, "AU4005");
    assert_eq!(shape_diagnostic.span, None);
    assert!(shape_diagnostic
        .message
        .contains("Array.shape result could not allocate"));

    for value in [shape, values, array, cloned] {
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn direct_array_constructor_shape_copy_reports_au4005_at_call_site() {
    let shape = integer_vector(IntegerKind::Int64, &[1]);
    let shape_address = shape as usize;
    let diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            crate::runtime_value::with_array_allocation_budget(0, || {
                let _ =
                    super::aura_direct_array_zeros(0, shape_address as *mut OpaqueValue, 47, 53);
            });
            Ok(Value::Unit)
        })
    })
    .expect_err("an Array constructor shape-copy allocation failure should trap");

    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(diagnostic.span, Some(Span::new(47, 53)));
    assert!(
        diagnostic
            .message
            .contains("Array shape could not allocate storage"),
        "unexpected allocation diagnostic: {}",
        diagnostic.message
    );

    unsafe {
        release_value(shape);
    }
}

#[test]
fn direct_nested_array_collection_reads_and_clones_use_fallible_copy() {
    fn array_value(value: i32) -> Value {
        Value::Array(ArrayValue {
            shape: vec![1].into_boxed_slice(),
            storage: ArrayStorage::Int32(vec![value].into_boxed_slice()),
        })
    }

    fn allocation_diagnostic(invoke: impl FnOnce() + Send + 'static) -> crate::diag::Diagnostic {
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                crate::runtime_value::with_array_allocation_budget(0, invoke);
                Ok(Value::Unit)
            })
        })
        .expect_err("copying a nested Array with no allocation budget should trap")
    }

    let vector = boxed_value(Value::Vec(VecValue {
        element_type: Type::Named("Array".to_string(), vec![Type::named("int32")]),
        elements: vec![array_value(7)],
    }));
    let vector_address = vector as usize;
    let diagnostic = allocation_diagnostic(move || {
        let _ = super::aura_direct_vec_get(vector_address as *mut OpaqueValue, 0);
    });
    assert_eq!(diagnostic.code, "AU4005");
    assert!(diagnostic
        .message
        .contains("Array shape could not allocate"));

    let map = boxed_value(Value::Map(MapValue {
        key_type: Type::named("str"),
        value_type: Type::Named("Array".to_string(), vec![Type::named("int32")]),
        entries: vec![(Value::String("item".to_string()), array_value(11))],
    }));
    let map_address = map as usize;
    let key = string_value("item");
    let key_address = key as usize;
    let diagnostic = allocation_diagnostic(move || {
        let _ = super::aura_direct_map_index(
            map_address as *mut OpaqueValue,
            key_address as *mut OpaqueValue,
            59,
            61,
        );
    });
    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(diagnostic.span, Some(Span::new(59, 61)));
    assert!(diagnostic
        .message
        .contains("Array shape could not allocate"));

    let clone_source = boxed_value(Value::Tuple(TupleValue {
        element_types: vec![Type::Named("Array".to_string(), vec![Type::named("int32")])],
        elements: vec![array_value(13)],
    }));
    let clone_source_address = clone_source as usize;
    let diagnostic = allocation_diagnostic(move || {
        let _ = super::aura_direct_clone_value(clone_source_address as *mut OpaqueValue);
    });
    assert_eq!(diagnostic.code, "AU4005");
    assert!(diagnostic.message.contains("Array"));

    let instance = boxed_value(Value::Instance(InstanceValue {
        class_name: "Holder".to_string(),
        fields: BTreeMap::from([("array".to_string(), array_value(23))]),
    }));
    let instance_address = instance as usize;
    let diagnostic = allocation_diagnostic(move || {
        let _ = super::aura_direct_instance_get_field(
            instance_address as *mut OpaqueValue,
            b"array".as_ptr(),
            "array".len(),
        );
    });
    assert_eq!(diagnostic.code, "AU4005");
    assert!(diagnostic.message.contains("Array"));

    let new_value = int_value(29);
    let instance_address = instance as usize;
    let new_value_address = new_value as usize;
    let diagnostic = allocation_diagnostic(move || {
        let _ = super::aura_direct_instance_set_field(
            instance_address as *mut OpaqueValue,
            b"other".as_ptr(),
            "other".len(),
            new_value_address as *mut OpaqueValue,
        );
    });
    assert_eq!(diagnostic.code, "AU4005");
    assert!(diagnostic.message.contains("Array"));

    for value in [vector, map, key, clone_source, instance, new_value] {
        unsafe {
            release_value(value);
        }
    }
}

fn direct_ffi_spec(
    symbol: &str,
    params: Vec<super::DirectFfiParam>,
    result: super::DirectFfiType,
) -> Vec<u8> {
    super::encode_direct_ffi_call_spec(&super::DirectFfiCallSpec {
        symbol: symbol.to_string(),
        params,
        result,
    })
}

fn direct_ffi_call(spec: &[u8], args: &[*mut OpaqueValue]) -> *mut OpaqueValue {
    let raw_args = args
        .iter()
        .map(|argument| *argument as i64)
        .collect::<Vec<_>>();
    super::aura_direct_ffi_call(
        spec.as_ptr(),
        spec.len() as i64,
        raw_args.as_ptr(),
        raw_args.len() as i64,
    )
}

#[test]
fn direct_ffi_call_spec_round_trips_and_rejects_corruption() {
    let call = super::DirectFfiCallSpec {
        symbol: "read".to_string(),
        params: vec![
            super::DirectFfiParam {
                passing: ReceiverKind::Borrow,
                ty: super::DirectFfiType::scalar(crate::ffi::FfiType::I32),
            },
            super::DirectFfiParam {
                passing: ReceiverKind::BorrowMut,
                ty: super::DirectFfiType::scalar(crate::ffi::FfiType::BytesViewMut),
            },
            super::DirectFfiParam {
                passing: ReceiverKind::Value,
                ty: super::DirectFfiType::opaque("Handle"),
            },
        ],
        result: super::DirectFfiType::scalar(crate::ffi::FfiType::I64),
    };
    let encoded = super::encode_direct_ffi_call_spec(&call);
    assert_eq!(
        super::decode_direct_ffi_call_spec(&encoded).expect("valid direct FFI metadata"),
        call
    );

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        super::decode_direct_ffi_call_spec(&wrong_magic),
        Err("metadata magic is not `AUFI`".to_string())
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        super::decode_direct_ffi_call_spec(&trailing),
        Err("metadata has trailing bytes".to_string())
    );

    assert_eq!(
        super::decode_direct_ffi_call_spec(&[]),
        Err("metadata ended unexpectedly".to_string())
    );

    let mut wrong_version = direct_ffi_spec(
        "x",
        Vec::new(),
        super::DirectFfiType::scalar(crate::ffi::FfiType::Unit),
    );
    wrong_version[4] = 1;
    assert_eq!(
        super::decode_direct_ffi_call_spec(&wrong_version),
        Err("unsupported metadata version 1".to_string())
    );

    let mut invalid_utf8 = direct_ffi_spec(
        "x",
        Vec::new(),
        super::DirectFfiType::scalar(crate::ffi::FfiType::Unit),
    );
    invalid_utf8[9] = 0xff;
    assert_eq!(
        super::decode_direct_ffi_call_spec(&invalid_utf8),
        Err("metadata text is not valid UTF-8".to_string())
    );

    let empty_symbol = direct_ffi_spec(
        "",
        Vec::new(),
        super::DirectFfiType::scalar(crate::ffi::FfiType::Unit),
    );
    assert_eq!(
        super::decode_direct_ffi_call_spec(&empty_symbol),
        Err("symbol name is empty".to_string())
    );

    let mut unknown_ownership = direct_ffi_spec(
        "x",
        vec![super::DirectFfiParam {
            passing: ReceiverKind::Borrow,
            ty: super::DirectFfiType::scalar(crate::ffi::FfiType::I32),
        }],
        super::DirectFfiType::scalar(crate::ffi::FfiType::Unit),
    );
    unknown_ownership[14] = 9;
    assert_eq!(
        super::decode_direct_ffi_call_spec(&unknown_ownership),
        Err("unknown ownership mode code 9".to_string())
    );

    let mut unknown_type = direct_ffi_spec(
        "x",
        Vec::new(),
        super::DirectFfiType::scalar(crate::ffi::FfiType::Unit),
    );
    unknown_type[14] = 255;
    assert_eq!(
        super::decode_direct_ffi_call_spec(&unknown_type),
        Err("unknown FFI type code 255".to_string())
    );

    let missing_handle_name = direct_ffi_spec("x", Vec::new(), super::DirectFfiType::opaque(""));
    assert_eq!(
        super::decode_direct_ffi_call_spec(&missing_handle_name),
        Err("opaque-handle metadata is missing its nominal type".to_string())
    );

    let scalar_with_handle_name = super::encode_direct_ffi_call_spec(&super::DirectFfiCallSpec {
        symbol: "x".to_string(),
        params: Vec::new(),
        result: super::DirectFfiType {
            ffi_type: crate::ffi::FfiType::I32,
            opaque_name: Some("NotAHandle".to_string()),
        },
    });
    assert_eq!(
        super::decode_direct_ffi_call_spec(&scalar_with_handle_name),
        Err("non-handle FFI type `int32` carries an opaque nominal name".to_string())
    );
}

#[test]
fn direct_ffi_metadata_pins_every_v0_type_code() {
    use crate::ffi::FfiType;

    for (ffi_type, code) in [
        (FfiType::Unit, 0),
        (FfiType::Bool, 1),
        (FfiType::I8, 2),
        (FfiType::I16, 3),
        (FfiType::I32, 4),
        (FfiType::I64, 5),
        (FfiType::U8, 6),
        (FfiType::U16, 7),
        (FfiType::U32, 8),
        (FfiType::U64, 9),
        (FfiType::F32, 10),
        (FfiType::F64, 11),
        (FfiType::StringView, 12),
        (FfiType::BytesView, 13),
        (FfiType::BytesViewMut, 14),
        (FfiType::OpaqueHandle, 15),
    ] {
        let result = if ffi_type == FfiType::OpaqueHandle {
            super::DirectFfiType::opaque("Handle")
        } else {
            super::DirectFfiType::scalar(ffi_type)
        };
        let call = super::DirectFfiCallSpec {
            symbol: "x".to_string(),
            params: Vec::new(),
            result,
        };
        let encoded = super::encode_direct_ffi_call_spec(&call);

        assert_eq!(encoded[14], code, "FFI v0 type-code drift for {ffi_type}");
        assert_eq!(
            super::decode_direct_ffi_call_spec(&encoded).expect("the v0 code must decode"),
            call,
            "FFI v0 type-code round trip for {ffi_type}"
        );
    }
}

#[test]
fn direct_ffi_value_conversion_preserves_every_exact_scalar_kind() {
    use crate::ffi::{FfiType, FfiValue};

    let cases = [
        (
            FfiType::I8,
            FfiValue::I8(i8::MIN),
            Value::Int(
                IntegerValue::from_typed_signed(i8::MIN as i128, IntegerKind::Int8).unwrap(),
            ),
        ),
        (
            FfiType::I16,
            FfiValue::I16(i16::MIN),
            Value::Int(
                IntegerValue::from_typed_signed(i16::MIN as i128, IntegerKind::Int16).unwrap(),
            ),
        ),
        (
            FfiType::I32,
            FfiValue::I32(i32::MIN),
            Value::Int(IntegerValue::from_i32(i32::MIN)),
        ),
        (
            FfiType::I64,
            FfiValue::I64(i64::MIN),
            Value::Int(IntegerValue::from_i64(i64::MIN)),
        ),
        (
            FfiType::U8,
            FfiValue::U8(u8::MAX),
            Value::Int(
                IntegerValue::from_typed_unsigned(u8::MAX as u128, IntegerKind::Uint8).unwrap(),
            ),
        ),
        (
            FfiType::U16,
            FfiValue::U16(u16::MAX),
            Value::Int(
                IntegerValue::from_typed_unsigned(u16::MAX as u128, IntegerKind::Uint16).unwrap(),
            ),
        ),
        (
            FfiType::U32,
            FfiValue::U32(u32::MAX),
            Value::Int(
                IntegerValue::from_typed_unsigned(u32::MAX as u128, IntegerKind::Uint32).unwrap(),
            ),
        ),
        (
            FfiType::U64,
            FfiValue::U64(u64::MAX),
            Value::Int(IntegerValue::from_u64(u64::MAX)),
        ),
    ];
    for (ffi_type, ffi_value, runtime_value) in cases {
        let ty = super::DirectFfiType::scalar(ffi_type);
        assert_eq!(
            super::direct_ffi_to_value(ffi_value, &ty).expect("exact scalar result"),
            runtime_value
        );
        assert_eq!(
            super::direct_value_to_ffi(&runtime_value, &ty).expect("exact scalar argument"),
            match ffi_type {
                FfiType::I8 => FfiValue::I8(i8::MIN),
                FfiType::I16 => FfiValue::I16(i16::MIN),
                FfiType::I32 => FfiValue::I32(i32::MIN),
                FfiType::I64 => FfiValue::I64(i64::MIN),
                FfiType::U8 => FfiValue::U8(u8::MAX),
                FfiType::U16 => FfiValue::U16(u16::MAX),
                FfiType::U32 => FfiValue::U32(u32::MAX),
                FfiType::U64 => FfiValue::U64(u64::MAX),
                _ => unreachable!(),
            }
        );
    }

    for (ffi_type, ffi_value, runtime_value) in [
        (FfiType::Bool, FfiValue::Bool(true), Value::Bool(true)),
        (
            FfiType::F32,
            FfiValue::F32(1.25),
            Value::Float(f64::from(1.25_f32)),
        ),
        (FfiType::F64, FfiValue::F64(-9.5), Value::Float(-9.5)),
    ] {
        let ty = super::DirectFfiType::scalar(ffi_type);
        assert_eq!(
            super::direct_ffi_to_value(ffi_value, &ty).expect("exact scalar result"),
            runtime_value
        );
    }

    for (ffi_type, runtime_value, ffi_value) in [
        (FfiType::Bool, Value::Bool(false), FfiValue::Bool(false)),
        (FfiType::F32, Value::Float(3.5), FfiValue::F32(3.5)),
        (FfiType::F64, Value::Float(-2.25), FfiValue::F64(-2.25)),
    ] {
        assert_eq!(
            super::direct_value_to_ffi(&runtime_value, &super::DirectFfiType::scalar(ffi_type))
                .expect("exact non-integer scalar argument"),
            ffi_value
        );
    }

    assert_eq!(
        super::direct_value_to_ffi(
            &Value::String("aura".to_string()),
            &super::DirectFfiType::scalar(FfiType::StringView),
        ),
        Ok(FfiValue::String("aura".to_string()))
    );
    let bytes = super::bytes_vec_value(vec![0, 127, 255]);
    for ffi_type in [FfiType::BytesView, FfiType::BytesViewMut] {
        assert_eq!(
            super::direct_value_to_ffi(&bytes, &super::DirectFfiType::scalar(ffi_type)),
            Ok(FfiValue::Bytes(vec![0, 127, 255]))
        );
    }
    assert_eq!(
        super::direct_ffi_to_value(FfiValue::Unit, &super::DirectFfiType::scalar(FfiType::Unit),),
        Ok(Value::Unit)
    );
}

#[test]
fn direct_ffi_value_conversion_reports_range_type_and_nominal_mismatches() {
    use crate::ffi::{FfiType, FfiValue};

    assert_eq!(
        super::direct_value_to_ffi(
            &Value::Int(
                IntegerValue::from_typed_unsigned(u128::MAX, IntegerKind::Uint128)
                    .expect("u128::MAX is a valid uint128"),
            ),
            &super::DirectFfiType::scalar(FfiType::I64),
        ),
        Err("FFI argument expected int64, but the integer is too large".to_string())
    );
    assert_eq!(
        super::direct_value_to_ffi(
            &Value::Int(IntegerValue::from_i32(-1)),
            &super::DirectFfiType::scalar(FfiType::U8),
        ),
        Err("FFI argument expected uint8, but received -1".to_string())
    );
    assert_eq!(
        super::direct_value_to_ffi(
            &Value::Int(IntegerValue::from_i32(256)),
            &super::DirectFfiType::scalar(FfiType::U8),
        ),
        Err("FFI argument expected uint8, but received `integer`".to_string())
    );
    for (ffi_type, value) in [
        (FfiType::I8, i128::from(i8::MAX) + 1),
        (FfiType::I16, i128::from(i16::MAX) + 1),
        (FfiType::I32, i128::from(i32::MAX) + 1),
        (FfiType::I64, i128::from(i64::MAX) + 1),
    ] {
        assert_eq!(
            super::direct_value_to_ffi(
                &Value::Int(IntegerValue::from_signed(value)),
                &super::DirectFfiType::scalar(ffi_type),
            ),
            Err(format!(
                "FFI argument expected {ffi_type}, but received `integer`"
            )),
            "the direct FFI adapter must reject a signed value just above {ffi_type}::MAX"
        );
    }
    for (ffi_type, value) in [
        (FfiType::U16, u128::from(u16::MAX) + 1),
        (FfiType::U32, u128::from(u32::MAX) + 1),
        (FfiType::U64, u128::from(u64::MAX) + 1),
    ] {
        assert_eq!(
            super::direct_value_to_ffi(
                &Value::Int(
                    IntegerValue::from_typed_unsigned(value, IntegerKind::Uint128)
                        .expect("each out-of-range probe still fits `uint128`"),
                ),
                &super::DirectFfiType::scalar(ffi_type),
            ),
            Err(format!(
                "FFI argument expected {ffi_type}, but received `integer`"
            )),
            "the direct FFI adapter must reject an unsigned value just above {ffi_type}::MAX"
        );
    }
    assert_eq!(
        super::direct_value_to_ffi(
            &Value::String("not-a-bool".to_string()),
            &super::DirectFfiType::scalar(FfiType::Bool),
        ),
        Err("FFI argument expected bool, but received `str`".to_string())
    );

    let handle = crate::runtime_value::FfiHandleValue::new(
        "ActualHandle".to_string(),
        std::ptr::without_provenance_mut::<std::ffi::c_void>(1),
    )
    .expect("the non-null test address is an opaque identity only");
    let rendered = Value::FfiHandle(handle.clone()).render();
    assert_eq!(rendered, "<opaque ActualHandle>");
    assert!(
        !rendered.contains("0x") && !rendered.contains('1'),
        "opaque handle rendering must never expose its address: {rendered}"
    );
    assert_eq!(
        super::direct_value_to_ffi(
            &Value::FfiHandle(handle),
            &super::DirectFfiType::opaque("ExpectedHandle"),
        ),
        Err("FFI argument expected opaque handle, but received `ActualHandle`".to_string())
    );
    assert_eq!(
        super::direct_ffi_to_value(
            FfiValue::I32(7),
            &super::DirectFfiType::scalar(FfiType::Bool),
        ),
        Err("FFI engine returned a value incompatible with declared result `bool`".to_string())
    );
}

#[test]
fn direct_ffi_mutable_byte_writeback_rejects_lost_buffer_without_mutating_the_caller() {
    use crate::ffi::FfiValue;

    let bytes = int_vec(&[4, 5]);
    assert_eq!(
        super::direct_ffi_write_back_mut_bytes(bytes, &FfiValue::Bool(false)),
        Err("FFI mutable byte view lost its byte buffer".to_string())
    );
    assert_eq!(
        unsafe { value_ref(bytes) },
        super::bytes_vec_value(vec![4, 5])
    );
    unsafe { release_value(bytes) };
}

#[test]
fn direct_ffi_adapter_rejects_invalid_boundary_metadata_and_argument_buffers() {
    use crate::ffi::FfiType;

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_ffi_call(std::ptr::null(), -1, std::ptr::null(), 0);
        }),
        "invalid direct FFI call-spec length"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_ffi_call(std::ptr::null(), 1, std::ptr::null(), 0);
        }),
        "direct FFI call received a null call-spec pointer"
    );

    let invalid_spec = b"not-a-call-spec".to_vec();
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_ffi_call(
                invalid_spec.as_ptr(),
                invalid_spec.len() as i64,
                std::ptr::null(),
                0,
            );
        }),
        "invalid direct FFI call spec: metadata magic is not `AUFI`"
    );

    let empty_call = direct_ffi_spec(
        "getpid",
        Vec::new(),
        super::DirectFfiType::scalar(FfiType::I32),
    );
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_ffi_call(
                empty_call.as_ptr(),
                empty_call.len() as i64,
                std::ptr::null(),
                -1,
            );
        }),
        "invalid direct FFI argument count"
    );

    let one_argument = direct_ffi_spec(
        "abs",
        vec![super::DirectFfiParam {
            passing: ReceiverKind::Borrow,
            ty: super::DirectFfiType::scalar(FfiType::I32),
        }],
        super::DirectFfiType::scalar(FfiType::I32),
    );
    assert_eq!(
        capture_direct_boundary_error_message({
            let one_argument = one_argument.clone();
            move || {
                super::aura_direct_ffi_call(
                    one_argument.as_ptr(),
                    one_argument.len() as i64,
                    std::ptr::null(),
                    1,
                );
            }
        }),
        "direct FFI call received a null argument buffer"
    );
    assert_eq!(
        capture_direct_boundary_error_message({
            let one_argument = one_argument.clone();
            move || {
                super::aura_direct_ffi_call(
                    one_argument.as_ptr(),
                    one_argument.len() as i64,
                    std::ptr::null(),
                    0,
                );
            }
        }),
        "direct FFI call spec expected 1 argument(s), but received 0"
    );
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            let null_argument = [0_i64];
            super::aura_direct_ffi_call(
                one_argument.as_ptr(),
                one_argument.len() as i64,
                null_argument.as_ptr(),
                1,
            );
        }),
        "direct FFI argument 1 has a null runtime value"
    );
}

#[test]
fn direct_ffi_adapter_reports_argument_marshalling_as_au4005() {
    use crate::ffi::FfiType;

    let spec = direct_ffi_spec(
        "abs",
        vec![super::DirectFfiParam {
            passing: ReceiverKind::Borrow,
            ty: super::DirectFfiType::scalar(FfiType::Bool),
        }],
        super::DirectFfiType::scalar(FfiType::I32),
    );
    let value = int_value(7);
    let value_address = value as usize;
    let diagnostic = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| {
                direct_ffi_call(&spec, &[value_address as *mut OpaqueValue]);
                Ok(Value::Unit)
            })
        })
    })
    .expect_err("a mismatched FFI argument must fail before the foreign call");
    assert_eq!(diagnostic.code, "AU4005");
    assert_eq!(
        diagnostic.message,
        "FFI call to `abs` failed: FFI argument expected bool, but received `integer`"
    );
    unsafe { release_value(value) };
}

#[test]
fn direct_ffi_mutable_bytes_write_back_before_return_validation_failure() {
    use crate::ffi::{call_host_function, FfiError, FfiSignature, FfiType, FfiValue, HostFunction};

    unsafe extern "C" fn mutate_then_return_invalid_bool(bytes: *mut u8, len: usize) -> u8 {
        let bytes = unsafe { std::slice::from_raw_parts_mut(bytes, len) };
        for byte in bytes {
            *byte = byte.wrapping_add(1);
        }
        2
    }

    let bytes = int_vec(&[1, 2, 255]);
    let mut arguments = vec![FfiValue::Bytes(vec![1, 2, 255])];
    let signature = FfiSignature::new(vec![FfiType::BytesViewMut], FfiType::Bool);
    let function =
        HostFunction::new(mutate_then_return_invalid_bool as *const () as *mut std::ffi::c_void)
            .expect("test function has a non-null address");
    let engine_result =
        unsafe { call_host_function(function, &signature, arguments.as_mut_slice()) };
    assert_eq!(arguments, [FfiValue::Bytes(vec![2, 3, 0])]);
    assert_eq!(engine_result, Err(FfiError::NonCanonicalBoolReturn(2)));

    let spec = super::DirectFfiCallSpec {
        symbol: "mutate_then_return_invalid_bool".to_string(),
        params: vec![super::DirectFfiParam {
            passing: ReceiverKind::BorrowMut,
            ty: super::DirectFfiType::scalar(FfiType::BytesViewMut),
        }],
        result: super::DirectFfiType::scalar(FfiType::Bool),
    };
    assert_eq!(
        super::finish_direct_ffi_call(&spec, &[bytes as i64], &arguments, engine_result),
        Err(super::DirectFfiCompletionError::Engine(
            FfiError::NonCanonicalBoolReturn(2)
        ))
    );
    assert_eq!(
        unsafe { value_ref(bytes) },
        Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: vec![
                Value::Int(IntegerValue::from_typed_unsigned(2, IntegerKind::Uint8).unwrap()),
                Value::Int(IntegerValue::from_typed_unsigned(3, IntegerKind::Uint8).unwrap()),
                Value::Int(IntegerValue::from_typed_unsigned(0, IntegerKind::Uint8).unwrap()),
            ],
        })
    );
    unsafe { release_value(bytes) };
}

#[cfg(unix)]
#[test]
fn direct_ffi_adapter_calls_process_symbols_and_maps_runtime_failures() {
    use crate::ffi::FfiType;

    let getpid = direct_ffi_spec(
        "getpid",
        Vec::new(),
        super::DirectFfiType::scalar(FfiType::I32),
    );
    let pid = direct_ffi_call(&getpid, &[]);
    let Value::Int(pid_value) = (unsafe { value_ref(pid) }) else {
        panic!("getpid should return an integer");
    };
    assert!(pid_value.as_i128().is_some_and(|pid| pid > 0));
    assert_eq!(pid_value.runtime_kind(), Some(IntegerKind::Int32));
    unsafe { release_value(pid) };

    let missing = direct_ffi_spec(
        "__aura_missing_direct_ffi_symbol__",
        Vec::new(),
        super::DirectFfiType::scalar(FfiType::I32),
    );
    let missing = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| {
                direct_ffi_call(&missing, &[]);
                Ok(Value::Unit)
            })
        })
    })
    .expect_err("missing FFI symbols should fail the direct task");
    assert_eq!(missing.code, "AU4005");
    assert!(missing
        .message
        .starts_with("FFI call to `__aura_missing_direct_ffi_symbol__` failed:"));

    let invalid_bool = direct_ffi_spec(
        "dup2",
        vec![
            super::DirectFfiParam {
                passing: ReceiverKind::Borrow,
                ty: super::DirectFfiType::scalar(FfiType::I32),
            },
            super::DirectFfiParam {
                passing: ReceiverKind::Borrow,
                ty: super::DirectFfiType::scalar(FfiType::I32),
            },
        ],
        super::DirectFfiType::scalar(FfiType::Bool),
    );
    let invalid_fd = super::aura_direct_box_i32(-1);
    let target_fd = super::aura_direct_box_i32(2);
    let invalid_fd_address = invalid_fd as usize;
    let target_fd_address = target_fd as usize;
    let invalid_bool = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| {
                direct_ffi_call(
                    &invalid_bool,
                    &[
                        invalid_fd_address as *mut OpaqueValue,
                        target_fd_address as *mut OpaqueValue,
                    ],
                );
                Ok(Value::Unit)
            })
        })
    })
    .expect_err("non-canonical FFI bools should fail the direct task");
    assert_eq!(invalid_bool.code, "AU4001");
    assert!(invalid_bool
        .message
        .contains("FFI bool return must be encoded as 0 or 1"));
    unsafe {
        release_value(invalid_fd);
        release_value(target_fd);
    }
}

#[cfg(unix)]
#[test]
fn direct_ffi_adapter_writes_back_bytes_and_keeps_handles_opaque() {
    use crate::ffi::FfiType;

    let mut pipe_fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    let payload = b"ABC";
    assert_eq!(
        unsafe {
            libc::write(
                pipe_fds[1],
                payload.as_ptr().cast::<std::ffi::c_void>(),
                payload.len(),
            )
        },
        payload.len() as isize
    );
    assert_eq!(unsafe { libc::close(pipe_fds[1]) }, 0);

    let fd = super::aura_direct_box_i32(i64::from(pipe_fds[0]));
    let bytes = int_vec(&[0, 0, 0]);
    let read = direct_ffi_spec(
        "read",
        vec![
            super::DirectFfiParam {
                passing: ReceiverKind::Borrow,
                ty: super::DirectFfiType::scalar(FfiType::I32),
            },
            super::DirectFfiParam {
                passing: ReceiverKind::BorrowMut,
                ty: super::DirectFfiType::scalar(FfiType::BytesViewMut),
            },
        ],
        super::DirectFfiType::scalar(FfiType::I64),
    );
    let read_count = direct_ffi_call(&read, &[fd, bytes]);
    assert_eq!(
        unsafe { value_ref(read_count) },
        Value::Int(IntegerValue::from_i64(3))
    );
    let Value::Vec(bytes_after) = (unsafe { value_ref(bytes) }) else {
        panic!("mutable FFI byte argument should remain a vector");
    };
    assert_eq!(
        bytes_after
            .elements
            .iter()
            .map(|value| {
                let Value::Int(value) = value else {
                    panic!("byte vector should contain integers");
                };
                value.as_i128().unwrap() as u8
            })
            .collect::<Vec<_>>(),
        payload
    );
    assert_eq!(unsafe { libc::close(pipe_fds[0]) }, 0);
    unsafe {
        release_value(fd);
        release_value(bytes);
        release_value(read_count);
    }

    let size = super::aura_direct_box_u64(1);
    let malloc = direct_ffi_spec(
        "malloc",
        vec![super::DirectFfiParam {
            passing: ReceiverKind::Borrow,
            ty: super::DirectFfiType::scalar(FfiType::U64),
        }],
        super::DirectFfiType::opaque("ProcessHandle"),
    );
    let handle = direct_ffi_call(&malloc, &[size]);
    let pointer = match unsafe { value_ref(handle) } {
        Value::FfiHandle(handle) => {
            assert_eq!(handle.type_name(), "ProcessHandle");
            assert_eq!(
                Value::FfiHandle(handle.clone()).render(),
                "<opaque ProcessHandle>"
            );
            handle.as_ptr()
        }
        other => panic!("malloc should return a dedicated opaque handle, found {other:?}"),
    };
    assert!(!pointer.is_null());
    let ffi_value = super::direct_value_to_ffi(
        &unsafe { value_ref(handle) },
        &super::DirectFfiType::opaque("ProcessHandle"),
    )
    .expect("the dedicated handle should marshal");
    let crate::ffi::FfiValue::OpaqueHandle(round_trip) = ffi_value else {
        panic!("opaque handle should retain its FFI identity");
    };
    assert_eq!(round_trip.as_ptr(), pointer);

    let free = direct_ffi_spec(
        "free",
        vec![super::DirectFfiParam {
            passing: ReceiverKind::Value,
            ty: super::DirectFfiType::opaque("ProcessHandle"),
        }],
        super::DirectFfiType::scalar(FfiType::Unit),
    );
    let unit = direct_ffi_call(&free, &[handle]);
    assert_eq!(unsafe { value_ref(unit) }, Value::Unit);
    unsafe {
        release_value(size);
        release_value(handle);
        release_value(unit);
    }
}

fn duration_value(value: i64) -> *mut OpaqueValue {
    let nanoseconds = (value as i128) * crate::runtime_value::NANOS_PER_MILLISECOND;
    duration_nanoseconds_value(nanoseconds)
}

fn duration_nanoseconds_value(value: i128) -> *mut OpaqueValue {
    super::aura_direct_duration_literal(value as i64, (value >> 64) as i64)
}

fn select_sources(element_types: Vec<Type>, elements: Vec<Value>) -> *mut OpaqueValue {
    boxed_value(Value::Tuple(TupleValue {
        element_types,
        elements,
    }))
}

#[test]
fn direct_runtime_tuple_type_names_parse_and_match_structurally() {
    assert_eq!(
        runtime_type_from_name("(int32, str)"),
        Type::Tuple(vec![Type::named("int32"), Type::named("str")])
    );
    assert_eq!(
        runtime_type_from_name("list[(str,)]"),
        Type::Named(
            "list".to_string(),
            vec![Type::Tuple(vec![Type::named("str")])]
        )
    );

    let pattern = runtime_type_pattern_from_name("(?Element, ?Element)");
    assert!(runtime_type_pattern_matches(
        &pattern,
        &Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
        &mut BTreeMap::new(),
    ));
    assert!(!runtime_type_pattern_matches(
        &pattern,
        &Type::Tuple(vec![Type::named("int64"), Type::named("str")]),
        &mut BTreeMap::new(),
    ));
    assert!(!runtime_type_pattern_matches(
        &pattern,
        &Type::named("int64"),
        &mut BTreeMap::new(),
    ));

    let value = Value::Tuple(TupleValue {
        element_types: vec![Type::named("int64"), Type::named("str")],
        elements: vec![
            Value::Int(IntegerValue::from_signed(1)),
            Value::String("one".to_string()),
        ],
    });
    assert_eq!(value_type_name(&value), "tuple");
    assert_eq!(
        inferred_collection_type(&value),
        Type::Tuple(vec![Type::named("int64"), Type::named("str")])
    );
}

fn string_vec(values: &[&str]) -> *mut OpaqueValue {
    let vec = super::aura_direct_vec_empty();
    for value in values {
        super::aura_direct_vec_push_in_place(vec, string_value(value));
    }
    vec
}

fn int_vec(values: &[i64]) -> *mut OpaqueValue {
    let vec = super::aura_direct_vec_empty();
    for value in values {
        let value = u8::try_from(*value).expect("test byte vectors only contain uint8 values");
        super::aura_direct_vec_push_in_place(
            vec,
            boxed_value(Value::Int(
                IntegerValue::from_typed_unsigned(value as u128, IntegerKind::Uint8)
                    .expect("every byte fits the uint8 runtime kind"),
            )),
        );
    }
    vec
}

fn task_vec(tasks: &[TaskValue]) -> *mut OpaqueValue {
    let vec = super::aura_direct_vec_empty();
    for task in tasks {
        expect_unit(super::aura_direct_vec_push_in_place(
            vec,
            boxed_value(Value::Task(task.clone())),
        ));
    }
    vec
}

fn process_restart_never_value() -> *mut OpaqueValue {
    boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "process.RestartPolicy".to_string(),
        variant_name: "Never".to_string(),
        payloads: Vec::new(),
    }))
}

fn start_supervisor_diagnostic_case(
    stdin: *mut OpaqueValue,
    stdout: *mut OpaqueValue,
    stderr: *mut OpaqueValue,
    restart: *mut OpaqueValue,
) {
    super::aura_direct_process_supervisor_start(
        boxed_value(Value::ProcessSupervisor(ProcessSupervisorValue::new())),
        string_value("worker"),
        string_vec(&["/bin/true"]),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        stdin,
        stdout,
        stderr,
        restart,
        duration_value(1),
        int_value(-1),
        bool_value(false),
    );
}

unsafe fn free_arg_buffer(buffer: *mut i64, count: usize) {
    let boxed = Box::from_raw(std::ptr::slice_from_raw_parts_mut(buffer, count));
    drop(boxed);
}

unsafe fn take_value(ptr: *mut OpaqueValue) -> Value {
    super::take_value(ptr)
}

unsafe fn retain_value(ptr: *mut OpaqueValue) -> *mut OpaqueValue {
    super::aura_direct_retain_value(ptr)
}

unsafe fn release_value(ptr: *mut OpaqueValue) {
    super::aura_direct_release_value(ptr)
}

fn expect_unit(ptr: *mut OpaqueValue) {
    match unsafe { take_value(ptr) } {
        Value::Unit => {}
        other => panic!("expected unit, found {:?}", other),
    }
}

fn expect_string(ptr: *mut OpaqueValue) -> String {
    match unsafe { take_value(ptr) } {
        Value::String(text) => text,
        other => panic!("expected string, found {:?}", other),
    }
}

fn expect_int(ptr: *mut OpaqueValue) -> i128 {
    match unsafe { take_value(ptr) } {
        Value::Int(value) => value.as_i128().expect("expected signed integer"),
        other => panic!("expected int, found {:?}", other),
    }
}

fn expect_task_result_ready_int(ptr: *mut OpaqueValue) -> i128 {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "TaskResult" && variant.variant_name == "Ready" =>
        {
            match variant
                .single_payload()
                .expect("expected task result payload")
            {
                Value::Int(value) => value.as_i128().expect("expected signed integer"),
                other => panic!("expected int payload, found {:?}", other),
            }
        }
        other => panic!("expected TaskResult.Ready(int), found {:?}", other),
    }
}

fn expect_task_result_error_message(ptr: *mut OpaqueValue) -> String {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "TaskResult" && variant.variant_name == "Error" =>
        {
            match variant
                .single_payload()
                .expect("expected task result payload")
            {
                Value::String(text) => text.clone(),
                other => panic!("expected string payload, found {:?}", other),
            }
        }
        other => panic!("expected TaskResult.Error(str), found {:?}", other),
    }
}

fn expect_float(ptr: *mut OpaqueValue) -> f64 {
    match unsafe { take_value(ptr) } {
        Value::Float(value) => value,
        other => panic!("expected float, found {:?}", other),
    }
}

fn expect_bool_boxed(ptr: *mut OpaqueValue) -> bool {
    match unsafe { take_value(ptr) } {
        Value::Bool(value) => value,
        other => panic!("expected bool, found {:?}", other),
    }
}

fn expect_vec_ints(ptr: *mut OpaqueValue) -> Vec<i128> {
    match unsafe { take_value(ptr) } {
        Value::Vec(values) => values
            .elements
            .into_iter()
            .map(|value| match value {
                Value::Int(value) => value.as_i128().expect("expected signed integer"),
                other => panic!("expected int element, found {:?}", other),
            })
            .collect(),
        other => panic!("expected vec, found {:?}", other),
    }
}

fn expect_vec_strings(ptr: *mut OpaqueValue) -> Vec<String> {
    match unsafe { take_value(ptr) } {
        Value::Vec(values) => values
            .elements
            .into_iter()
            .map(|value| match value {
                Value::String(text) => text.to_string(),
                other => panic!("expected string element, found {:?}", other),
            })
            .collect(),
        other => panic!("expected vec, found {:?}", other),
    }
}

fn expect_option_some_int(ptr: *mut OpaqueValue) -> i128 {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "Some" =>
        {
            match variant.single_payload().expect("expected option payload") {
                Value::Int(value) => value.as_i128().expect("expected signed integer"),
                other => panic!("expected int payload, found {:?}", other),
            }
        }
        other => panic!("expected Option.Some(int), found {:?}", other),
    }
}

fn expect_option_some_string(ptr: *mut OpaqueValue) -> String {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "Some" =>
        {
            match variant.single_payload().expect("expected option payload") {
                Value::String(text) => text.to_string(),
                other => panic!("expected string payload, found {:?}", other),
            }
        }
        other => panic!("expected Option.Some(str), found {:?}", other),
    }
}

fn assert_value_metadata(value: &Value, display_name: &str, type_name: &str) {
    assert_eq!(value_type_name(value), display_name);
    assert_eq!(
        inferred_collection_type(value),
        crate::sema::Type::named(type_name)
    );
}

fn assert_direct_type_match(value: Value, type_name: &str) {
    let ptr = boxed_value(value);
    assert_eq!(
        super::aura_direct_value_type_matches(ptr, type_name.as_ptr(), type_name.len()),
        1
    );
    let _ = unsafe { take_value(ptr) };
}

fn close_via_direct(value: Value) {
    let ptr = boxed_value(value);
    expect_unit(super::aura_direct_close_value(ptr, 0));
    unsafe { release_value(ptr) };
}

fn direct_host_builtin_call(name: &str, arguments: &[*mut OpaqueValue]) -> *mut OpaqueValue {
    let buffer = super::aura_direct_arg_buffer_new(arguments.len() as i64);
    for (index, argument) in arguments.iter().copied().enumerate() {
        super::aura_direct_arg_buffer_store(buffer, index as i64, argument as i64);
    }
    super::aura_direct_host_builtin(name.as_ptr(), name.len(), buffer, arguments.len() as i64)
}

fn direct_json_host_builtin_call(name: &str, arguments: &[*mut OpaqueValue]) -> *mut OpaqueValue {
    let clone_count = super::direct_value_clone_count();
    let result = direct_host_builtin_call(name, arguments);
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "{name} must not clone an opaque argument before JSON evaluation"
    );
    result
}

fn direct_json_host_builtin_error(
    name: &str,
    arguments: Vec<Option<Value>>,
) -> crate::diag::Diagnostic {
    let pointers = arguments
        .into_iter()
        .map(|value| value.map_or(std::ptr::null_mut(), boxed_value))
        .collect::<Vec<_>>();
    let addresses = pointers
        .iter()
        .map(|pointer| *pointer as usize)
        .collect::<Vec<_>>();
    let name = name.to_string();
    let result = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let arguments = addresses
                .iter()
                .map(|address| *address as *mut OpaqueValue)
                .collect::<Vec<_>>();
            let unexpected = direct_host_builtin_call(&name, &arguments);
            unsafe {
                release_value(unexpected);
            }
            Ok(Value::Unit)
        })
    });
    for pointer in pointers.into_iter().filter(|pointer| !pointer.is_null()) {
        unsafe {
            release_value(pointer);
        }
    }
    result.expect_err("the malformed direct JSON host call should fail")
}

fn direct_json_value(value: crate::json_codec::JsonValue) -> *mut OpaqueValue {
    boxed_value(
        crate::runtime_value::json_value_to_runtime(value)
            .expect("test JSON values should fit the runtime materialization budget"),
    )
}

fn direct_option_int(value: Option<i64>) -> *mut OpaqueValue {
    boxed_value(match value {
        Some(value) => crate::runtime_value::option_some(Value::Int(IntegerValue::from_i64(value))),
        None => crate::runtime_value::option_none(),
    })
}

#[test]
fn direct_host_builtin_ffi_covers_success_and_diagnostic_boundaries() {
    let empty_args = super::aura_direct_arg_buffer_new(0);
    let value =
        super::aura_direct_host_builtin(b"sys::args".as_ptr(), "sys::args".len(), empty_args, 0);
    assert!(matches!(unsafe { take_value(value) }, Value::Vec(_)));

    let base = string_value("root");
    let child = string_value("leaf");
    let join_args = super::aura_direct_arg_buffer_new(2);
    super::aura_direct_arg_buffer_store(join_args, 0, base as i64);
    super::aura_direct_arg_buffer_store(join_args, 1, child as i64);
    let joined =
        super::aura_direct_host_builtin(b"path::join".as_ptr(), "path::join".len(), join_args, 2);
    assert_eq!(
        unsafe { take_value(joined) },
        Value::String(format!("root{}leaf", std::path::MAIN_SEPARATOR))
    );
    unsafe {
        super::with_value(base, |value| {
            assert_eq!(value, &Value::String("root".to_string()))
        });
        super::with_value(child, |value| {
            assert_eq!(value, &Value::String("leaf".to_string()))
        });
        release_value(base);
        release_value(child);
        release_value(joined);
        release_value(value);
    }

    let unknown = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let empty_args = super::aura_direct_arg_buffer_new(0);
            let _ = super::aura_direct_host_builtin(
                b"missing::call".as_ptr(),
                "missing::call".len(),
                empty_args,
                0,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("unknown host builtins should fail the active task");
    assert!(unknown.message.contains("unknown host builtin"));

    let invalid_count = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_host_builtin(
                b"sys::args".as_ptr(),
                "sys::args".len(),
                std::ptr::null_mut(),
                -1,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid host builtin argument counts should fail the active task");
    assert!(invalid_count
        .message
        .contains("invalid host builtin argument count"));
}

#[test]
fn direct_host_arg_buffer_reports_metadata_and_storage_contract_violations() {
    fn assert_au4001(error: Diagnostic, message: &str) {
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, message);
    }

    let empty = super::DirectHostArgBuffer {
        handles: Vec::new(),
    };
    assert_au4001(
        empty
            .validate("json::missing")
            .expect_err("unknown dynamic JSON builtins must be rejected"),
        "unknown dynamic host builtin `json::missing`",
    );
    assert_au4001(
        empty
            .validate("json::parse")
            .expect_err("metadata arity must match the stored argument count"),
        "`json::parse` expects 1 arguments, found 0",
    );
    assert_au4001(
        empty
            .handle("json::missing", 0, ReceiverKind::Borrow)
            .expect_err("unknown metadata cannot supply an argument handle"),
        "unknown dynamic host builtin `json::missing`",
    );
    assert_au4001(
        empty
            .handle("json::parse", 1, ReceiverKind::Borrow)
            .expect_err("argument indexes beyond metadata must be rejected"),
        "`json::parse` has no argument 2",
    );
    assert_au4001(
        empty
            .handle("json::parse", 0, ReceiverKind::Borrow)
            .expect_err("metadata without matching storage must be rejected"),
        "`json::parse` is missing argument 1",
    );

    let null = super::DirectHostArgBuffer { handles: vec![0] };
    assert_au4001(
        null.handle("json::parse", 0, ReceiverKind::Borrow)
            .expect_err("null opaque handles must be rejected"),
        "`json::parse` received a null argument 1",
    );

    let passing_mismatch = super::DirectHostArgBuffer {
        handles: vec![boxed_value(Value::String("{}".to_string())) as i64],
    };
    assert_au4001(
        passing_mismatch
            .handle("json::parse", 0, ReceiverKind::Value)
            .expect_err("the runtime must detect compiler/runtime passing-mode drift"),
        "dynamic host ABI expected `json::parse` argument `text` to use Value passing, found Borrow",
    );
}

#[test]
fn direct_json_host_abi_rejects_malformed_values_with_precise_diagnostics() {
    fn assert_au4001(error: Diagnostic, message: &str) {
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, message);
    }
    fn json_variant(variant_name: &str, payloads: Vec<Value>) -> Value {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "json.Value".to_string(),
            variant_name: variant_name.to_string(),
            payloads,
        })
    }

    assert_au4001(
        direct_json_host_builtin_error("json::parse", vec![]),
        "`json::parse` expects 1 arguments, found 0",
    );
    assert_au4001(
        direct_json_host_builtin_error("json::parse", vec![None]),
        "`json::parse` received a null argument 1",
    );
    assert_au4001(
        direct_json_host_builtin_error("json::parse", vec![Some(Value::Bool(true))]),
        "`json::parse` expects argument 1 to be `str`",
    );

    assert_au4001(
        direct_json_host_builtin_error(
            "json::dumps",
            vec![
                Some(json_variant("Null", Vec::new())),
                Some(Value::Bool(false)),
            ],
        ),
        "`json::dumps` expects `indent` to be `Option[int64]`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::dumps",
            vec![
                Some(json_variant("Null", Vec::new())),
                Some(crate::runtime_value::option_some(Value::String(
                    "two".to_string(),
                ))),
            ],
        ),
        "`json::dumps` expects `indent` to be `Option[int64]`",
    );

    assert_au4001(
        direct_json_host_builtin_error(
            "json::is_null",
            vec![Some(json_variant("Null", vec![Value::Unit]))],
        ),
        "malformed runtime `json.Value.Null` payload in `json::is_null`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::as_bool",
            vec![Some(Value::String("not-json".to_string()))],
        ),
        "`json::as_bool` expected a runtime `json.Value`, found `not-json`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::as_bool",
            vec![Some(json_variant("Bool", Vec::new()))],
        ),
        "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::as_bool",
            vec![Some(json_variant(
                "Bool",
                vec![Value::Int(IntegerValue::from_i64(1))],
            ))],
        ),
        "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::as_float",
            vec![Some(json_variant("Float", vec![Value::Bool(true)]))],
        ),
        "malformed runtime `json.Value.Float` payload in `json::as_float`",
    );

    assert_au4001(
        direct_json_host_builtin_error("json::into_string", vec![Some(Value::Bool(true))]),
        "`json::into_string` expected a runtime `json.Value`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::into_string",
            vec![Some(Value::EnumVariant(EnumVariantValue {
                enum_name: "Other".to_string(),
                variant_name: "str".to_string(),
                payloads: vec![Value::String("value".to_string())],
            }))],
        ),
        "`json::into_string` expected enum `json.Value`, found `Other`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::into_string",
            vec![Some(json_variant("String", Vec::new()))],
        ),
        "malformed runtime `json.Value.String` payload in `json::into_string`",
    );
    assert_au4001(
        direct_json_host_builtin_error(
            "json::into_string",
            vec![Some(json_variant("String", vec![Value::Bool(true)]))],
        ),
        "malformed runtime `json.Value.String` payload in `json::into_string`",
    );
}

#[test]
fn direct_json_accessors_return_none_for_other_variants_and_owned_accessors_still_consume() {
    use crate::json_codec::JsonValue;

    for (name, input) in [
        ("json::as_bool", JsonValue::Int(7)),
        ("json::as_int", JsonValue::Bool(true)),
        ("json::as_float", JsonValue::Int(7)),
    ] {
        let source = direct_json_value(input);
        let result = direct_json_host_builtin_call(name, &[source]);
        expect_option_none(result);
        unsafe {
            super::with_value(source, |value| {
                assert!(
                    !matches!(value, Value::Unit),
                    "{name} must preserve a borrowed value when returning None"
                );
            });
            release_value(source);
            release_value(result);
        }
    }

    for name in ["json::into_string", "json::into_array", "json::into_object"] {
        let source = direct_json_value(JsonValue::Null);
        let result = direct_json_host_builtin_call(name, &[source]);
        expect_option_none(result);
        unsafe {
            super::with_value(source, |value| {
                assert!(
                    matches!(value, Value::Unit),
                    "{name} must consume its owned value even when returning None"
                );
            });
            release_value(source);
            release_value(result);
        }
    }
}

#[test]
fn direct_json_host_builtins_borrow_without_cloning_and_move_owned_payloads() {
    use crate::json_codec::JsonValue;

    let parse_source = boxed_value(Value::String(
        r#"{"z":1.0,"items":[true,null,"x"]}"#.to_string(),
    ));
    let parse_source_allocation = unsafe {
        super::with_value(parse_source, |value| match value {
            Value::String(value) => value.as_ptr(),
            other => panic!("expected parse source str, found {other:?}"),
        })
    };
    let parsed = direct_json_host_builtin_call("json::parse", &[parse_source]);
    let parsed_value = expect_result_ok_payload(parsed);
    assert!(matches!(
        parsed_value,
        Value::EnumVariant(ref variant)
            if variant.enum_name == "json.Value" && variant.variant_name == "Object"
    ));
    unsafe {
        super::with_value(parse_source, |value| match value {
            Value::String(value) => {
                assert_eq!(value.as_ptr(), parse_source_allocation);
                assert_eq!(value, r#"{"z":1.0,"items":[true,null,"x"]}"#);
            }
            other => panic!("borrowed json.parse source changed to {other:?}"),
        });
    }

    let dump_source = direct_json_value(JsonValue::object(vec![
        ("z".to_string(), JsonValue::Int(1)),
        (
            "items".to_string(),
            JsonValue::Array(vec![JsonValue::Bool(true), JsonValue::Null]),
        ),
    ]));
    let dump_object_allocation = unsafe {
        super::with_value(dump_source, |value| match value {
            Value::EnumVariant(variant) => match variant.payloads.as_slice() {
                [Value::Map(value)] => value.entries.as_ptr(),
                other => panic!("expected json.Value.Object payload, found {other:?}"),
            },
            other => panic!("expected json.Value.Object, found {other:?}"),
        })
    };
    let indent = direct_option_int(None);
    let dumped = direct_json_host_builtin_call("json::dumps", &[dump_source, indent]);
    assert_eq!(
        unsafe { take_value(dumped) },
        Value::String(r#"{"items":[true,null],"z":1}"#.to_string())
    );
    unsafe {
        super::with_value(dump_source, |value| match value {
            Value::EnumVariant(variant) => match variant.payloads.as_slice() {
                [Value::Map(value)] => assert_eq!(value.entries.as_ptr(), dump_object_allocation),
                other => panic!("borrowed dump payload changed to {other:?}"),
            },
            other => panic!("borrowed json.dumps source changed to {other:?}"),
        });
        super::with_value(indent, |value| {
            assert_eq!(
                value.render(),
                "Option.None",
                "copy-valued indent should remain usable"
            )
        });
    }

    let null = direct_json_value(JsonValue::Null);
    let is_null = direct_json_host_builtin_call("json::is_null", &[null]);
    assert_eq!(unsafe { take_value(is_null) }, Value::Bool(true));
    unsafe {
        super::with_value(null, |value| {
            assert!(
                matches!(
                    value,
                    Value::EnumVariant(variant)
                        if variant.enum_name == "json.Value"
                            && variant.variant_name == "Null"
                ),
                "json.is_null must preserve its borrowed argument"
            )
        });
    }

    for (name, input, expected) in [
        ("json::as_bool", JsonValue::Bool(true), "Option.Some(true)"),
        ("json::as_int", JsonValue::Int(7), "Option.Some(7)"),
        ("json::as_float", JsonValue::Float(1.5), "Option.Some(1.5)"),
    ] {
        let source = direct_json_value(input);
        let output = direct_json_host_builtin_call(name, &[source]);
        assert_eq!(unsafe { take_value(output) }.render(), expected);
        unsafe {
            super::with_value(source, |value| {
                assert!(
                    !matches!(value, Value::Unit),
                    "{name} must preserve its borrowed argument"
                )
            });
            release_value(source);
            release_value(output);
        }
    }

    let owned_string = direct_json_value(JsonValue::String("aura".to_string()));
    let string_allocation = unsafe {
        super::with_value(owned_string, |value| match value {
            Value::EnumVariant(variant) => match variant.payloads.as_slice() {
                [Value::String(value)] => value.as_ptr(),
                other => panic!("expected json.Value.String payload, found {other:?}"),
            },
            other => panic!("expected json.Value.String, found {other:?}"),
        })
    };
    let extracted_string = direct_json_host_builtin_call("json::into_string", &[owned_string]);
    unsafe {
        super::with_value(extracted_string, |value| match value {
            Value::EnumVariant(variant)
                if variant.enum_name == "Option" && variant.variant_name == "Some" =>
            {
                match variant.payloads.as_slice() {
                    [Value::String(value)] => {
                        assert_eq!(value, "aura");
                        assert_eq!(value.as_ptr(), string_allocation);
                    }
                    other => panic!("expected extracted str, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(str), found {other:?}"),
        });
        super::with_value(owned_string, |value| {
            assert!(
                matches!(value, Value::Unit),
                "owned str source was not moved"
            )
        });
    }

    let owned_array = direct_json_value(JsonValue::Array(vec![JsonValue::Int(2)]));
    let array_allocation = unsafe {
        super::with_value(owned_array, |value| match value {
            Value::EnumVariant(variant) => match variant.payloads.as_slice() {
                [Value::Vec(value)] => value.elements.as_ptr(),
                other => panic!("expected json.Value.Array payload, found {other:?}"),
            },
            other => panic!("expected json.Value.Array, found {other:?}"),
        })
    };
    let extracted_array = direct_json_host_builtin_call("json::into_array", &[owned_array]);
    unsafe {
        super::with_value(extracted_array, |value| match value {
            Value::EnumVariant(variant)
                if variant.enum_name == "Option" && variant.variant_name == "Some" =>
            {
                match variant.payloads.as_slice() {
                    [Value::Vec(value)] => {
                        assert_eq!(value.elements.as_ptr(), array_allocation)
                    }
                    other => panic!("expected extracted Vec, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(Vec), found {other:?}"),
        });
        super::with_value(owned_array, |value| {
            assert!(
                matches!(value, Value::Unit),
                "owned Array source was not moved"
            )
        });
    }

    let owned_object = direct_json_value(JsonValue::object(vec![(
        "k".to_string(),
        JsonValue::Int(3),
    )]));
    let object_allocation = unsafe {
        super::with_value(owned_object, |value| match value {
            Value::EnumVariant(variant) => match variant.payloads.as_slice() {
                [Value::Map(value)] => value.entries.as_ptr(),
                other => panic!("expected json.Value.Object payload, found {other:?}"),
            },
            other => panic!("expected json.Value.Object, found {other:?}"),
        })
    };
    let extracted_object = direct_json_host_builtin_call("json::into_object", &[owned_object]);
    unsafe {
        super::with_value(extracted_object, |value| match value {
            Value::EnumVariant(variant)
                if variant.enum_name == "Option" && variant.variant_name == "Some" =>
            {
                match variant.payloads.as_slice() {
                    [Value::Map(value)] => assert_eq!(value.entries.as_ptr(), object_allocation),
                    other => panic!("expected extracted Map, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(Map), found {other:?}"),
        });
        super::with_value(owned_object, |value| {
            assert!(
                matches!(value, Value::Unit),
                "owned Object source was not moved"
            )
        });
    }

    for pointer in [
        parse_source,
        parsed,
        dump_source,
        indent,
        dumped,
        null,
        is_null,
        owned_string,
        extracted_string,
        owned_array,
        extracted_array,
        owned_object,
        extracted_object,
    ] {
        unsafe { release_value(pointer) };
    }
}

#[test]
fn direct_json_parse_materialization_allocation_failure_is_au4005_and_preserves_source() {
    let source = boxed_value(Value::String("[null]".to_string()));
    let source_address = source as usize;
    let source_ptr = unsafe {
        super::with_value(source, |value| match value {
            Value::String(value) => value.as_ptr(),
            other => panic!("expected str, found {other:?}"),
        })
    };

    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            crate::runtime_value::with_json_runtime_allocation_budget(0, || {
                let _ =
                    direct_host_builtin_call("json::parse", &[source_address as *mut OpaqueValue]);
            });
            Ok(Value::Unit)
        })
    })
    .expect_err("direct parse materialization allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing parsed JSON"
    );
    unsafe {
        super::with_value(source, |value| match value {
            Value::String(value) => assert_eq!(value.as_ptr(), source_ptr),
            other => panic!("expected str, found {other:?}"),
        });
        release_value(source);
    }
}

#[test]
fn direct_json_parse_reserves_capacity_before_borrowing_and_copying_the_source() {
    let source = boxed_value(Value::String("null".to_string()));
    let source_address = source as usize;
    let clone_count = super::direct_value_clone_count();

    let result = run_lightweight_root_task(move || {
        let (_, first_reservation) =
            crate::runtime_value::prepare_json_codec_source(|| Ok("held-one".to_string()))?;
        let (_, second_reservation) =
            crate::runtime_value::prepare_json_codec_source(|| Ok("held-two".to_string()))?;

        let parse = spawn_lightweight_task(move || {
            unsafe {
                super::retain_untracked_value(source_address as *mut OpaqueValue);
            }
            let args = super::DirectHostArgBuffer {
                handles: vec![source_address as i64],
            };
            super::evaluate_direct_json_host_builtin("json::parse", &args)
        })?;
        crate::runtime_value::yield_now_with_runtime_scheduler();

        let source = unsafe { &*(source_address as *mut OpaqueValue) };
        let mut source_guard = source
            .value
            .try_write()
            .expect("codec saturation must park before the direct adapter borrows its source");
        match &mut *source_guard {
            Value::String(source) => *source = "true".to_string(),
            other => panic!("expected direct JSON source str, found {other:?}"),
        }
        drop(source_guard);

        drop(first_reservation);
        drop(second_reservation);

        match parse
            .wait_result_with_cancellation_observed(
                Some(StdDuration::from_secs(2)),
                None,
            )
            .expect("the bounded parse wait should be representable")
        {
            TaskWaitStatus::Ready(Ok(Value::EnumVariant(variant)))
                if variant.enum_name == "Result"
                    && variant.variant_name == "Ok"
                    && matches!(
                        variant.payloads.as_slice(),
                        [Value::EnumVariant(parsed)]
                            if parsed.enum_name == "json.Value"
                                && parsed.variant_name == "Bool"
                                && parsed.payloads == vec![Value::Bool(true)]
                    ) => {}
            other => panic!(
                "the admitted parse must copy the source after capacity becomes available, found {other:?}"
            ),
        }
        Ok(Value::Unit)
    });

    assert_eq!(
        result.expect("the direct JSON admission probe should complete"),
        Value::Unit
    );
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "the direct JSON adapter must borrow rather than clone the opaque source value"
    );
    unsafe {
        super::with_value(source, |value| {
            assert_eq!(value, &Value::String("true".to_string()))
        });
        release_value(source);
    }
}

#[test]
fn direct_bytes_adapter_propagates_materialization_allocation_failure_as_au4005() {
    let source = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: vec![Value::Int(
            IntegerValue::from_typed_unsigned(0xab, IntegerKind::Uint8)
                .expect("the test byte fits uint8"),
        )],
    }));
    let source_elements = unsafe {
        super::with_value(source, |value| match value {
            Value::Vec(value) => value.elements.as_ptr(),
            other => panic!("expected list[uint8], found {other:?}"),
        })
    };
    let args = super::DirectHostArgBuffer {
        handles: vec![source as i64],
    };

    let error = crate::runtime_value::with_bytes_runtime_allocation_budget(0, || {
        super::evaluate_direct_bytes_host_builtin("bytes::hex_encode", &args)
    })
    .expect_err("direct byte materialization allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing byte data"
    );
    unsafe {
        super::with_value(source, |value| match value {
            Value::Vec(value) => assert_eq!(value.elements.as_ptr(), source_elements),
            other => panic!("expected list[uint8], found {other:?}"),
        });
    }
}

#[test]
fn direct_bytes_host_ffi_dispatches_without_consuming_borrowed_input() {
    let source = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("uint8"),
        elements: [0x00_u8, 0xab, 0xff]
            .into_iter()
            .map(|byte| {
                Value::Int(
                    IntegerValue::from_typed_unsigned(u128::from(byte), IntegerKind::Uint8)
                        .expect("every test byte fits uint8"),
                )
            })
            .collect(),
    }));
    let source_elements = unsafe {
        super::with_value(source, |value| match value {
            Value::Vec(value) => value.elements.as_ptr(),
            other => panic!("expected list[uint8], found {other:?}"),
        })
    };

    let encoded = direct_host_builtin_call("bytes::hex_encode", &[source]);
    assert_eq!(
        unsafe { take_value(encoded) },
        Value::String("00abff".to_string())
    );
    unsafe {
        super::with_value(source, |value| match value {
            Value::Vec(value) => assert_eq!(value.elements.as_ptr(), source_elements),
            other => panic!("expected list[uint8], found {other:?}"),
        });
        release_value(encoded);
        release_value(source);
    }
}

#[test]
fn direct_json_dump_trap_preserves_borrowed_value_and_copy_indent() {
    let source = direct_json_value(crate::json_codec::JsonValue::Null);
    let indent = direct_option_int(Some(17));
    let source_address = source as usize;
    let indent_address = indent as usize;
    let clone_count = super::direct_value_clone_count();
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = direct_host_builtin_call(
                "json::dumps",
                &[
                    source_address as *mut OpaqueValue,
                    indent_address as *mut OpaqueValue,
                ],
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid JSON indent should fail the active task");
    assert_eq!(error.code, "AU4003");
    assert_eq!(
        error.message,
        "JSON indent must be between 0 and 16, found 17"
    );
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "trapping json.dumps must not clone its borrowed value before evaluation"
    );

    unsafe {
        super::with_value(source, |value| {
            assert!(
                matches!(
                    value,
                    Value::EnumVariant(variant)
                        if variant.enum_name == "json.Value"
                            && variant.variant_name == "Null"
                ),
                "json.dumps must preserve its borrowed value when dumping traps"
            )
        });
        super::with_value(indent, |value| {
            assert_eq!(
                value.render(),
                "Option.Some(17)",
                "json.dumps must preserve its copy-valued indent when dumping traps"
            )
        });
        release_value(source);
        release_value(indent);
    }
}

fn capture_runtime_diagnostic(f: impl FnOnce() + panic::UnwindSafe) -> Diagnostic {
    let payload = panic::catch_unwind(|| super::with_task_runtime_error_capture(f))
        .expect_err("runtime error should be captured as a panic");
    payload
        .downcast_ref::<crate::runtime_value::LightweightTaskFailureSignal>()
        .map(|signal| signal.0.clone())
        .unwrap_or_else(|| panic!("unexpected panic payload"))
}

fn capture_runtime_error_message(f: impl FnOnce() + panic::UnwindSafe) -> String {
    capture_runtime_diagnostic(f).message
}

#[test]
fn direct_binary_shift_preserves_the_structured_code_and_source_span() {
    let diagnostic = capture_direct_boundary_diagnostic(|| {
        let int8 = |value| {
            boxed_value(Value::Int(
                IntegerValue::from_typed_signed(value, IntegerKind::Int8)
                    .expect("test value should fit int8"),
            ))
        };
        super::aura_direct_binary_value_at(19, int8(1), int8(-1), 0, 6, 11);
    });

    assert_eq!(diagnostic.code, "AU4002");
    assert_eq!(
        diagnostic.message,
        "integer shift count `-1` is outside the required range `0..8`"
    );
    assert_eq!(diagnostic.span, Some(Span::new(6, 11)));
}

fn capture_direct_boundary_diagnostic(work: impl FnOnce() + Send + 'static) -> Diagnostic {
    run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| {
                work();
                Ok(Value::Unit)
            })
        })
    })
    .expect_err("the exported direct-runtime boundary should fail")
}

fn capture_direct_boundary_error_message(work: impl FnOnce() + Send + 'static) -> String {
    capture_direct_boundary_diagnostic(work).message
}

fn expect_option_none(ptr: *mut OpaqueValue) {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None" => {}
        other => panic!("expected Option.None, found {:?}", other),
    }
}

fn expect_variant_value(value: Value, enum_name: &str, variant_name: &str) -> Vec<Value> {
    match value {
        Value::EnumVariant(variant)
            if variant.enum_name == enum_name && variant.variant_name == variant_name =>
        {
            variant.payloads
        }
        other => panic!("expected {}.{}, found {:?}", enum_name, variant_name, other),
    }
}

fn expect_variant_ptr(ptr: *mut OpaqueValue, enum_name: &str, variant_name: &str) -> Vec<Value> {
    expect_variant_value(unsafe { take_value(ptr) }, enum_name, variant_name)
}

fn expect_queue_receive_item_int(ptr: *mut OpaqueValue) -> i128 {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "QueueReceive" && variant.variant_name == "Item" =>
        {
            match variant
                .single_payload()
                .expect("expected queue receive payload")
            {
                Value::Int(value) => value.as_i128().expect("expected signed integer"),
                other => panic!("expected int payload, found {:?}", other),
            }
        }
        other => panic!("expected QueueReceive.Item(int), found {:?}", other),
    }
}

fn expect_queue_receive_closed(ptr: *mut OpaqueValue) {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "QueueReceive" && variant.variant_name == "Closed" => {}
        other => panic!("expected QueueReceive.Closed, found {:?}", other),
    }
}

fn expect_result_ok_int(ptr: *mut OpaqueValue) -> i128 {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::Int(value) => value.as_i128().expect("expected signed integer"),
                other => panic!("expected int payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(int), found {:?}", other),
    }
}

fn expect_result_ok_float(ptr: *mut OpaqueValue) -> f64 {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::Float(value) => *value,
                other => panic!("expected float payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(float), found {:?}", other),
    }
}

fn expect_result_ok_unit(ptr: *mut OpaqueValue) {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::Unit => {}
                other => panic!("expected unit payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(unit), found {:?}", other),
    }
}

fn expect_result_ok_string(ptr: *mut OpaqueValue) -> String {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::String(text) => text.to_string(),
                other => panic!("expected string payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(str), found {:?}", other),
    }
}

fn expect_result_ok_payload(ptr: *mut OpaqueValue) -> Value {
    let mut payloads = expect_variant_ptr(ptr, "Result", "Ok");
    assert_eq!(payloads.len(), 1, "expected one Result.Ok payload");
    payloads.remove(0)
}

fn expect_result_err_payload(ptr: *mut OpaqueValue) -> Value {
    let mut payloads = expect_variant_ptr(ptr, "Result", "Err");
    assert_eq!(payloads.len(), 1, "expected one Result.Err payload");
    payloads.remove(0)
}

fn expect_process_invalid_input(value: Value) {
    let mut io_payloads = expect_variant_value(value, "Error", "Io");
    assert_eq!(io_payloads.len(), 1);
    assert!(expect_variant_value(io_payloads.remove(0), "io.Error", "InvalidInput").is_empty());
}

fn expect_option_some_payload(value: Value) -> Value {
    match value {
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "Some" =>
        {
            let mut payloads = variant.payloads;
            assert_eq!(payloads.len(), 1, "expected one Option.Some payload");
            payloads.remove(0)
        }
        other => panic!("expected Option.Some(...), found {:?}", other),
    }
}

fn expect_result_ok_vec_ints(ptr: *mut OpaqueValue) -> Vec<i128> {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::Vec(values) => values
                    .elements
                    .iter()
                    .map(|value| match value {
                        Value::Int(value) => value.as_i128().expect("expected signed integer"),
                        other => panic!("expected int element, found {:?}", other),
                    })
                    .collect(),
                other => panic!("expected vec payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(list[int]), found {:?}", other),
    }
}

fn expect_result_ok_vec_strings(ptr: *mut OpaqueValue) -> Vec<String> {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::Vec(values) => values
                    .elements
                    .iter()
                    .map(|value| match value {
                        Value::String(text) => text.to_string(),
                        other => panic!("expected string element, found {:?}", other),
                    })
                    .collect(),
                other => panic!("expected vec payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Ok(list[str]), found {:?}", other),
    }
}

fn string_map(entries: &[(&str, &str)]) -> *mut OpaqueValue {
    let map = super::aura_direct_map_empty();
    for (key, value) in entries {
        expect_option_none(super::aura_direct_map_set_in_place(
            map,
            string_value(key),
            string_value(value),
        ));
    }
    map
}

fn expect_result_err_string(ptr: *mut OpaqueValue) -> String {
    match unsafe { take_value(ptr) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Err" =>
        {
            match variant.single_payload().expect("expected result payload") {
                Value::String(text) => text.to_string(),
                other => panic!("expected string payload, found {:?}", other),
            }
        }
        other => panic!("expected Result.Err(str), found {:?}", other),
    }
}

#[test]
fn render_bool_uses_aura_boolean_strings() {
    assert_eq!(render_bool(0), "false");
    assert_eq!(render_bool(1), "true");
    assert_eq!(render_bool(99), "true");
}

#[test]
fn direct_duration_literal_reconstructs_signed_i128_from_low_then_high_limbs() {
    for (expected, low, high) in [
        (0, 0, 0),
        (-1, -1, -1),
        ((i64::MAX as i128) + 1, i64::MIN, 0),
        (i128::MAX, -1, i64::MAX),
        (i128::MIN, 0, i64::MIN),
    ] {
        assert_eq!(
            unsafe { take_value(super::aura_direct_duration_literal(low, high)) },
            Value::Duration(expected),
            "duration limbs low={low} high={high}"
        );
    }
}

#[test]
fn direct_duration_runtime_surface_is_checked_exact_and_uses_floor_division() {
    let duration = |value| Value::Duration(value);
    let integer = |value| Value::Int(IntegerValue::from_signed(value));

    for (op, left, right, expected) in [
        (BinaryOp::Add, duration(9), duration(4), duration(13)),
        (BinaryOp::Sub, duration(9), duration(14), duration(-5)),
        (BinaryOp::Mul, duration(7), integer(-3), duration(-21)),
        (BinaryOp::Mul, integer(-3), duration(7), duration(-21)),
        (BinaryOp::FloorDiv, duration(-5), integer(2), duration(-3)),
        (BinaryOp::FloorDiv, duration(5), integer(-2), duration(-3)),
        (BinaryOp::FloorDiv, duration(6), integer(3), duration(2)),
    ] {
        assert_eq!(
            super::eval_binary_value(left, right, op).expect("Duration operation should succeed"),
            expected
        );
    }

    for (op, expected) in [
        (BinaryOp::Eq, false),
        (BinaryOp::NotEq, true),
        (BinaryOp::Less, true),
        (BinaryOp::LessEq, true),
        (BinaryOp::Greater, false),
        (BinaryOp::GreaterEq, false),
    ] {
        assert_eq!(
            super::eval_binary_value(duration(4), duration(5), op)
                .expect("Duration comparison should succeed"),
            Value::Bool(expected)
        );
    }

    for (op, left, right, expected) in [
        (
            BinaryOp::Add,
            duration(i128::MAX),
            duration(1),
            "duration overflow",
        ),
        (
            BinaryOp::Sub,
            duration(i128::MIN),
            duration(1),
            "duration overflow",
        ),
        (
            BinaryOp::Mul,
            duration(i128::MAX),
            integer(2),
            "duration overflow",
        ),
        (
            BinaryOp::FloorDiv,
            duration(1),
            integer(0),
            "division by zero",
        ),
        (
            BinaryOp::FloorDiv,
            duration(i128::MIN),
            integer(-1),
            "duration overflow",
        ),
    ] {
        assert_eq!(
            super::eval_binary_value(left, right, op)
                .expect_err("invalid Duration operation should fail")
                .message,
            expected
        );
    }

    for (value, unit, expected) in [
        (
            2,
            crate::runtime_value::NANOS_PER_MILLISECOND as i64,
            2_000_000,
        ),
        (
            -3,
            crate::runtime_value::NANOS_PER_SECOND as i64,
            -3_000_000_000,
        ),
        (
            4,
            crate::runtime_value::NANOS_PER_MINUTE as i64,
            240_000_000_000,
        ),
    ] {
        assert_eq!(
            unsafe { take_value(super::aura_direct_duration_from_i64(value, unit)) },
            duration(expected)
        );
    }
    assert_eq!(
        super::aura_direct_duration_to_float(
            duration_nanoseconds_value(1_500_000),
            crate::runtime_value::NANOS_PER_MILLISECOND as i64,
        ),
        1.5
    );
    assert_eq!(
        super::aura_direct_duration_to_float(
            duration_nanoseconds_value(-1_500_000_000),
            crate::runtime_value::NANOS_PER_SECOND as i64,
        ),
        -1.5
    );

    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_duration_from_i64(1, 7);
            Ok(Value::Unit)
        })
    })
    .expect_err("unknown Duration constructor units should fail the active task");
    assert!(error
        .message
        .contains("unknown Duration constructor unit `7`"));

    let duration_ptr = duration_nanoseconds_value(1);
    let duration_address = duration_ptr as usize;
    let error = run_lightweight_root_task(move || {
        let duration_ptr = duration_address as *mut super::OpaqueValue;
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_duration_to_float(duration_ptr, 7);
            Ok(Value::Unit)
        })
    })
    .expect_err("unknown Duration conversion units should fail the active task");
    assert!(error
        .message
        .contains("unknown Duration conversion unit `7`"));
    unsafe { release_value(duration_ptr) };

    let string_ptr = string_value("1ms");
    let string_address = string_ptr as usize;
    let error = run_lightweight_root_task(move || {
        let string_ptr = string_address as *mut super::OpaqueValue;
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_duration_to_float(
                string_ptr,
                crate::runtime_value::NANOS_PER_MILLISECOND as i64,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("non-Duration conversions should fail the active task");
    assert!(error.message.contains("expected `Duration`, found `str`"));
    unsafe { release_value(string_ptr) };
}

#[test]
fn direct_random_runtime_is_deterministic_borrowed_and_mutates_vectors_in_place() {
    let integers = super::aura_direct_rng_new(42);
    assert_eq!(
        super::aura_direct_value_type_matches(integers, b"random.Rng".as_ptr(), "random.Rng".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(integers, b"Rng".as_ptr(), "Rng".len()),
        0,
        "a builtin generator must not satisfy an unrelated bare user type named Rng",
    );
    assert_eq!(super::aura_direct_rng_next_int(integers, 0, 10), 2);
    assert_eq!(super::aura_direct_rng_next_int(integers, -5, 6), 2);
    assert_eq!(
        super::aura_direct_rng_next_int(integers, i64::MIN, i64::MAX),
        3_321_214_725_393_783_201
    );
    assert_eq!(super::aura_direct_rng_next_int(integers, 7, 8), 7);

    let retained = unsafe { retain_value(integers) };
    unsafe { release_value(integers) };
    assert!((0..10).contains(&super::aura_direct_rng_next_int(retained, 0, 10)));
    unsafe { release_value(retained) };

    let floats = super::aura_direct_rng_new(42);
    assert_eq!(
        super::aura_direct_rng_next_float(floats),
        0.083_862_971_059_882_16
    );
    assert_eq!(
        super::aura_direct_rng_next_float(floats),
        0.378_980_250_662_668_6
    );
    unsafe { release_value(floats) };

    let shuffle_rng = super::aura_direct_rng_new(42);
    let values = string_vec(&["a", "b", "c", "d", "e", "f"]);
    super::aura_direct_rng_shuffle(shuffle_rng, values);
    match unsafe { super::value_ref(values) } {
        Value::Vec(vector) => assert_eq!(
            vector
                .elements
                .into_iter()
                .map(|value| value.render())
                .collect::<Vec<_>>(),
            ["d", "f", "e", "b", "c", "a"]
        ),
        other => panic!("expected shuffled vector, found {other:?}"),
    }
    unsafe {
        release_value(values);
        release_value(shuffle_rng);
    }

    assert_eq!(super::aura_direct_random_secure_int(5, 6), 5);
    for count in [0, 8] {
        let bytes = super::aura_direct_random_secure_bytes(count);
        match unsafe { super::value_ref(bytes) } {
            Value::Vec(vector) => {
                assert_eq!(vector.element_type, Type::named("uint8"));
                assert_eq!(vector.elements.len(), count as usize);
            }
            other => panic!("expected secure bytes, found {other:?}"),
        }
        unsafe { release_value(bytes) };
    }

    let invalid_rng = super::aura_direct_rng_new(42);
    let invalid_rng_address = invalid_rng as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_rng_next_int(invalid_rng_address as *mut OpaqueValue, 4, 4);
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid deterministic bounds should fail the active task");
    unsafe { release_value(invalid_rng) };
    assert_eq!(error.code, "AU4003");
    assert_eq!(
        error.message,
        "random bounds require `lo < hi`, found `4 >= 4`"
    );
}

#[test]
fn direct_random_runtime_rejects_wrong_receivers_and_shuffle_targets() {
    let integer_receiver = string_value("not a generator");
    let integer_receiver_address = integer_receiver as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_rng_next_int(
                integer_receiver_address as *mut OpaqueValue,
                0,
                10,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("next_int on a non-generator should fail the active task");
    unsafe { release_value(integer_receiver) };
    assert_eq!(error.message, "expected `random.Rng`, found `str`");

    let float_receiver = string_value("not a generator");
    let float_receiver_address = float_receiver as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_rng_next_float(float_receiver_address as *mut OpaqueValue);
            Ok(Value::Unit)
        })
    })
    .expect_err("next_float on a non-generator should fail the active task");
    unsafe { release_value(float_receiver) };
    assert_eq!(error.message, "expected `random.Rng`, found `str`");

    let shuffle_receiver = string_value("not a generator");
    let shuffle_receiver_address = shuffle_receiver as usize;
    let values = string_vec(&["a", "b"]);
    let values_address = values as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_rng_shuffle(
                shuffle_receiver_address as *mut OpaqueValue,
                values_address as *mut OpaqueValue,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("shuffle on a non-generator should fail the active task");
    unsafe {
        release_value(shuffle_receiver);
        release_value(values);
    }
    assert_eq!(error.message, "expected `random.Rng`, found `str`");

    let shuffle_rng = super::aura_direct_rng_new(42);
    let shuffle_rng_address = shuffle_rng as usize;
    let non_vector = super::aura_direct_rng_new(7);
    let non_vector_address = non_vector as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_rng_shuffle(
                shuffle_rng_address as *mut OpaqueValue,
                non_vector_address as *mut OpaqueValue,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("shuffle with a non-vector target should fail the active task");
    unsafe {
        release_value(shuffle_rng);
        release_value(non_vector);
    }
    assert_eq!(error.message, "expected `list`, found `random.Rng`");
}

#[test]
fn direct_secure_random_runtime_preserves_validation_and_resource_diagnostics() {
    let invalid_bounds = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_random_secure_int(8, 8);
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid secure random bounds should fail the active task");
    assert_eq!(invalid_bounds.code, "AU4003");
    assert_eq!(
        invalid_bounds.message,
        "random bounds require `lo < hi`, found `8 >= 8`"
    );

    let negative_count = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_random_secure_bytes(-1);
            Ok(Value::Unit)
        })
    })
    .expect_err("negative secure byte counts should fail the active task");
    assert_eq!(negative_count.code, "AU4003");
    assert_eq!(
        negative_count.message,
        "`random.secure_bytes(n)` requires a non-negative byte count, found `-1`"
    );

    let over_ceiling_count = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_random_secure_bytes(i64::from(i32::MAX) + 1);
            Ok(Value::Unit)
        })
    })
    .expect_err("secure byte counts above the request ceiling should fail the active task");
    assert_eq!(over_ceiling_count.code, "AU4005");
    assert_eq!(
        over_ceiling_count.message,
        "`random.secure_bytes(n)` count `2147483648` exceeds the secure-random request ceiling `2147483647`"
    );

    let allocation = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_random_secure_bytes(i64::MAX);
            Ok(Value::Unit)
        })
    })
    .expect_err("an impossible secure byte count should fail the active task");
    assert_eq!(allocation.code, "AU4005");
    assert_eq!(
        allocation.message,
        "`random.secure_bytes(n)` count `9223372036854775807` exceeds the secure-random request ceiling `2147483647`"
    );

    let host_allocation_error = Vec::<u8>::new()
        .try_reserve_exact(usize::MAX)
        .expect_err("usize::MAX bytes must exceed the host allocation domain");
    let mapped_allocation = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            super::direct_random_resource_error(
                SecureRandomError::Allocation(host_allocation_error),
                None,
            )
        })
    })
    .expect_err("host allocation failure should fail the active task");
    assert_eq!(mapped_allocation.code, "AU4005");
    assert!(
        mapped_allocation
            .message
            .starts_with("secure random allocation failed:"),
        "unexpected allocation diagnostic: {}",
        mapped_allocation.message
    );

    let entropy = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            super::direct_random_resource_error(
                SecureRandomError::Entropy(getrandom::Error::UNSUPPORTED),
                None,
            )
        })
    })
    .expect_err("an unavailable OS random source should fail the active task");
    assert_eq!(entropy.code, "AU4005");
    assert_eq!(
        entropy.message,
        format!(
            "operating-system random source failed: {}",
            getrandom::Error::UNSUPPORTED
        )
    );
}

#[test]
fn direct_duration_task_deadlines_preserve_ready_error_and_timeout_outcomes() {
    let ready = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(11)))
    }))));
    assert_eq!(
        expect_task_result_ready_int(super::aura_direct_task_join_timeout_value(
            ready,
            duration_value(1_000),
        )),
        11
    );
    unsafe { release_value(ready) };

    let failed = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        Err(Diagnostic::new("duration task failed"))
    }))));
    assert_eq!(
        expect_task_result_error_message(super::aura_direct_task_join_timeout_value(
            failed,
            duration_value(1_000),
        )),
        "duration task failed"
    );
    unsafe { release_value(failed) };

    let delayed = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(25));
        Ok(Value::Unit)
    }))));
    assert!(expect_variant_ptr(
        super::aura_direct_task_join_timeout_value(delayed, duration_value(0)),
        "TaskResult",
        "TimedOut",
    )
    .is_empty());
    expect_variant_ptr(
        super::aura_direct_task_join_timeout_value(delayed, duration_value(1_000)),
        "TaskResult",
        "Ready",
    );
    unsafe { release_value(delayed) };
}

#[test]
fn int32_overflow_message_mentions_value_and_type() {
    assert_eq!(
        int32_overflow_message(123),
        "integer value `123` does not fit in `int32`"
    );
}

#[test]
fn render_float_preserves_whole_number_fraction() {
    assert_eq!(render_float(42.0), "42.0");
    assert_eq!(render_float(3.5), "3.5");
}

#[test]
fn render_float_uses_each_source_types_shortest_roundtrip_spelling() {
    assert_eq!(render_float(9_007_199_254_740_992.0), "9007199254740992.0");
    assert_eq!(render_float(1e300), "1e300");
    assert_eq!(render_float(1e-300), "1e-300");
    assert_eq!(render_float(-0.0), "-0.0");

    let float32_value = 834.6_f32;
    assert_eq!(render_float32(float32_value), "834.6");
}

#[test]
fn render_float_covers_nonfinite_and_full_precision_values() {
    assert_eq!(render_float(f64::INFINITY), "inf");
    let precise = std::f64::consts::PI;
    assert_eq!(render_float(precise), precise.to_string());
}

#[test]
fn native_runtime_operator_helpers_cover_comparison_binary_and_unary_error_edges() {
    assert_eq!(
        super::compare_values(
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(2)),
            BinaryOp::Less,
        )
        .expect("int comparisons should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        super::compare_values(Value::Float(2.5), Value::Float(2.5), BinaryOp::GreaterEq,)
            .expect("float comparisons should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        super::compare_values(
            Value::String("ada".to_string()),
            Value::String("grace".to_string()),
            BinaryOp::Less,
        )
        .expect("string ordering should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        super::compare_values(Value::Unit, Value::Unit, BinaryOp::Eq)
            .expect("unit equality should succeed"),
        Value::Bool(true)
    );
    assert!(super::compare_values(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::from_signed(2)),
        BinaryOp::Add,
    )
    .expect_err("non-comparison int ops should fail in compare_values")
    .message
    .contains("unsupported comparison operator"));
    assert!(
        super::compare_values(Value::Float(1.0), Value::Float(2.0), BinaryOp::Add,)
            .expect_err("non-comparison float ops should fail in compare_values")
            .message
            .contains("unsupported comparison operator")
    );
    assert!(super::compare_values(
        Value::String("a".to_string()),
        Value::String("b".to_string()),
        BinaryOp::Add,
    )
    .expect_err("non-comparison string ops should fail in compare_values")
    .message
    .contains("unsupported comparison operator"));
    assert!(super::compare_values(
        Value::Bool(true),
        Value::String("b".to_string()),
        BinaryOp::Less,
    )
    .expect_err("mismatched comparisons should fail")
    .message
    .contains("unsupported comparison"));

    assert_eq!(
        super::eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::And)
            .expect("bool and should succeed"),
        Value::Bool(false)
    );
    assert_eq!(
        super::eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::Or)
            .expect("bool or should succeed"),
        Value::Bool(true)
    );
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::And,
    )
    .expect_err("logical and should reject non-bool rhs")
    .message
    .contains("logical `and` expects bool operands"));
    assert!(super::eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Bool(false),
        BinaryOp::Or,
    )
    .expect_err("logical or should reject non-bool lhs")
    .message
    .contains("logical `or` expects bool operands"));
    assert_eq!(
        super::eval_binary_value(
            Value::String("aura".to_string()),
            Value::String(" repo".to_string()),
            BinaryOp::Add,
        )
        .expect("string concat should succeed"),
        Value::String("aura repo".to_string())
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(9.0), Value::Float(4.0), BinaryOp::Div,)
            .expect("float division should succeed"),
        Value::Float(2.25)
    );
    assert!(
        super::eval_binary_value(Value::Float(9.0), Value::Float(0.0), BinaryOp::Div,)
            .expect_err("float division by zero should fail")
            .message
            .contains("division by zero")
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(9.0), Value::Float(4.0), BinaryOp::Mod,)
            .expect("float modulo should succeed"),
        Value::Float(1.0)
    );
    assert_eq!(
        super::eval_binary_value(
            Value::Int(IntegerValue::from_signed(-7)),
            Value::Int(IntegerValue::from_signed(3)),
            BinaryOp::FloorDiv,
        )
        .expect("integer floor division should round toward negative infinity"),
        Value::Int(IntegerValue::from_signed(-3))
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(7.5), Value::Float(-2.0), BinaryOp::FloorDiv,)
            .expect("float floor division should round toward negative infinity"),
        Value::Float(-4.0)
    );
    assert_eq!(
        super::eval_binary_value(
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(0)),
            BinaryOp::FloorDiv,
        )
        .expect_err("integer floor division by zero should fail")
        .message,
        "division by zero"
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(1.0), Value::Float(0.0), BinaryOp::FloorDiv)
            .expect_err("float floor division by zero should fail")
            .message,
        "division by zero"
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(1.0), Value::Float(-0.0), BinaryOp::FloorDiv)
            .expect_err("float floor division by negative zero should fail")
            .message,
        "division by zero"
    );
    assert_eq!(
        super::eval_binary_value(Value::Float(1.0), Value::Float(-0.0), BinaryOp::Mod)
            .expect_err("float remainder by negative zero should fail")
            .message,
        "division by zero"
    );
    assert!(super::eval_binary_value(
        Value::Int(IntegerValue::from_literal(u128::MAX)),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Add,
    )
    .expect_err("checked int add should report overflow")
    .message
    .contains("integer overflow"));
    assert!(super::eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::from_signed(0)),
        BinaryOp::Div,
    )
    .expect_err("int division by zero should fail")
    .message
    .contains("division by zero"));
    assert!(super::eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::from_signed(0)),
        BinaryOp::Mod,
    )
    .expect_err("int modulo by zero should fail")
    .message
    .contains("division by zero"));
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Add,
    )
    .expect_err("unsupported add operands should fail")
    .message
    .contains("unsupported `+` operands"));
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Sub,
    )
    .expect_err("unsupported sub operands should fail")
    .message
    .contains("unsupported `-` operands"));
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Mul,
    )
    .expect_err("unsupported mul operands should fail")
    .message
    .contains("unsupported `*` operands"));
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Div,
    )
    .expect_err("unsupported div operands should fail")
    .message
    .contains("unsupported `/` operands"));
    assert!(super::eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Mod,
    )
    .expect_err("unsupported mod operands should fail")
    .message
    .contains("unsupported `%` operands"));

    assert_eq!(
        super::eval_unary_value(Value::Bool(false), UnaryOp::Not).expect("bool not should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        super::eval_unary_value(Value::Float(3.5), UnaryOp::Neg)
            .expect("float negation should succeed"),
        Value::Float(-3.5)
    );
    assert!(super::eval_unary_value(
        Value::Int(IntegerValue::from_literal((1_u128 << 127) + 1)),
        UnaryOp::Neg,
    )
    .expect_err("minimum signed integer negation should overflow")
    .message
    .contains("integer overflow"));
    assert!(
        super::eval_unary_value(Value::String("x".to_string()), UnaryOp::Not)
            .expect_err("logical not should reject non-bools")
            .message
            .contains("expects `bool`")
    );
    assert!(
        super::eval_unary_value(Value::String("x".to_string()), UnaryOp::Neg)
            .expect_err("unary minus should reject non-numerics")
            .message
            .contains("expects a numeric value")
    );
}

#[test]
fn native_runtime_timeout_and_option_decoders_cover_error_edges() {
    assert_eq!(super::extract_duration_nanoseconds(Value::Duration(42)), 42);
    let message = capture_runtime_error_message(|| {
        let _ = super::extract_duration_nanoseconds(Value::Int(IntegerValue::from_literal(
            (i128::MAX as u128) + 1,
        )));
    });
    assert!(message.contains("outside signed timer range"));
    let message = capture_runtime_error_message(|| {
        let _ = super::extract_duration_nanoseconds(Value::String("soon".to_string()));
    });
    assert!(message.contains("expected `Duration`"));

    let invalid_utf8 = [0xff_u8];
    let message = capture_runtime_error_message(|| {
        let _ = super::decode_bytes(invalid_utf8.as_ptr(), invalid_utf8.len());
    });
    assert!(message.contains("invalid UTF-8"));

    let mut null_payloads = vec![0_i64].into_boxed_slice();
    let null_payloads_ptr = null_payloads.as_mut_ptr();
    let null_payloads_len = null_payloads.len();
    std::mem::forget(null_payloads);
    let message = capture_runtime_error_message(|| unsafe {
        let _ = super::consume_opaque_buffer(null_payloads_ptr, null_payloads_len);
    });
    assert!(message.contains("null enum payload handle"));

    let cleanup_value = int_value(9);
    let mut cleanup_args = vec![cleanup_value as i64].into_boxed_slice();
    let cleanup_args_ptr = cleanup_args.as_mut_ptr();
    let cleanup_args_len = cleanup_args.len();
    std::mem::forget(cleanup_args);
    unsafe {
        super::release_direct_cleanup_args(cleanup_args_ptr, cleanup_args_len);
    }
    unsafe {
        super::release_direct_cleanup_args(std::ptr::null_mut(), 1);
    }
    let mut zero_cleanup_args = vec![0_i64].into_boxed_slice();
    let zero_cleanup_args_ptr = zero_cleanup_args.as_mut_ptr();
    let zero_cleanup_args_len = zero_cleanup_args.len();
    std::mem::forget(zero_cleanup_args);
    unsafe {
        super::release_direct_cleanup_args(zero_cleanup_args_ptr, zero_cleanup_args_len);
    }

    assert_eq!(
        super::optional_timeout_from_ptr(std::ptr::null_mut(), "timeout"),
        None
    );
    assert_eq!(
        super::process_optional_timeout_from_ptr(std::ptr::null_mut(), "timeout"),
        None
    );

    let unit = boxed_value(Value::Unit);
    assert_eq!(super::optional_timeout_from_ptr(unit, "timeout"), None);
    assert_eq!(
        super::process_optional_timeout_from_ptr(unit, "timeout"),
        None
    );
    unsafe { release_value(unit) };

    let duration = duration_value(25);
    assert_eq!(
        super::optional_timeout_from_ptr(duration, "timeout"),
        Some(StdDuration::from_millis(25))
    );
    assert_eq!(
        super::process_optional_timeout_from_ptr(duration, "timeout"),
        Some(StdDuration::from_millis(25))
    );
    unsafe { release_value(duration) };

    let negative_timeout = duration_value(-1);
    let message = capture_runtime_error_message(|| {
        let _ = super::optional_timeout_from_ptr(negative_timeout, "timeout");
    });
    assert!(message.contains("must be non-negative"));
    unsafe { release_value(negative_timeout) };

    let open_ended_timeout = duration_value(-1);
    let message = capture_runtime_error_message(|| {
        let _ = super::process_optional_timeout_from_ptr(open_ended_timeout, "timeout");
    });
    assert!(message.contains("must be non-negative"));
    unsafe { release_value(open_ended_timeout) };

    let huge_timeout = boxed_value(Value::Duration(i128::MAX));
    let message = capture_runtime_error_message(|| {
        let _ = super::process_optional_timeout_from_ptr(huge_timeout, "timeout");
    });
    assert!(message.contains("host timer range") || message.contains("host deadline range"));
    unsafe { release_value(huge_timeout) };

    let wrong_timeout = boxed_value(Value::String("soon".to_string()));
    let message = capture_runtime_error_message(|| {
        let _ = super::optional_timeout_from_ptr(wrong_timeout, "timeout");
    });
    assert!(message.contains("expects `Duration`"));
    unsafe { release_value(wrong_timeout) };

    let wrong_duration = boxed_value(Value::String("soon".to_string()));
    let message = capture_runtime_error_message(|| {
        let _ = super::duration_from_ptr(wrong_duration, "sleep");
    });
    assert!(message.contains("expects `Duration`"));
    unsafe { release_value(wrong_duration) };

    let invalid_restarts = int_value(-2);
    let message = capture_runtime_error_message(|| {
        let _ = super::supervisor_max_restarts_from_ptr(invalid_restarts, "supervisor");
    });
    assert!(message.contains("max_restarts"));
    unsafe { release_value(invalid_restarts) };

    assert_eq!(
        super::expect_command_vec(
            &Value::Vec(VecValue {
                element_type: crate::sema::Type::named("Unknown"),
                elements: vec![Value::String("echo".to_string())],
            }),
            "command",
        ),
        vec!["echo".to_string()]
    );
    let message = capture_runtime_error_message(|| {
        let _ = super::expect_command_vec(
            &Value::Vec(VecValue {
                element_type: crate::sema::Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            }),
            "command",
        );
    });
    assert!(message.contains("expects `list[str]`"));

    assert_eq!(
        super::expect_optional_string_value(&Value::Unit, "stderr"),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(
            &Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "None".to_string(),
                payloads: vec![],
            }),
            "stderr",
        ),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(
            &Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: vec![Value::String("log".to_string())],
            }),
            "stderr",
        ),
        Some("log".to_string())
    );
    let message = capture_runtime_error_message(|| {
        let _ = super::expect_optional_string_value(
            &Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: vec![],
            }),
            "stderr",
        );
    });
    assert!(message.contains("malformed option payload"));
    let message = capture_runtime_error_message(|| {
        let _ = super::expect_optional_string_value(&Value::Bool(true), "stderr");
    });
    assert!(message.contains("expects `Option[str]`"));
}

#[test]
fn invalid_direct_host_timers_use_typed_io_and_process_errors() {
    let io_error = expect_result_err_payload(super::aura_direct_net_connect_timeout(
        string_value("127.0.0.1:9"),
        duration_value(-1),
    ));
    assert!(expect_variant_value(io_error, "io.Error", "InvalidInput").is_empty());

    let process_error = expect_result_err_payload(super::aura_direct_process_run(
        string_vec(&["/bin/true"]),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        duration_value(-1),
        bool_value(false),
    ));
    let mut io_payloads = expect_variant_value(process_error, "Error", "Io");
    assert_eq!(io_payloads.len(), 1);
    assert!(expect_variant_value(io_payloads.remove(0), "io.Error", "InvalidInput").is_empty());

    let placeholder_child = boxed_value(Value::Unit);
    let mut wait_payloads = expect_variant_ptr(
        super::aura_direct_process_child_wait(placeholder_child, duration_value(-1)),
        "Wait",
        "Failed",
    );
    assert_eq!(wait_payloads.len(), 1);
    expect_process_invalid_input(wait_payloads.remove(0));
    unsafe { release_value(placeholder_child) };

    let supervisor = boxed_value(Value::ProcessSupervisor(ProcessSupervisorValue::new()));
    let name = string_value("invalid-backoff");
    let command = string_vec(&["/bin/true"]);
    let cwd = boxed_value(Value::Unit);
    let env = super::aura_direct_map_empty();
    let stdin = super::aura_direct_process_null();
    let stdout = super::aura_direct_process_null();
    let stderr = super::aura_direct_process_null();
    let restart = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "process.RestartPolicy".to_string(),
        variant_name: "Never".to_string(),
        payloads: Vec::new(),
    }));
    let backoff = duration_value(-1);
    let max_restarts = int_value(-1);
    let group = bool_value(false);
    expect_process_invalid_input(expect_result_err_payload(
        super::aura_direct_process_supervisor_start(
            supervisor,
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
        ),
    ));
    let mut wait_payloads = expect_variant_ptr(
        super::aura_direct_process_supervisor_wait(supervisor, duration_value(-1)),
        "SupervisorWait",
        "Event",
    );
    assert_eq!(wait_payloads.len(), 1);
    let mut failed_payloads =
        expect_variant_value(wait_payloads.remove(0), "SupervisorEvent", "Failed");
    assert_eq!(failed_payloads.len(), 3);
    assert_eq!(
        failed_payloads.remove(0),
        Value::String("<supervisor>".to_string())
    );
    let mut process_io_payloads = expect_variant_value(failed_payloads.remove(0), "Error", "Io");
    assert_eq!(process_io_payloads.len(), 1);
    assert!(
        expect_variant_value(process_io_payloads.remove(0), "io.Error", "InvalidInput",).is_empty()
    );
    assert_eq!(
        failed_payloads.remove(0),
        Value::Int(IntegerValue::from_signed(0))
    );

    let wait_or_none_error = expect_result_err_payload(
        super::aura_direct_process_supervisor_wait_or_none(supervisor, duration_value(-1)),
    );
    let mut process_io_payloads = expect_variant_value(wait_or_none_error, "Error", "Io");
    assert_eq!(process_io_payloads.len(), 1);
    assert!(
        expect_variant_value(process_io_payloads.remove(0), "io.Error", "InvalidInput",).is_empty()
    );
    unsafe { release_value(supervisor) };
}

#[test]
fn direct_wait_deadline_helper_rejects_overflow_instead_of_becoming_unlimited() {
    let now = Instant::now();
    assert_eq!(
        super::checked_timeout_deadline_at(None, now, "wait_any timeout")
            .expect("an omitted timeout should remain unlimited"),
        None
    );
    assert_eq!(
        super::checked_timeout_deadline_at(Some(StdDuration::ZERO), now, "wait_all timeout",)
            .expect("zero should produce an immediate deadline"),
        Some(now)
    );

    for label in ["wait_any(timeout=...)", "wait_all(timeout=...)"] {
        let diagnostic = super::checked_timeout_deadline_at(Some(StdDuration::MAX), now, label)
            .expect_err("an unrepresentable deadline must not become unlimited");
        assert_eq!(
            diagnostic.message,
            format!("{label} exceeds the host deadline range")
        );
        assert_eq!(diagnostic.code, "AU4001");
        assert_eq!(diagnostic.into_runtime_trap().code, "AU4001");
    }
}

#[test]
fn legacy_direct_sleep_maps_deadline_overflow_to_explicit_au4001() {
    let diagnostic = super::checked_sleep_milliseconds_with(i64::MAX, |_| {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sleep duration exceeds the host deadline range",
        ))
    })
    .expect_err("a host deadline overflow must not become an immediate wakeup");
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "sleep duration exceeds the host deadline range"
    );
}

#[test]
fn runtime_init_is_callable() {
    super::aura_direct_runtime_init(
        b"/virtual/test.au".as_ptr(),
        b"/virtual/test.au".len(),
        b"def main() -> int32:\n    return 0\n".as_ptr(),
        b"def main() -> int32:\n    return 0\n".len(),
    );
}

#[test]
fn native_runtime_ref_count_helpers_reject_zero_and_overflow() {
    let released = AtomicUsize::new(1);
    assert!(super::release_ref_count(&released).expect("final release should succeed"));
    assert_eq!(released.load(Ordering::Relaxed), 0);

    let released_retain = AtomicUsize::new(0);
    let retain_after_release_error = super::retain_ref_count(&released_retain)
        .expect_err("retain after release should be rejected");
    assert!(retain_after_release_error.contains("already-released"));

    let overflow = AtomicUsize::new(usize::MAX);
    let overflow_error =
        super::retain_ref_count(&overflow).expect_err("overflow should be rejected");
    assert!(overflow_error.contains("overflow"));
    assert_eq!(overflow.load(Ordering::Relaxed), usize::MAX);

    let zero = AtomicUsize::new(0);
    let underflow_error =
        super::release_ref_count(&zero).expect_err("underflow should be rejected");
    assert!(underflow_error.contains("already-released"));
    assert_eq!(zero.load(Ordering::Relaxed), 0);

    let shared = AtomicUsize::new(2);
    assert!(!super::release_ref_count(&shared).expect("shared release should succeed"));
    assert_eq!(shared.load(Ordering::Relaxed), 1);
    super::retain_ref_count(&shared).expect("retain should succeed");
    assert_eq!(shared.load(Ordering::Relaxed), 2);
}

#[cfg(unix)]
#[test]
fn with_sigpipe_blocked_restores_the_previous_signal_mask_after_broken_pipe() {
    unsafe fn current_sigpipe_blocked() -> bool {
        let mut current: libc::sigset_t = std::mem::zeroed();
        let rc = libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut current);
        assert_eq!(rc, 0, "should read current signal mask");
        libc::sigismember(&current, libc::SIGPIPE) == 1
    }

    let before = unsafe { current_sigpipe_blocked() };
    let error = super::with_sigpipe_blocked(|| {
        Err::<(), _>(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "simulated broken pipe",
        ))
    })
    .expect_err("broken pipe should propagate through helper");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    let after = unsafe { current_sigpipe_blocked() };
    assert_eq!(
        after, before,
        "SIGPIPE mask should be restored after helper returns"
    );
}

#[test]
fn direct_print_helpers_are_callable() {
    super::aura_direct_print_i64(7);
    super::aura_direct_print_f32(7.0);
    super::aura_direct_print_f64(7.0);
    super::aura_direct_print_bool(0);
    super::aura_direct_print_bool(1);
    let value = string_value("");
    let clone_count = super::direct_value_clone_count();
    super::aura_direct_print_value(value);
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "printing a direct runtime value must render the shared value without cloning it"
    );
    unsafe { release_value(value) };
    expect_result_ok_unit(super::aura_direct_io_write(string_value("")));
    expect_result_ok_unit(super::aura_direct_io_flush());
}

#[test]
fn direct_print_u64_renders_the_full_unsigned_range() {
    const HELPER_ENV: &str = "AURA_DIRECT_RUNTIME_PRINT_U64_HELPER";
    if std::env::var_os(HELPER_ENV).is_some() {
        super::aura_direct_print_u64(u64::MAX);
        return;
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg("native_runtime::tests::direct_print_u64_renders_the_full_unsigned_range")
        .arg("--nocapture")
        .env(HELPER_ENV, "1")
        .output()
        .expect("child test process should run");

    assert!(
        output.status.success(),
        "uint64 print helper should succeed"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("18446744073709551615\n"),
        "uint64 print helper should render u64::MAX as unsigned decimal"
    );
}

#[test]
fn direct_uint64_boxing_helpers_preserve_the_full_range() {
    for value in [0, (i64::MAX as u64) + 1, u64::MAX] {
        let boxed = super::aura_direct_box_u64(value);
        match unsafe { value_ref(boxed) } {
            Value::Int(actual) => {
                assert_eq!(
                    actual.representation(),
                    IntegerRepresentation::Unsigned(u128::from(value))
                );
                assert_eq!(actual.runtime_type_name(), Some("uint64"));
            }
            other => panic!("expected canonical unsigned integer, found {:?}", other),
        }
        assert_eq!(super::aura_direct_unbox_u64(boxed), value);
        unsafe {
            release_value(boxed);
        }
    }
}

#[test]
fn direct_runtime_type_tags_preserve_generic_identity_through_clone() {
    let value = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Option".to_string(),
        variant_name: "Some".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i32(7))],
    }));
    super::aura_direct_tag_value_type(value, b"Option[int32]".as_ptr(), "Option[int32]".len());

    for candidate in [value, super::aura_direct_clone_value(value)] {
        assert_eq!(
            super::aura_direct_value_type_matches(
                candidate,
                b"Option[int32]".as_ptr(),
                "Option[int32]".len(),
            ),
            1
        );
        assert_eq!(
            super::aura_direct_value_type_matches(
                candidate,
                b"Option[int64]".as_ptr(),
                "Option[int64]".len(),
            ),
            0
        );
        assert_eq!(
            super::aura_direct_value_type_matches(candidate, b"Option".as_ptr(), "Option".len(),),
            1
        );
        assert_eq!(
            super::aura_direct_value_type_matches(
                candidate,
                b"Option[?T]".as_ptr(),
                "Option[?T]".len(),
            ),
            1
        );
        assert_eq!(
            super::aura_direct_value_type_matches(
                candidate,
                b"Option[list[?T]]".as_ptr(),
                "Option[list[?T]]".len(),
            ),
            0
        );
        unsafe {
            release_value(candidate);
        }
    }

    let nested = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Option".to_string(),
        variant_name: "Some".to_string(),
        payloads: vec![Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements: vec![Value::Int(
                IntegerValue::from_typed_signed(9, IntegerKind::Int64).expect("9 fits int64"),
            )],
        })],
    }));
    super::aura_direct_tag_value_type(
        nested,
        b"Option[list[int64]]".as_ptr(),
        "Option[list[int64]]".len(),
    );
    assert_eq!(
        super::aura_direct_value_type_matches(nested, b"Option[?T]".as_ptr(), "Option[?T]".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            nested,
            b"Option[list[?T]]".as_ptr(),
            "Option[list[?T]]".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            nested,
            b"Option[list[int32]]".as_ptr(),
            "Option[list[int32]]".len(),
        ),
        0
    );
    unsafe {
        release_value(nested);
    }

    let mixed_map = boxed_value(Value::Map(MapValue {
        key_type: Type::named("Unknown"),
        value_type: Type::named("Unknown"),
        entries: Vec::new(),
    }));
    super::aura_direct_tag_value_type(
        mixed_map,
        b"dict[int32, int64]".as_ptr(),
        "dict[int32, int64]".len(),
    );
    match unsafe { value_ref(mixed_map) } {
        Value::Map(map) => {
            assert_eq!(map.key_type, Type::named("int32"));
            assert_eq!(map.value_type, Type::named("int64"));
        }
        other => panic!("expected tagged map, found {other:?}"),
    }
    assert_eq!(
        super::aura_direct_value_type_matches(
            mixed_map,
            b"dict[?K, ?V]".as_ptr(),
            "dict[?K, ?V]".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            mixed_map,
            b"dict[?T, ?T]".as_ptr(),
            "dict[?T, ?T]".len(),
        ),
        0
    );
    unsafe {
        release_value(mixed_map);
    }

    let vector = boxed_value(Value::Vec(VecValue {
        element_type: Type::named("Unknown"),
        elements: Vec::new(),
    }));
    super::aura_direct_tag_value_type(vector, b"list[int32]".as_ptr(), "list[int32]".len());
    assert_eq!(super::aura_direct_value_has_runtime_type(vector), 1);
    match unsafe { value_ref(vector) } {
        Value::Vec(vector) => assert_eq!(vector.element_type, Type::named("int32")),
        other => panic!("expected tagged vector, found {other:?}"),
    }
    assert_eq!(
        super::aura_direct_value_type_matches(vector, b"list[?T]".as_ptr(), "list[?T]".len(),),
        1
    );
    unsafe {
        release_value(vector);
    }

    let set = boxed_value(Value::Set(SetValue {
        element_type: Type::named("Unknown"),
        elements: Vec::new(),
    }));
    super::aura_direct_tag_value_type(set, b"set[int64]".as_ptr(), "set[int64]".len());
    match unsafe { value_ref(set) } {
        Value::Set(set) => assert_eq!(set.element_type, Type::named("int64")),
        other => panic!("expected tagged set, found {other:?}"),
    }
    assert_eq!(
        super::aura_direct_value_type_matches(set, b"set[?T]".as_ptr(), "set[?T]".len(),),
        1
    );
    unsafe {
        release_value(set);
    }

    let instance = boxed_value(Value::Instance(InstanceValue {
        class_name: "Marker".to_string(),
        fields: BTreeMap::new(),
    }));
    super::aura_direct_tag_value_type(instance, b"Marker[int64]".as_ptr(), "Marker[int64]".len());
    assert_eq!(
        super::aura_direct_value_type_matches(instance, b"Marker[?T]".as_ptr(), "Marker[?T]".len(),),
        1
    );
    let cloned_instance = super::aura_direct_clone_value(instance);
    assert_eq!(
        super::aura_direct_value_type_matches(
            cloned_instance,
            b"Marker[int64]".as_ptr(),
            "Marker[int64]".len(),
        ),
        1
    );
    unsafe {
        release_value(instance);
        release_value(cloned_instance);
    }

    let queue = boxed_value(Value::Channel(ChannelValue::new()));
    super::aura_direct_tag_value_type(queue, b"Queue[int32]".as_ptr(), "Queue[int32]".len());
    assert_eq!(
        super::aura_direct_value_type_matches(queue, b"Queue[?T]".as_ptr(), "Queue[?T]".len(),),
        1
    );
    unsafe {
        release_value(queue);
    }

    let task = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Unit)
    }))));
    super::aura_direct_tag_value_type(task, b"Task[int64]".as_ptr(), "Task[int64]".len());
    assert_eq!(
        super::aura_direct_value_type_matches(task, b"Task[?T]".as_ptr(), "Task[?T]".len(),),
        1
    );
    unsafe {
        release_value(task);
    }

    let nested_callback = Type::Function {
        params: Vec::new(),
        return_type: Box::new(Type::named("bool")),
    };
    let concrete_function_signature = Type::Function {
        params: vec![
            FunctionParamContract {
                name: "shared".to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Borrow,
                has_default: false,
                default_erased: false,
            },
            FunctionParamContract {
                name: "mutable".to_string(),
                ty: Type::Named("list".to_string(), vec![Type::named("int32")]),
                passing: ReceiverKind::BorrowMut,
                has_default: true,
                default_erased: false,
            },
            FunctionParamContract {
                name: "owned".to_string(),
                ty: nested_callback,
                passing: ReceiverKind::Value,
                has_default: false,
                default_erased: false,
            },
        ],
        return_type: Box::new(Type::named("int64")),
    };
    let function = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "selected".to_string(),
        signature: Type::Function {
            params: vec![
                FunctionParamContract {
                    name: "shared".to_string(),
                    ty: Type::TypeParam("A".to_string()),
                    passing: ReceiverKind::Borrow,
                    has_default: false,
                    default_erased: false,
                },
                FunctionParamContract {
                    name: "mutable".to_string(),
                    ty: Type::TypeParam("B".to_string()),
                    passing: ReceiverKind::BorrowMut,
                    has_default: true,
                    default_erased: false,
                },
                FunctionParamContract {
                    name: "owned".to_string(),
                    ty: Type::TypeParam("C".to_string()),
                    passing: ReceiverKind::Value,
                    has_default: false,
                    default_erased: false,
                },
            ],
            return_type: Box::new(Type::TypeParam("R".to_string())),
        },
        source_path: Some("/workspace/selected.au".to_string()),
        entry_span: Span::new(7, 3),
        direct_thunk: Some(11),
        direct_default_binder: Some(22),
        closure_environment: None,
    })));
    let encoded = super::canonical_runtime_type_name(&concrete_function_signature);
    super::aura_direct_tag_value_type(function, encoded.as_ptr(), encoded.len());
    match unsafe { value_ref(function) } {
        Value::Function(function_value) => {
            assert_eq!(function_value.signature, concrete_function_signature);
            assert_eq!(function_value.name, "selected");
            assert_eq!(
                function_value.source_path.as_deref(),
                Some("/workspace/selected.au")
            );
            assert_eq!(function_value.entry_span, Span::new(7, 3));
            assert_eq!(function_value.direct_thunk, Some(11));
            assert_eq!(function_value.direct_default_binder, Some(22));
        }
        other => panic!("expected tagged function value, found {other:?}"),
    }
    assert_eq!(
        super::aura_direct_value_type_matches(function, encoded.as_ptr(), encoded.len()),
        1
    );
    let mut wrong_mode = concrete_function_signature.clone();
    let Type::Function { params, .. } = &mut wrong_mode else {
        unreachable!()
    };
    params[1].passing = ReceiverKind::Borrow;
    let wrong_mode = super::canonical_runtime_type_name(&wrong_mode);
    assert_eq!(
        super::aura_direct_value_type_matches(function, wrong_mode.as_ptr(), wrong_mode.len(),),
        0,
        "canonical function tags preserve nested shared/mut/own modes"
    );
    unsafe {
        release_value(function);
    }

    let unit = boxed_value(Value::Unit);
    assert_eq!(super::aura_direct_value_has_runtime_type(unit), 0);
    unsafe {
        release_value(unit);
    }

    let untagged = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Option".to_string(),
        variant_name: "Some".to_string(),
        payloads: vec![Value::Int(IntegerValue::from_i32(11))],
    }));
    assert_eq!(
        super::aura_direct_value_type_matches(untagged, b"Option[?T]".as_ptr(), "Option[?T]".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            untagged,
            b"Option[list[?T]]".as_ptr(),
            "Option[list[?T]]".len(),
        ),
        0
    );
    unsafe {
        release_value(untagged);
    }
}

#[test]
fn direct_function_value_abi_preserves_signature_capabilities_defaults_and_metadata() {
    let signature = Type::Function {
        params: vec![
            FunctionParamContract {
                name: "shared".to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Borrow,
                has_default: false,
                default_erased: false,
            },
            FunctionParamContract {
                name: "mutable".to_string(),
                ty: Type::Named("list".to_string(), vec![Type::named("int32")]),
                passing: ReceiverKind::BorrowMut,
                has_default: true,
                default_erased: false,
            },
            FunctionParamContract {
                name: "owned".to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Value,
                has_default: false,
                default_erased: false,
            },
        ],
        return_type: Box::new(Type::named("int64")),
    };
    let encoded_signature =
        serde_json::to_vec(&signature).expect("function signature should serialize");
    let function = super::aura_direct_function_value(
        test_native_thunk as *const () as usize as i64,
        0x5eed,
        b"selected".as_ptr(),
        "selected".len(),
        encoded_signature.as_ptr(),
        encoded_signature.len(),
        b"/workspace/selected.au".as_ptr(),
        "/workspace/selected.au".len(),
        7,
        3,
    );

    assert_eq!(
        super::aura_direct_function_thunk(function),
        test_native_thunk as *const () as usize as i64
    );
    assert_eq!(super::aura_direct_function_default_binder(function), 0x5eed);
    match unsafe { value_ref(function) } {
        Value::Function(function_value) => {
            assert_eq!(function_value.name, "selected");
            assert_eq!(function_value.signature, signature);
            assert_eq!(
                function_value.source_path.as_deref(),
                Some("/workspace/selected.au")
            );
            assert_eq!(function_value.entry_span, Span::new(7, 3));
        }
        other => panic!("expected Function value, found {other:?}"),
    }
    let value = unsafe { value_ref(function) };
    assert_eq!(value_type_name(&value), signature.to_string());
    assert_eq!(inferred_collection_type(&value), signature);
    assert_eq!(super::aura_direct_value_has_runtime_type(function), 1);
    let canonical = super::canonical_runtime_type_name(&signature);
    assert_eq!(
        super::aura_direct_value_type_matches(function, canonical.as_ptr(), canonical.len()),
        1
    );
    let displayed = signature.to_string();
    assert_eq!(
        super::aura_direct_value_type_matches(function, displayed.as_ptr(), displayed.len(),),
        1,
        "the compatibility matcher must recognize the complete displayed callable signature"
    );

    unsafe {
        release_value(function);
    }
}

#[test]
fn direct_function_value_type_patterns_bind_nested_types_and_capabilities() {
    fn contract(name: &str, ty: Type, passing: ReceiverKind) -> FunctionParamContract {
        FunctionParamContract {
            name: name.to_string(),
            ty,
            passing,
            has_default: false,
            default_erased: false,
        }
    }

    fn function_value(signature: &Type, name: &'static [u8]) -> *mut OpaqueValue {
        let encoded = serde_json::to_vec(signature).expect("function signature should serialize");
        super::aura_direct_function_value(
            test_native_thunk as *const () as usize as i64,
            1,
            name.as_ptr(),
            name.len(),
            encoded.as_ptr(),
            encoded.len(),
            std::ptr::null(),
            0,
            1,
            1,
        )
    }

    let wildcard = Type::named("?T");
    let pattern = Type::Function {
        params: vec![
            contract("shared", wildcard.clone(), ReceiverKind::Borrow),
            contract("owned", wildcard.clone(), ReceiverKind::Value),
        ],
        return_type: Box::new(wildcard),
    };
    let encoded_pattern = super::canonical_runtime_type_name(&pattern);
    let decoded_pattern = runtime_type_pattern_from_name(&encoded_pattern);
    let Type::Function {
        params,
        return_type,
    } = &decoded_pattern
    else {
        panic!("canonical callable pattern should decode as a function")
    };
    assert!(params
        .iter()
        .all(|param| param.ty == Type::TypeParam("T".to_string())));
    assert_eq!(**return_type, Type::TypeParam("T".to_string()));
    let encoded_pattern = super::canonical_runtime_type_name(&decoded_pattern);

    let matching_signature = Type::Function {
        params: vec![
            contract("shared", Type::named("int64"), ReceiverKind::Borrow),
            contract("owned", Type::named("int64"), ReceiverKind::Value),
        ],
        return_type: Box::new(Type::named("int64")),
    };
    let matching = function_value(&matching_signature, b"matching");
    assert_eq!(
        super::aura_direct_value_type_matches(
            matching,
            encoded_pattern.as_ptr(),
            encoded_pattern.len(),
        ),
        1,
        "one callable wildcard must bind consistently across parameters and return type"
    );

    let mut wrong_capability = decoded_pattern.clone();
    let Type::Function { params, .. } = &mut wrong_capability else {
        unreachable!()
    };
    params[0].passing = ReceiverKind::BorrowMut;
    let wrong_capability = super::canonical_runtime_type_name(&wrong_capability);
    assert_eq!(
        super::aura_direct_value_type_matches(
            matching,
            wrong_capability.as_ptr(),
            wrong_capability.len(),
        ),
        0,
        "callable pattern matching must preserve shared, mutable, and owned modes"
    );

    let shorter_pattern = Type::Function {
        params: vec![contract(
            "shared",
            Type::TypeParam("T".to_string()),
            ReceiverKind::Borrow,
        )],
        return_type: Box::new(Type::TypeParam("T".to_string())),
    };
    let shorter_pattern = super::canonical_runtime_type_name(&shorter_pattern);
    assert_eq!(
        super::aura_direct_value_type_matches(
            matching,
            shorter_pattern.as_ptr(),
            shorter_pattern.len(),
        ),
        0,
        "callable patterns require the selected function's exact arity"
    );

    let inconsistent_signature = Type::Function {
        params: vec![
            contract("shared", Type::named("int64"), ReceiverKind::Borrow),
            contract("owned", Type::named("str"), ReceiverKind::Value),
        ],
        return_type: Box::new(Type::named("int64")),
    };
    let inconsistent = function_value(&inconsistent_signature, b"inconsistent");
    assert_eq!(
        super::aura_direct_value_type_matches(
            inconsistent,
            encoded_pattern.as_ptr(),
            encoded_pattern.len(),
        ),
        0,
        "repeated callable wildcards must reject inconsistent substitutions"
    );

    let ordinary = int_value(7);
    assert_eq!(
        super::aura_direct_value_type_matches(
            ordinary,
            encoded_pattern.as_ptr(),
            encoded_pattern.len(),
        ),
        0,
        "a callable pattern must reject non-callable runtime values"
    );

    unsafe {
        release_value(matching);
        release_value(inconsistent);
        release_value(ordinary);
    }
}

#[test]
fn direct_closure_type_matching_preserves_callable_and_capture_contracts() {
    let param = |ty, passing| FunctionParamContract {
        name: "value".to_string(),
        ty,
        passing,
        has_default: false,
        default_erased: false,
    };
    let capture = |ty, mode| ClosureCapture {
        name: "captured".to_string(),
        ty,
        mode,
        span: Span::new(3, 5),
    };
    let closure = |parameter_ty, passing, capture_ty, mode, call_kind| Type::Closure {
        params: Box::new(vec![param(parameter_ty, passing)]),
        return_type: Box::new(Type::named("int64")),
        captures: Box::new(vec![capture(capture_ty, mode)]),
        call_kind,
    };

    let actual = closure(
        Type::named("int64"),
        ReceiverKind::Borrow,
        Type::named("str"),
        ClosureCaptureMode::Copy,
        ClosureCallKind::Repeatable,
    );
    let value = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "predicate".to_string(),
        signature: actual.clone(),
        source_path: Some("/workspace/closure.au".to_string()),
        entry_span: Span::new(7, 11),
        direct_thunk: Some(1),
        direct_default_binder: Some(1),
        closure_environment: None,
    })));
    let wildcard = Type::TypeParam("T".to_string());
    let matching = closure(
        wildcard.clone(),
        ReceiverKind::Borrow,
        Type::TypeParam("Captured".to_string()),
        ClosureCaptureMode::Copy,
        ClosureCallKind::Repeatable,
    );
    let no_params = Type::Closure {
        params: Box::new(Vec::new()),
        return_type: Box::new(Type::named("int64")),
        captures: Box::new(vec![capture(
            Type::TypeParam("Captured".to_string()),
            ClosureCaptureMode::Copy,
        )]),
        call_kind: ClosureCallKind::Repeatable,
    };
    let no_captures = Type::Closure {
        params: Box::new(vec![param(
            Type::TypeParam("T".to_string()),
            ReceiverKind::Borrow,
        )]),
        return_type: Box::new(Type::named("int64")),
        captures: Box::new(Vec::new()),
        call_kind: ClosureCallKind::Repeatable,
    };
    let cases = [
        ("matching closure", matching, 1),
        ("parameter count", no_params, 0),
        ("capture count", no_captures, 0),
        (
            "call kind",
            closure(
                wildcard.clone(),
                ReceiverKind::Borrow,
                Type::TypeParam("Captured".to_string()),
                ClosureCaptureMode::Copy,
                ClosureCallKind::Consuming,
            ),
            0,
        ),
        (
            "parameter capability",
            closure(
                wildcard.clone(),
                ReceiverKind::BorrowMut,
                Type::TypeParam("Captured".to_string()),
                ClosureCaptureMode::Copy,
                ClosureCallKind::Repeatable,
            ),
            0,
        ),
        (
            "capture mode",
            closure(
                wildcard,
                ReceiverKind::Borrow,
                Type::TypeParam("Captured".to_string()),
                ClosureCaptureMode::Move,
                ClosureCallKind::Repeatable,
            ),
            0,
        ),
    ];
    for (label, pattern, expected) in cases {
        let encoded = super::canonical_runtime_type_name(&pattern);
        assert_eq!(
            super::aura_direct_value_type_matches(value, encoded.as_ptr(), encoded.len()),
            expected,
            "{label}"
        );
    }

    let function_pattern = Type::Function {
        params: vec![param(Type::named("int64"), ReceiverKind::Borrow)],
        return_type: Box::new(Type::named("int64")),
    };
    let encoded = super::canonical_runtime_type_name(&function_pattern);
    assert_eq!(
        super::aura_direct_value_type_matches(value, encoded.as_ptr(), encoded.len()),
        0,
        "a closure contract must not masquerade as a plain function contract"
    );
    unsafe { release_value(value) };
}

#[test]
fn direct_function_value_abi_rejects_invalid_signatures_and_missing_native_targets() {
    let signature = Type::Function {
        params: Vec::new(),
        return_type: Box::new(Type::Unit),
    };
    let encoded_signature =
        serde_json::to_vec(&signature).expect("function signature should serialize");
    let null_signature = encoded_signature.clone();
    let null_thunk = capture_direct_boundary_error_message(move || {
        super::aura_direct_function_value(
            0,
            1,
            b"missing".as_ptr(),
            "missing".len(),
            null_signature.as_ptr(),
            null_signature.len(),
            std::ptr::null(),
            0,
            1,
            1,
        );
    });
    assert_eq!(null_thunk, "direct runtime received a null function thunk");

    let invalid_signature = capture_direct_boundary_error_message(|| {
        super::aura_direct_function_value(
            1,
            2,
            b"invalid".as_ptr(),
            "invalid".len(),
            b"{".as_ptr(),
            1,
            std::ptr::null(),
            0,
            1,
            1,
        );
    });
    assert!(
        invalid_signature
            .starts_with("direct runtime received invalid function signature metadata:"),
        "{invalid_signature}"
    );

    let ordinary_value = int_value(7);
    let ordinary_address = ordinary_value as usize;
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_function_thunk(ordinary_address as *mut OpaqueValue);
        }),
        "indirect call expected a function value, found `integer`"
    );
    let ordinary_address = ordinary_value as usize;
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_function_default_binder(ordinary_address as *mut OpaqueValue);
        }),
        "indirect call expected a function value, found `integer`"
    );
    unsafe {
        release_value(ordinary_value);
    }

    let missing_targets = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "declaration-only".to_string(),
        signature,
        source_path: None,
        entry_span: Span::new(1, 1),
        direct_thunk: None,
        direct_default_binder: None,
        closure_environment: None,
    })));
    let missing_targets_address = missing_targets as usize;
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_function_thunk(missing_targets_address as *mut OpaqueValue);
        }),
        "direct function value has no native thunk"
    );
    let missing_targets_address = missing_targets as usize;
    assert_eq!(
        capture_direct_boundary_error_message(move || {
            super::aura_direct_function_default_binder(missing_targets_address as *mut OpaqueValue);
        }),
        "direct function value has no native default binder"
    );
    unsafe {
        release_value(missing_targets);
    }
}

#[test]
fn direct_int64_unbox_helper_preserves_the_full_signed_range() {
    for value in [i64::MIN, -1, 0, i64::MAX] {
        let boxed = super::aura_direct_box_i64(value);
        assert_eq!(super::aura_direct_unbox_int64(boxed), value);
        unsafe {
            release_value(boxed);
        }
    }
}

#[test]
fn direct_integer_to_float_helper_rounds_without_consuming_the_integer() {
    let boxed = boxed_value(Value::Int(IntegerValue::from_literal(
        9_007_199_254_740_993,
    )));
    assert_eq!(
        super::aura_direct_integer_to_float(boxed),
        9_007_199_254_740_992.0
    );
    assert_eq!(
        super::aura_direct_integer_to_float(boxed),
        9_007_199_254_740_992.0
    );
    unsafe {
        release_value(boxed);
    }
}

#[test]
fn direct_unboxed_wide_cast_helpers_preserve_checked_numeric_semantics() {
    assert_eq!(
        super::aura_direct_cast_integer_to_integer((-42_i64) as u64, 0, 0, 0, 0),
        (-42_i64) as u64
    );
    assert_eq!(
        super::aura_direct_cast_integer_to_integer(42, 0, 1, 0, 0),
        42
    );
    assert_eq!(
        super::aura_direct_cast_integer_to_integer(u64::MAX, 1, 2, 0, 0),
        u64::MAX
    );
    assert_eq!(
        super::aura_direct_cast_integer_to_float(1_u64 << 53, 0, 1, 0, 0),
        (1_u64 << 53) as f64
    );
    assert_eq!(
        super::aura_direct_cast_integer_to_float(1_u64 << 63, 1, 1, 0, 0),
        (1_u64 << 63) as f64
    );
    assert_eq!(
        super::aura_direct_cast_integer_to_float(42, 0, 0, 0, 0),
        42.0_f32 as f64
    );
    assert_eq!(
        super::aura_direct_cast_float_to_integer(4_294_967_296.75, 1, 0, 0),
        4_294_967_296
    );
    assert_eq!(
        super::aura_direct_cast_float_to_integer(-42.75, 1, 0, 0),
        (-42_i64) as u64
    );
}

#[test]
fn wide_integer_overflow_messages_match_mir_diagnostics_exactly() {
    for (kind, op, left, right, expected) in [
        (
            0,
            0,
            i64::MAX as u64,
            1,
            "integer value `9223372036854775808` does not fit in `int64`",
        ),
        (
            0,
            1,
            i64::MIN as u64,
            1,
            "integer value `-9223372036854775809` does not fit in `int64`",
        ),
        (
            0,
            2,
            i64::MAX as u64,
            2,
            "integer value `18446744073709551614` does not fit in `int64`",
        ),
        (
            0,
            3,
            i64::MIN as u64,
            (-1_i64) as u64,
            "integer value `9223372036854775808` does not fit in `int64`",
        ),
        (
            1,
            0,
            u64::MAX,
            1,
            "integer value `18446744073709551616` does not fit in `uint64`",
        ),
        (1, 1, 0, 1, "integer value `-1` does not fit in `uint64`"),
        (
            1,
            2,
            u64::MAX,
            2,
            "integer value `36893488147419103230` does not fit in `uint64`",
        ),
    ] {
        assert_eq!(
            super::wide_integer_overflow_message(kind, op, left, right),
            expected
        );
    }
}

#[test]
fn direct_stdout_result_helpers_accept_empty_writes_and_flushes() {
    super::write_stdout_result("").expect("empty direct stdout writes should succeed");
    super::flush_stdout_result().expect("direct stdout flushes should succeed");
}

#[test]
fn native_runtime_process_capture_task_helper_covers_success_and_malformed_results() {
    assert_eq!(
        super::await_process_capture_task(None, "stdout"),
        Vec::<u8>::new()
    );

    let bytes_task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: crate::sema::Type::named("uint8"),
            elements: vec![
                Value::Int(IntegerValue::from_signed(65)),
                Value::Int(IntegerValue::from_literal(66)),
            ],
        }))
    }));
    assert_eq!(
        super::await_process_capture_task(Some(bytes_task), "stdout"),
        b"AB".to_vec()
    );

    let non_byte_integer = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: crate::sema::Type::named("uint8"),
            elements: vec![Value::Int(IntegerValue::from_signed(300))],
        }))
    }));
    let message = capture_runtime_error_message(|| {
        super::await_process_capture_task(Some(non_byte_integer), "stdout");
    });
    assert!(message.contains("process stdout capture returned a non-byte integer"));

    let wrong_payload = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: crate::sema::Type::named("uint8"),
            elements: vec![Value::String("bad".to_string())],
        }))
    }));
    let message = capture_runtime_error_message(|| {
        super::await_process_capture_task(Some(wrong_payload), "stderr");
    });
    assert!(message.contains("process stderr capture returned `bad` inside `list[uint8]"));

    let wrong_result_type = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: crate::sema::Type::named("str"),
            elements: vec![Value::String("bad".to_string())],
        }))
    }));
    let message = capture_runtime_error_message(|| {
        super::await_process_capture_task(Some(wrong_result_type), "stderr");
    });
    assert!(message.contains("process stderr capture returned `[bad]` instead of `list[uint8]"));

    let capture_error =
        TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("pipe failed"))));
    let message = capture_runtime_error_message(|| {
        super::await_process_capture_task(Some(capture_error), "stdout");
    });
    assert!(message.contains("pipe failed"));

    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();
    let message = with_cancellation_scope(cancellation, || {
        let cancelled_task = TaskValue::from_handle(thread::spawn(|| {
            thread::sleep(StdDuration::from_millis(50));
            Ok(Value::Vec(VecValue {
                element_type: crate::sema::Type::named("uint8"),
                elements: Vec::new(),
            }))
        }));
        capture_runtime_error_message(|| {
            super::await_process_capture_task(Some(cancelled_task), "stdout");
        })
    });
    assert!(message.contains("process stdout capture was cancelled unexpectedly"));
}

#[test]
fn native_runtime_process_error_and_wait_all_helpers_cover_remaining_paths() {
    assert!(expect_variant_value(
        super::process_error_from_io(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
        "Error",
        "TimedOut",
    )
    .is_empty());
    assert!(expect_variant_value(
        super::process_error_from_io(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
        "Error",
        "Cancelled",
    )
    .is_empty());
    assert_eq!(
        expect_variant_value(
            super::process_error_from_io(io::Error::new(io::ErrorKind::Other, "io failure")),
            "Error",
            "Io",
        )
        .len(),
        1
    );

    let wait_all_payloads = expect_variant_ptr(
        super::aura_direct_wait_all(super::aura_direct_vec_empty()),
        "WaitAll",
        "Ready",
    );
    match wait_all_payloads.as_slice() {
        [Value::Vec(values)] => assert!(values.elements.is_empty()),
        other => panic!(
            "expected WaitAll.Ready empty vec payload, found {:?}",
            other
        ),
    }

    assert!(expect_variant_ptr(
        super::aura_direct_wait_any(super::aura_direct_vec_empty()),
        "WaitAny",
        "TimedOut",
    )
    .is_empty());
    assert!(expect_variant_ptr(
        super::aura_direct_wait_any_timeout_value(
            super::aura_direct_vec_empty(),
            duration_value(0),
        ),
        "WaitAny",
        "TimedOut",
    )
    .is_empty());

    let timed_wait_all_payloads = expect_variant_ptr(
        super::aura_direct_wait_all_timeout_value(
            super::aura_direct_vec_empty(),
            duration_value(0),
        ),
        "WaitAll",
        "Ready",
    );
    match timed_wait_all_payloads.as_slice() {
        [Value::Vec(values)] => assert!(values.elements.is_empty()),
        other => panic!(
            "expected WaitAll.Ready empty vec payload, found {:?}",
            other
        ),
    }

    let ready_task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(70)))
    }));
    let ready_payloads = expect_variant_ptr(
        super::aura_direct_wait_any(task_vec(&[ready_task.clone()])),
        "WaitAny",
        "Ready",
    );
    match ready_payloads.as_slice() {
        [Value::Int(index), Value::Int(value)] => {
            assert_eq!(index.as_i128(), Some(0));
            assert_eq!(value.as_i128(), Some(70));
        }
        other => panic!("expected WaitAny.Ready(0, 70), found {:?}", other),
    }

    let error_task =
        TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("wait_any failed"))));
    let error_payloads = expect_variant_ptr(
        super::aura_direct_wait_any(task_vec(&[error_task.clone()])),
        "WaitAny",
        "Error",
    );
    match error_payloads.as_slice() {
        [Value::Int(index), Value::String(message)] => {
            assert_eq!(index.as_i128(), Some(0));
            assert_eq!(message, "wait_any failed");
        }
        other => panic!("expected WaitAny.Error(0, message), found {:?}", other),
    }

    let first = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(1)))
    }));
    let second = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Int(IntegerValue::from_signed(2)))
    }));
    let all_ready_payloads = expect_variant_ptr(
        super::aura_direct_wait_all(task_vec(&[first.clone(), second.clone()])),
        "WaitAll",
        "Ready",
    );
    match all_ready_payloads.as_slice() {
        [Value::Vec(values)] => {
            let ints = values
                .elements
                .iter()
                .map(|value| match value {
                    Value::Int(value) => value.as_i128().expect("expected signed integer"),
                    other => panic!("expected int wait_all value, found {:?}", other),
                })
                .collect::<Vec<_>>();
            assert_eq!(ints, vec![1, 2]);
        }
        other => panic!("expected WaitAll.Ready([1, 2]), found {:?}", other),
    }

    let wait_all_error_task =
        TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("wait_all failed"))));
    let all_error_payloads = expect_variant_ptr(
        super::aura_direct_wait_all(task_vec(&[first.clone(), wait_all_error_task.clone()])),
        "WaitAll",
        "Error",
    );
    match all_error_payloads.as_slice() {
        [Value::Int(index), Value::String(message)] => {
            assert_eq!(index.as_i128(), Some(1));
            assert_eq!(message, "wait_all failed");
        }
        other => panic!("expected WaitAll.Error(1, message), found {:?}", other),
    }

    let slow_task = TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(50));
        Ok(Value::Int(IntegerValue::from_signed(9)))
    }));
    assert!(expect_variant_ptr(
        super::aura_direct_wait_any_timeout_value(
            task_vec(&[slow_task.clone()]),
            duration_value(0)
        ),
        "WaitAny",
        "TimedOut",
    )
    .is_empty());
    assert!(expect_variant_ptr(
        super::aura_direct_wait_all_timeout_value(
            task_vec(&[slow_task.clone()]),
            duration_value(0)
        ),
        "WaitAll",
        "TimedOut",
    )
    .is_empty());
    assert_eq!(
        expect_task_result_ready_int(super::aura_direct_task_join(boxed_value(Value::Task(
            slow_task
        )))),
        9
    );

    let no_start_command = expect_result_err_payload(super::aura_direct_process_start(
        super::aura_direct_vec_empty(),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        bool_value(false),
    ));
    assert!(expect_variant_value(no_start_command, "Error", "NoCommand").is_empty());

    let no_run_command = expect_result_err_payload(super::aura_direct_process_run(
        super::aura_direct_vec_empty(),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        boxed_value(Value::Unit),
        bool_value(false),
    ));
    assert!(expect_variant_value(no_run_command, "Error", "NoCommand").is_empty());
}

#[test]
fn direct_select_abi_consumes_its_owned_tuple_and_returns_canonical_deadline_outcome() {
    let _guard = super::direct_task_claim_flag_test_guard();
    let clone_count = super::direct_value_clone_count();
    let payloads = expect_variant_ptr(
        super::aura_direct_select(select_sources(
            vec![Type::named("Duration")],
            vec![Value::Duration(0)],
        )),
        "SelectOutcome",
        "Deadline",
    );
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count + 1,
        "only reading the returned outcome may clone an opaque value; the direct select boundary \
         must move its owned input tuple"
    );
    match payloads.as_slice() {
        [Value::Int(index)] => assert_eq!(index.as_i128(), Some(0)),
        other => panic!("expected SelectOutcome.Deadline(0), found {other:?}"),
    }
}

#[test]
fn native_runtime_single_consumer_task_observation_is_defended_across_aliases() {
    fn task_ptr(task: &TaskValue) -> *mut OpaqueValue {
        boxed_value(Value::Task(task.clone()))
    }

    #[derive(Clone, Copy)]
    enum Observer {
        Result,
        ResultWithTimeout,
        ResultOrNone,
        ResultOrNoneWithTimeout,
        ResultOr,
        ResultOrWithTimeout,
    }

    fn repeated_observation_error(task: TaskValue, observer: Observer) -> Diagnostic {
        struct OwnedOpaque(*mut OpaqueValue);

        impl Drop for OwnedOpaque {
            fn drop(&mut self) {
                unsafe {
                    release_value(self.0);
                }
            }
        }

        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                let task = OwnedOpaque(task_ptr(&task));
                let _ = match observer {
                    Observer::Result => super::aura_direct_task_join(task.0),
                    Observer::ResultWithTimeout => {
                        super::aura_direct_task_join_timeout_value(task.0, duration_value(0))
                    }
                    Observer::ResultOrNone => super::aura_direct_task_join_or_none(task.0),
                    Observer::ResultOrNoneWithTimeout => {
                        super::aura_direct_task_join_or_none_timeout_value(
                            task.0,
                            duration_value(0),
                        )
                    }
                    Observer::ResultOr => {
                        super::aura_direct_task_join_or_value(task.0, string_value("fallback"))
                    }
                    Observer::ResultOrWithTimeout => {
                        super::aura_direct_task_join_or_value_timeout_value(
                            task.0,
                            string_value("fallback"),
                            duration_value(0),
                        )
                    }
                };
                Ok(Value::Unit)
            })
        })
        .expect_err("a repeated direct task observation should fail its Aura task")
    }

    fn repeated_join_error(task: TaskValue) -> Diagnostic {
        repeated_observation_error(task, Observer::Result)
    }

    let repeatable = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::Int(IntegerValue::from_signed(7)))),
        true,
    );
    for _ in 0..2 {
        assert_eq!(
            expect_task_result_ready_int(super::aura_direct_task_join(task_ptr(&repeatable))),
            7
        );
    }

    let timeout_blocker = ChannelValue::new();
    let timeout_unblocker = timeout_blocker.clone();
    let nonrepeatable = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(move || {
            let _ = timeout_unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    assert!(expect_variant_ptr(
        super::aura_direct_task_join_timeout_value(task_ptr(&nonrepeatable), duration_value(0),),
        "TaskResult",
        "TimedOut",
    )
    .is_empty());
    let repeated = repeated_join_error(nonrepeatable.clone());
    assert_eq!(repeated.code, "AU4001");
    assert!(repeated.message.contains("already been observed"));
    timeout_blocker.close();

    let default_blocker = ChannelValue::new();
    let default_unblocker = default_blocker.clone();
    let default_task = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(move || {
            let _ = default_unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    assert_eq!(
        expect_string(super::aura_direct_task_join_or_value(
            task_ptr(&default_task),
            string_value("fallback"),
        )),
        "fallback"
    );
    assert_eq!(repeated_join_error(default_task).code, "AU4001");
    default_blocker.close();

    let cancelled_task = Arc::new(Mutex::new(None));
    let saved_cancelled_task = cancelled_task.clone();
    run_lightweight_root_task(move || {
        let task = crate::runtime_value::spawn_lightweight_task_with_result_repeatability(
            false,
            || -> crate::diag::Result<Value> {
                crate::runtime_value::cancel_current_lightweight_task_boundary()
            },
        )?;
        *saved_cancelled_task
            .lock()
            .expect("cancelled task slot should remain usable") = Some(task.clone());
        assert!(expect_variant_ptr(
            super::aura_direct_task_join(task_ptr(&task)),
            "TaskResult",
            "Cancelled",
        )
        .is_empty());
        Ok(Value::Unit)
    })
    .expect("the first cancelled direct observation should return a TaskResult");
    let cancelled_task = cancelled_task
        .lock()
        .expect("cancelled task slot should remain usable")
        .take()
        .expect("cancelled task should be retained for the competing observer");
    assert_eq!(repeated_join_error(cancelled_task).code, "AU4001");

    let duplicate = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("ready".to_string()))),
        false,
    );
    let duplicate_error = super::wait_any_tasks(
        vec![duplicate.clone(), duplicate.clone()],
        Some(StdDuration::from_secs(1)),
    )
    .expect_err("wait_any must reject duplicate non-repeatable aliases");
    assert_eq!(duplicate_error.code, "AU4001");
    let repeated = repeated_join_error(duplicate);
    assert_eq!(repeated.code, "AU4001");

    let duplicate_all = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("once".to_string()))),
        false,
    );
    let duplicate_all_error = super::wait_all_tasks(
        vec![duplicate_all.clone(), duplicate_all],
        Some(StdDuration::from_secs(1)),
    )
    .expect_err("wait_all must not deliver one non-repeatable result twice");
    assert_eq!(duplicate_all_error.code, "AU4001");

    let selected = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("selected".to_string()))),
        false,
    );
    let selected_payloads = expect_variant_ptr(
        super::aura_direct_wait_any(task_vec(std::slice::from_ref(&selected))),
        "WaitAny",
        "Ready",
    );
    assert_eq!(selected_payloads.len(), 2);
    assert_eq!(repeated_join_error(selected).code, "AU4001");

    let first = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Ok(Value::String("first".to_string()))),
        false,
    );
    let second = TaskValue::from_handle_with_result_repeatability(
        thread::spawn(|| Err(Diagnostic::new("second failed"))),
        false,
    );
    let error = expect_variant_ptr(
        super::aura_direct_wait_all(task_vec(&[first.clone(), second.clone()])),
        "WaitAll",
        "Error",
    );
    assert_eq!(error.len(), 2);
    for claimed in [first, second] {
        let diagnostic = repeated_join_error(claimed);
        assert_eq!(diagnostic.code, "AU4001");
    }

    for observer in [
        Observer::ResultWithTimeout,
        Observer::ResultOrNone,
        Observer::ResultOrNoneWithTimeout,
        Observer::ResultOr,
        Observer::ResultOrWithTimeout,
    ] {
        let blocker = ChannelValue::new();
        let unblocker = blocker.clone();
        let task = TaskValue::from_handle_with_result_repeatability(
            thread::spawn(move || {
                let _ = unblocker.recv_with_cancellation(None, None);
                Ok(Value::String("late".to_string()))
            }),
            false,
        );
        task.claim_result_observation()
            .expect("the first alias should consume the single observation right");
        let error = repeated_observation_error(task, observer);
        assert_eq!(error.code, "AU4001");
        assert_eq!(
            error.message,
            "task result has already been observed; non-repeatable task results allow exactly one observing attempt"
        );
        blocker.close();
    }

    run_lightweight_root_task(|| {
        let cancelled = spawn_lightweight_task(|| -> crate::diag::Result<Value> {
            crate::runtime_value::cancel_current_lightweight_task_boundary()
        })?;
        assert!(matches!(
            cancelled
                .wait_result_with_cancellation(None, None)
                .expect("cancelled task completion should remain observable internally"),
            TaskWaitStatus::Cancelled
        ));

        assert!(expect_variant_ptr(
            super::aura_direct_task_join_or_none(task_ptr(&cancelled)),
            "Option",
            "None",
        )
        .is_empty());
        assert_eq!(
            expect_string(super::aura_direct_task_join_or_value(
                task_ptr(&cancelled),
                string_value("fallback"),
            )),
            "fallback"
        );
        assert!(expect_variant_value(
            super::wait_any_tasks(vec![cancelled], Some(StdDuration::from_secs(1)))?,
            "WaitAny",
            "Cancelled",
        )
        .is_empty());
        Ok(Value::Unit)
    })
    .expect("all direct observers should report an already-cancelled child consistently");
}

#[test]
fn native_runtime_direct_process_wrappers_cover_child_pipe_and_completed_paths() {
    assert!(
        expect_variant_ptr(super::aura_direct_process_inherit(), "Stdio", "Inherit",).is_empty()
    );
    assert!(
        expect_variant_ptr(super::aura_direct_process_pipe(), "Stdio", "Pipe").is_empty(),
        "the direct process wrapper must expose captured stdio as process.Stdio.Pipe"
    );

    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        b"stdout".to_vec(),
        b"stderr".to_vec(),
    );
    let completed_ptr = boxed_value(Value::ProcessCompleted(completed));
    assert_eq!(
        super::aura_direct_process_completed_success(completed_ptr),
        1
    );
    assert_eq!(
        expect_string(super::aura_direct_process_completed_stdout(completed_ptr)),
        "stdout"
    );
    assert_eq!(
        expect_string(super::aura_direct_process_completed_stderr(completed_ptr)),
        "stderr"
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_process_completed_stdout_bytes(
            completed_ptr
        )),
        b"stdout"
            .iter()
            .map(|byte| i128::from(*byte))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_process_completed_stderr_bytes(
            completed_ptr
        )),
        b"stderr"
            .iter()
            .map(|byte| i128::from(*byte))
            .collect::<Vec<_>>()
    );
    expect_result_ok_unit(super::aura_direct_process_completed_check(completed_ptr));
    let status_payload = expect_variant_ptr(
        super::aura_direct_process_completed_status(completed_ptr),
        "process.ExitStatus",
        "Exited",
    );
    assert_eq!(status_payload.len(), 1);
    unsafe { release_value(completed_ptr) };

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
    let child_ptr = boxed_value(Value::ProcessChild(child));
    let stdout_payload = expect_variant_ptr(
        super::aura_direct_process_child_stdout(child_ptr),
        "Option",
        "Some",
    );
    let stdout_pipe = match stdout_payload.as_slice() {
        [Value::ProcessPipe(pipe)] => pipe.clone(),
        other => panic!("expected process stdout pipe, found {:?}", other),
    };
    expect_option_none(super::aura_direct_process_child_stderr(child_ptr));

    let stdout_text = expect_result_ok_string(super::aura_direct_process_pipe_read_all(
        boxed_value(Value::ProcessPipe(stdout_pipe.clone())),
    ));
    assert!(
        stdout_text.contains("Usage") || stdout_text.contains("USAGE"),
        "unexpected child help stdout: {stdout_text}"
    );

    let wait_payload = expect_variant_ptr(
        super::aura_direct_process_child_wait_ok(child_ptr, std::ptr::null_mut()),
        "Result",
        "Ok",
    );
    assert!(matches!(
        wait_payload.as_slice(),
        [Value::EnumVariant(status)] if status.enum_name == "ExitStatus"
    ));
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(stdout_pipe),
    )));
    expect_unit(super::aura_direct_process_child_close(child_ptr));
    unsafe { release_value(child_ptr) };
}

#[test]
fn direct_completed_text_accessors_report_non_utf8_bytes_as_au4005() {
    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        vec![0xff],
        vec![0xfe],
    );
    let completed_ptr = boxed_value(Value::ProcessCompleted(completed));

    let stdout_ptr = completed_ptr as usize;
    let stdout_error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_process_completed_stdout(stdout_ptr as *mut OpaqueValue);
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid stdout text should fail the active task");
    assert_eq!(stdout_error.code, "AU4005");
    assert!(
        stdout_error.message.contains("received non-UTF-8 data"),
        "stdout text decoding should explain why byte access is required: {}",
        stdout_error.message
    );

    let stderr_ptr = completed_ptr as usize;
    let stderr_error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_process_completed_stderr(stderr_ptr as *mut OpaqueValue);
            Ok(Value::Unit)
        })
    })
    .expect_err("invalid stderr text should fail the active task");
    assert_eq!(stderr_error.code, "AU4005");
    assert!(
        stderr_error.message.contains("received non-UTF-8 data"),
        "stderr text decoding should explain why byte access is required: {}",
        stderr_error.message
    );

    unsafe { release_value(completed_ptr) };
}

#[test]
fn native_runtime_direct_process_wrappers_cover_streaming_and_signal_paths() {
    fn process_pipe_from_option(
        ptr: *mut OpaqueValue,
        label: &str,
    ) -> crate::runtime_value::ProcessPipeValue {
        let payloads = expect_variant_ptr(ptr, "Option", "Some");
        match payloads.as_slice() {
            [Value::ProcessPipe(pipe)] => pipe.clone(),
            other => panic!("expected {label} process pipe, found {:?}", other),
        }
    }

    fn string_from_option(value: Value, label: &str) -> String {
        match expect_option_some_payload(value) {
            Value::String(text) => text,
            other => panic!("expected {label} string payload, found {:?}", other),
        }
    }

    fn byte_values_from_option(value: Value, label: &str) -> Vec<i128> {
        match expect_option_some_payload(value) {
            Value::Vec(values) => values
                .elements
                .into_iter()
                .map(|value| match value {
                    Value::Int(byte) => byte.as_i128().expect("byte should be signed"),
                    other => panic!("expected {label} byte payload, found {:?}", other),
                })
                .collect(),
            other => panic!("expected {label} byte vector, found {:?}", other),
        }
    }

    let io_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'alpha\\nbeta'; printf 'err\\n' >&2".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        false,
    )
    .expect("process with stdout and stderr should spawn");
    let io_child_ptr = boxed_value(Value::ProcessChild(io_child));
    let stdout_pipe = process_pipe_from_option(
        super::aura_direct_process_child_stdout(io_child_ptr),
        "stdout",
    );
    let stderr_pipe = process_pipe_from_option(
        super::aura_direct_process_child_stderr(io_child_ptr),
        "stderr",
    );

    let first_line = string_from_option(
        expect_result_ok_payload(super::aura_direct_process_pipe_read_line(
            boxed_value(Value::ProcessPipe(stdout_pipe.clone())),
            duration_value(5_000),
        )),
        "stdout line",
    );
    assert!(first_line.starts_with("alpha"));
    let byte_chunk = byte_values_from_option(
        expect_result_ok_payload(super::aura_direct_process_pipe_read_bytes(
            boxed_value(Value::ProcessPipe(stdout_pipe.clone())),
            int_value(4),
            duration_value(5_000),
        )),
        "stdout byte chunk",
    );
    assert!(!byte_chunk.is_empty());
    assert_eq!(
        expect_result_ok_string(super::aura_direct_process_pipe_read_all(boxed_value(
            Value::ProcessPipe(stderr_pipe.clone())
        ))),
        "err\n"
    );

    let maybe_status = expect_result_ok_payload(super::aura_direct_process_child_wait_or_none(
        io_child_ptr,
        duration_value(5_000),
    ));
    match expect_option_some_payload(maybe_status) {
        Value::EnumVariant(status) if status.enum_name == "ExitStatus" => {}
        other => panic!("expected process exit status, found {:?}", other),
    }
    let waited_again = expect_variant_ptr(
        super::aura_direct_process_child_wait(io_child_ptr, std::ptr::null_mut()),
        "Wait",
        "Exited",
    );
    assert_eq!(waited_again.len(), 1);
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(stdout_pipe),
    )));
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(stderr_pipe),
    )));
    expect_unit(super::aura_direct_process_child_close(io_child_ptr));
    unsafe { release_value(io_child_ptr) };

    let cat_child = ProcessChildValue::spawn(
        vec!["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("cat process should spawn");
    let cat_child_ptr = boxed_value(Value::ProcessChild(cat_child));
    let cat_stdin = process_pipe_from_option(
        super::aura_direct_process_child_stdin(cat_child_ptr),
        "stdin",
    );
    let cat_stdout = process_pipe_from_option(
        super::aura_direct_process_child_stdout(cat_child_ptr),
        "stdout",
    );
    expect_result_ok_unit(super::aura_direct_process_pipe_write_all(
        boxed_value(Value::ProcessPipe(cat_stdin.clone())),
        string_value("left"),
        duration_value(5_000),
    ));
    expect_result_ok_unit(super::aura_direct_process_pipe_write_bytes(
        boxed_value(Value::ProcessPipe(cat_stdin.clone())),
        int_vec(&[114, 105, 103, 104, 116, 10]),
        duration_value(5_000),
    ));
    expect_result_ok_unit(super::aura_direct_process_pipe_flush(boxed_value(
        Value::ProcessPipe(cat_stdin.clone()),
    )));
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(cat_stdin),
    )));
    assert_eq!(
        expect_result_ok_string(super::aura_direct_process_pipe_read_all(boxed_value(
            Value::ProcessPipe(cat_stdout.clone())
        ))),
        "leftright\n"
    );
    let cat_wait = expect_variant_ptr(
        super::aura_direct_process_child_wait(cat_child_ptr, duration_value(5_000)),
        "Wait",
        "Exited",
    );
    assert_eq!(cat_wait.len(), 1);
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(cat_stdout),
    )));
    expect_unit(super::aura_direct_process_child_close(cat_child_ptr));
    unsafe { release_value(cat_child_ptr) };

    let signal_wrappers: [extern "C-unwind" fn(*mut OpaqueValue) -> *mut OpaqueValue; 2] = [
        super::aura_direct_process_child_terminate,
        super::aura_direct_process_child_kill,
    ];
    for signal_wrapper in signal_wrappers {
        let child = ProcessChildValue::spawn(
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
        .expect("sleep process should spawn");
        let child_ptr = boxed_value(Value::ProcessChild(child));
        expect_result_ok_unit(signal_wrapper(child_ptr));
        let wait_payloads = expect_variant_ptr(
            super::aura_direct_process_child_wait(child_ptr, duration_value(5_000)),
            "Wait",
            "Exited",
        );
        assert_eq!(wait_payloads.len(), 1);
        expect_unit(super::aura_direct_process_child_close(child_ptr));
        unsafe { release_value(child_ptr) };
    }
}

#[test]
fn native_runtime_direct_process_wrappers_cover_timeout_and_error_results() {
    fn process_pipe_from_option(
        ptr: *mut OpaqueValue,
        label: &str,
    ) -> crate::runtime_value::ProcessPipeValue {
        let payloads = expect_variant_ptr(ptr, "Option", "Some");
        match payloads.as_slice() {
            [Value::ProcessPipe(pipe)] => pipe.clone(),
            other => panic!("expected {label} process pipe, found {:?}", other),
        }
    }

    fn assert_process_io_error(value: Value) {
        assert_eq!(expect_variant_value(value, "Error", "Io").len(), 1);
    }

    let failed_completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(7))],
        }),
        Vec::new(),
        Vec::new(),
    );
    let failed_completed_ptr = boxed_value(Value::ProcessCompleted(failed_completed));
    assert_eq!(
        super::aura_direct_process_completed_success(failed_completed_ptr),
        0
    );
    assert_eq!(
        expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_completed_check(
                failed_completed_ptr
            )),
            "Error",
            "Other",
        )
        .len(),
        1
    );
    unsafe { release_value(failed_completed_ptr) };

    assert_eq!(
        expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_start(
                string_vec(&["__definitely_missing_aura_process_start__"]),
                boxed_value(Value::Unit),
                super::aura_direct_map_empty(),
                super::aura_direct_process_null(),
                super::aura_direct_process_null(),
                super::aura_direct_process_null(),
                bool_value(false),
            )),
            "Error",
            "Spawn",
        )
        .len(),
        1
    );
    assert_eq!(
        expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_run(
                string_vec(&["__definitely_missing_aura_process_run__"]),
                boxed_value(Value::Unit),
                super::aura_direct_map_empty(),
                super::aura_direct_process_null(),
                super::aura_direct_process_null(),
                super::aura_direct_process_null(),
                boxed_value(Value::Unit),
                bool_value(false),
            )),
            "Error",
            "Spawn",
        )
        .len(),
        1
    );

    let slow_child = ProcessChildValue::spawn(
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
    .expect("slow child should spawn");
    let slow_child_ptr = boxed_value(Value::ProcessChild(slow_child));
    assert!(expect_variant_ptr(
        super::aura_direct_process_child_wait(slow_child_ptr, duration_value(0)),
        "Wait",
        "TimedOut",
    )
    .is_empty());
    let wait_or_none = expect_result_ok_payload(super::aura_direct_process_child_wait_or_none(
        slow_child_ptr,
        duration_value(0),
    ));
    assert!(matches!(
        wait_or_none,
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None"
    ));
    assert!(expect_variant_value(
        expect_result_err_payload(super::aura_direct_process_child_wait_ok(
            slow_child_ptr,
            duration_value(0)
        )),
        "Error",
        "TimedOut",
    )
    .is_empty());
    expect_unit(super::aura_direct_process_child_close(slow_child_ptr));
    unsafe { release_value(slow_child_ptr) };

    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();
    with_cancellation_scope(cancellation.clone(), || {
        let cancelled_run = expect_result_err_payload(super::aura_direct_process_run(
            string_vec(&["/bin/sh", "-c", "sleep 1"]),
            boxed_value(Value::Unit),
            super::aura_direct_map_empty(),
            super::aura_direct_process_null(),
            super::aura_direct_process_null(),
            super::aura_direct_process_null(),
            duration_value(1_000),
            bool_value(false),
        ));
        assert!(expect_variant_value(cancelled_run, "Error", "Cancelled").is_empty());
    });

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
    .expect("cancelled wait child should spawn");
    let cancelled_child_ptr = boxed_value(Value::ProcessChild(cancelled_child));
    with_cancellation_scope(cancellation.clone(), || {
        assert!(expect_variant_ptr(
            super::aura_direct_process_child_wait(cancelled_child_ptr, duration_value(1_000)),
            "Wait",
            "Cancelled",
        )
        .is_empty());
        assert!(expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_child_wait_or_none(
                cancelled_child_ptr,
                duration_value(1_000),
            )),
            "Error",
            "Cancelled",
        )
        .is_empty());
        assert!(expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_child_wait_ok(
                cancelled_child_ptr,
                duration_value(1_000),
            )),
            "Error",
            "Cancelled",
        )
        .is_empty());
        expect_unit(super::aura_direct_process_child_close(cancelled_child_ptr));
    });
    unsafe { release_value(cancelled_child_ptr) };

    let pipe_child = ProcessChildValue::spawn(
        vec!["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("pipe child should spawn");
    let pipe_child_ptr = boxed_value(Value::ProcessChild(pipe_child));
    let stdin_pipe = process_pipe_from_option(
        super::aura_direct_process_child_stdin(pipe_child_ptr),
        "stdin",
    );
    let stdout_pipe = process_pipe_from_option(
        super::aura_direct_process_child_stdout(pipe_child_ptr),
        "stdout",
    );
    assert_process_io_error(expect_result_err_payload(
        super::aura_direct_process_pipe_read_all(boxed_value(Value::ProcessPipe(
            stdin_pipe.clone(),
        ))),
    ));
    assert_process_io_error(expect_result_err_payload(
        super::aura_direct_process_pipe_read_line(
            boxed_value(Value::ProcessPipe(stdin_pipe.clone())),
            duration_value(0),
        ),
    ));
    assert_process_io_error(expect_result_err_payload(
        super::aura_direct_process_pipe_read_bytes(
            boxed_value(Value::ProcessPipe(stdin_pipe.clone())),
            int_value(4),
            duration_value(0),
        ),
    ));
    assert_process_io_error(expect_result_err_payload(
        super::aura_direct_process_pipe_write_all(
            boxed_value(Value::ProcessPipe(stdout_pipe.clone())),
            string_value("payload"),
            duration_value(0),
        ),
    ));
    assert_process_io_error(expect_result_err_payload(
        super::aura_direct_process_pipe_write_bytes(
            boxed_value(Value::ProcessPipe(stdout_pipe.clone())),
            int_vec(&[1, 2, 3]),
            duration_value(0),
        ),
    ));
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(stdin_pipe),
    )));
    expect_unit(super::aura_direct_process_pipe_close(boxed_value(
        Value::ProcessPipe(stdout_pipe),
    )));
    expect_unit(super::aura_direct_process_child_close(pipe_child_ptr));
    unsafe { release_value(pipe_child_ptr) };

    let empty_supervisor_ptr = boxed_value(Value::ProcessSupervisor(ProcessSupervisorValue::new()));
    assert!(expect_variant_ptr(
        super::aura_direct_process_supervisor_wait(empty_supervisor_ptr, duration_value(0)),
        "SupervisorWait",
        "TimedOut",
    )
    .is_empty());
    expect_result_ok_unit(super::aura_direct_process_supervisor_start(
        empty_supervisor_ptr,
        string_value("worker"),
        string_vec(&["/bin/sh", "-c", "sleep 1"]),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        boxed_value(Value::EnumVariant(EnumVariantValue {
            enum_name: "process.RestartPolicy".to_string(),
            variant_name: "Never".to_string(),
            payloads: Vec::new(),
        })),
        duration_value(0),
        int_value(-1),
        bool_value(false),
    ));
    with_cancellation_scope(cancellation, || {
        assert!(
            expect_variant_ptr(
                super::aura_direct_process_supervisor_wait(
                    empty_supervisor_ptr,
                    duration_value(1_000),
                ),
                "SupervisorWait",
                "Cancelled",
            )
            .is_empty()
        );
        assert!(expect_variant_value(
            expect_result_err_payload(super::aura_direct_process_supervisor_wait_or_none(
                empty_supervisor_ptr,
                duration_value(1_000),
            )),
            "Error",
            "Cancelled",
        )
        .is_empty());
    });
    expect_result_ok_unit(super::aura_direct_process_supervisor_stop(
        empty_supervisor_ptr,
    ));
    expect_unit(super::aura_direct_process_supervisor_close(
        empty_supervisor_ptr,
    ));
    unsafe { release_value(empty_supervisor_ptr) };
}

#[test]
fn native_runtime_direct_process_run_wrapper_covers_timeout_result_path() {
    let timed_out = expect_result_err_payload(super::aura_direct_process_run(
        string_vec(&["/bin/sh", "-c", "sleep 1"]),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        duration_value(0),
        bool_value(false),
    ));
    assert!(expect_variant_value(timed_out, "Error", "TimedOut").is_empty());
}

#[test]
fn native_runtime_direct_process_run_wrapper_captures_both_streams() {
    let completed = run_lightweight_root_task(|| {
        Ok(expect_result_ok_payload(super::aura_direct_process_run(
            string_vec(&[
                "/bin/sh",
                "-c",
                "printf aura-stdout; printf aura-stderr >&2",
            ]),
            boxed_value(Value::Unit),
            super::aura_direct_map_empty(),
            super::aura_direct_process_null(),
            super::aura_direct_process_pipe(),
            super::aura_direct_process_pipe(),
            duration_value(1_000),
            bool_value(false),
        )))
    })
    .expect("capturing process.run(...) should complete on the task scheduler");
    let Value::ProcessCompleted(completed) = completed else {
        panic!("process.run(...) should return process.Completed");
    };
    assert!(completed.success());
    assert_eq!(completed.stdout_bytes(), b"aura-stdout");
    assert_eq!(completed.stderr_bytes(), b"aura-stderr");
}

#[test]
fn native_runtime_direct_process_supervisor_wrappers_cover_start_wait_and_stop_paths() {
    fn restart_policy_never() -> *mut OpaqueValue {
        boxed_value(Value::EnumVariant(EnumVariantValue {
            enum_name: "process.RestartPolicy".to_string(),
            variant_name: "Never".to_string(),
            payloads: Vec::new(),
        }))
    }

    let supervisor = match unsafe { take_value(super::aura_direct_process_supervisor()) } {
        Value::ProcessSupervisor(supervisor) => supervisor,
        other => panic!("expected process.Supervisor, found {:?}", other),
    };
    let supervisor_ptr = boxed_value(Value::ProcessSupervisor(supervisor.clone()));
    assert_eq!(
        super::aura_direct_process_supervisor_is_empty(supervisor_ptr),
        1
    );

    let no_command = expect_result_err_payload(super::aura_direct_process_supervisor_start(
        supervisor_ptr,
        string_value("empty"),
        super::aura_direct_vec_empty(),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        restart_policy_never(),
        duration_value(0),
        int_value(-1),
        bool_value(false),
    ));
    assert!(expect_variant_value(no_command, "Error", "NoCommand").is_empty());

    expect_result_ok_unit(super::aura_direct_process_supervisor_start(
        supervisor_ptr,
        string_value("worker"),
        string_vec(&["/bin/sh", "-c", "exit 0"]),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        restart_policy_never(),
        duration_value(0),
        int_value(-1),
        bool_value(false),
    ));
    assert_eq!(
        super::aura_direct_process_supervisor_is_empty(supervisor_ptr),
        0
    );

    let wait_payloads = expect_variant_ptr(
        super::aura_direct_process_supervisor_wait(supervisor_ptr, duration_value(5_000)),
        "SupervisorWait",
        "Event",
    );
    let event = match wait_payloads.as_slice() {
        [Value::EnumVariant(event)] => event,
        other => panic!("expected supervisor event payload, found {:?}", other),
    };
    assert_eq!(event.enum_name, "SupervisorEvent");
    assert_eq!(event.variant_name, "Exited");
    assert!(matches!(event.payloads.as_slice(), [Value::String(name), ..] if name == "worker"));
    assert_eq!(
        super::aura_direct_process_supervisor_is_empty(supervisor_ptr),
        1
    );

    let empty_wait = expect_result_ok_payload(super::aura_direct_process_supervisor_wait_or_none(
        supervisor_ptr,
        duration_value(0),
    ));
    assert!(matches!(
        empty_wait,
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None"
    ));

    expect_result_ok_unit(super::aura_direct_process_supervisor_stop(supervisor_ptr));
    expect_unit(super::aura_direct_process_supervisor_close(supervisor_ptr));
    unsafe { release_value(supervisor_ptr) };
}

#[test]
fn native_runtime_direct_filesystem_wrappers_cover_file_success_paths() {
    let root = std::env::temp_dir().join(format!(
        "aura-native-fs-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("temp fs root should be created");
    let data_path = root.join("data.txt");
    let data = data_path
        .to_str()
        .expect("temp data path should be valid UTF-8")
        .to_string();
    let child_dir_path = root.join("child");
    let child_dir = child_dir_path
        .to_str()
        .expect("temp child path should be valid UTF-8")
        .to_string();
    let second_path = root.join("second.txt");
    let second = second_path
        .to_str()
        .expect("second path should be valid UTF-8")
        .to_string();
    let root_text = root
        .to_str()
        .expect("temp root path should be valid UTF-8")
        .to_string();

    expect_result_ok_unit(super::aura_direct_fs_write_string(
        string_value(&data),
        string_value("one"),
    ));
    assert!(expect_bool_boxed(super::aura_direct_fs_exists(
        string_value(&data)
    )));
    expect_result_ok_unit(super::aura_direct_fs_append_string(
        string_value(&data),
        string_value("two"),
    ));
    assert_eq!(
        expect_result_ok_string(super::aura_direct_fs_read_to_string(string_value(&data))),
        "onetwo"
    );

    expect_result_ok_unit(super::aura_direct_fs_write_bytes(
        string_value(&data),
        int_vec(&[65, 66]),
    ));
    expect_result_ok_unit(super::aura_direct_fs_append_bytes(
        string_value(&data),
        int_vec(&[67]),
    ));
    assert_eq!(
        expect_result_ok_vec_ints(super::aura_direct_fs_read_bytes(string_value(&data))),
        vec![65, 66, 67]
    );

    expect_result_ok_unit(super::aura_direct_fs_create_dir(string_value(&child_dir)));
    let names =
        expect_result_ok_vec_strings(super::aura_direct_fs_read_dir(string_value(&root_text)));
    assert!(names.contains(&"child".to_string()));
    assert!(names.contains(&"data.txt".to_string()));

    let file_payload = expect_variant_ptr(
        super::aura_direct_fs_open(string_value(&data)),
        "Result",
        "Ok",
    );
    let file = match file_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!("expected opened fs.File, found {:?}", other),
    };
    let file_ptr = boxed_value(Value::File(file));
    assert_eq!(
        expect_result_ok_string(super::aura_direct_file_read_all(file_ptr)),
        "ABC"
    );
    expect_unit(super::aura_direct_file_close(file_ptr));
    unsafe { release_value(file_ptr) };

    let bytes_file_payload = expect_variant_ptr(
        super::aura_direct_fs_open(string_value(&data)),
        "Result",
        "Ok",
    );
    let bytes_file = match bytes_file_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!("expected opened fs.File for bytes, found {:?}", other),
    };
    let bytes_file_ptr = boxed_value(Value::File(bytes_file));
    assert_eq!(
        expect_result_ok_vec_ints(super::aura_direct_file_read_bytes(bytes_file_ptr)),
        vec![65, 66, 67]
    );
    expect_unit(super::aura_direct_file_close(bytes_file_ptr));
    unsafe { release_value(bytes_file_ptr) };

    let created_payload = expect_variant_ptr(
        super::aura_direct_fs_create(string_value(&second)),
        "Result",
        "Ok",
    );
    let created = match created_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!("expected created fs.File, found {:?}", other),
    };
    let created_ptr = boxed_value(Value::File(created));
    expect_result_ok_unit(super::aura_direct_file_write_all(
        created_ptr,
        string_value("hi"),
    ));
    expect_result_ok_unit(super::aura_direct_file_write_bytes(
        created_ptr,
        int_vec(&[33]),
    ));
    expect_result_ok_unit(super::aura_direct_file_flush(created_ptr));
    expect_unit(super::aura_direct_file_close(created_ptr));
    unsafe { release_value(created_ptr) };
    assert_eq!(
        expect_result_ok_string(super::aura_direct_fs_read_to_string(string_value(&second))),
        "hi!"
    );

    let append_payload = expect_variant_ptr(
        super::aura_direct_fs_append(string_value(&second)),
        "Result",
        "Ok",
    );
    let append_file = match append_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!("expected append fs.File, found {:?}", other),
    };
    let append_ptr = boxed_value(Value::File(append_file));
    expect_result_ok_unit(super::aura_direct_file_write_all(
        append_ptr,
        string_value(" again"),
    ));
    expect_unit(super::aura_direct_file_close(append_ptr));
    unsafe { release_value(append_ptr) };
    assert_eq!(
        expect_result_ok_string(super::aura_direct_fs_read_to_string(string_value(&second))),
        "hi! again"
    );

    let close_payload = expect_variant_ptr(
        super::aura_direct_fs_open(string_value(&second)),
        "Result",
        "Ok",
    );
    let close_file = match close_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!(
            "expected opened fs.File for close(value), found {:?}",
            other
        ),
    };
    let close_ptr = boxed_value(Value::File(close_file));
    expect_unit(super::aura_direct_close_value(close_ptr, 0));
    unsafe { release_value(close_ptr) };

    expect_result_ok_unit(super::aura_direct_fs_remove_file(string_value(&data)));
    assert!(!expect_bool_boxed(super::aura_direct_fs_exists(
        string_value(&data)
    )));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn native_runtime_direct_filesystem_wrappers_cover_io_error_results() {
    fn expect_io_result_error(ptr: *mut OpaqueValue) {
        assert!(matches!(
            expect_result_err_payload(ptr),
            Value::EnumVariant(_)
        ));
    }

    let root = std::env::temp_dir().join(format!(
        "aura-native-fs-errors-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("temp fs error root should be created");
    let missing = root.join("missing.txt");
    let missing_text = missing
        .to_str()
        .expect("missing path should be valid UTF-8")
        .to_string();
    let directory = root.join("dir");
    std::fs::create_dir_all(&directory).expect("temp child directory should be created");
    let directory_text = directory
        .to_str()
        .expect("directory path should be valid UTF-8")
        .to_string();
    let file_path = root.join("file.txt");
    std::fs::write(&file_path, "data").expect("temp file should be written");
    let file_text = file_path
        .to_str()
        .expect("file path should be valid UTF-8")
        .to_string();
    let invalid_utf8_path = root.join("invalid-utf8.bin");
    std::fs::write(&invalid_utf8_path, [0xff, 0xfe])
        .expect("invalid UTF-8 fixture should be written");
    let invalid_utf8_text = invalid_utf8_path
        .to_str()
        .expect("invalid UTF-8 fixture path should be valid UTF-8")
        .to_string();

    expect_io_result_error(super::aura_direct_fs_read_to_string(string_value(
        &missing_text,
    )));
    expect_io_result_error(super::aura_direct_fs_read_bytes(string_value(
        &missing_text,
    )));
    expect_io_result_error(super::aura_direct_fs_read_dir(string_value(&missing_text)));
    expect_io_result_error(super::aura_direct_fs_open(string_value(&missing_text)));
    expect_io_result_error(super::aura_direct_fs_remove_file(string_value(
        &missing_text,
    )));
    let invalid_data = expect_result_err_payload(super::aura_direct_fs_read_to_string(
        string_value(&invalid_utf8_text),
    ));
    assert!(
        expect_variant_value(invalid_data, "io.Error", "InvalidData").is_empty(),
        "fs.read_to_string must classify non-UTF-8 file contents as io.Error.InvalidData"
    );

    expect_io_result_error(super::aura_direct_fs_write_string(
        string_value(&directory_text),
        string_value("data"),
    ));
    expect_io_result_error(super::aura_direct_fs_write_bytes(
        string_value(&directory_text),
        int_vec(&[1, 2, 3]),
    ));
    expect_io_result_error(super::aura_direct_fs_append_string(
        string_value(&directory_text),
        string_value("data"),
    ));
    expect_io_result_error(super::aura_direct_fs_append_bytes(
        string_value(&directory_text),
        int_vec(&[4, 5, 6]),
    ));
    expect_io_result_error(super::aura_direct_fs_create(string_value(&directory_text)));
    expect_io_result_error(super::aura_direct_fs_append(string_value(&directory_text)));
    expect_io_result_error(super::aura_direct_fs_create_dir(string_value(&file_text)));

    let file_payload = expect_variant_ptr(
        super::aura_direct_fs_open(string_value(&file_text)),
        "Result",
        "Ok",
    );
    let file = match file_payload.as_slice() {
        [Value::File(file)] => file.clone(),
        other => panic!("expected fs.File, found {:?}", other),
    };
    let file_ptr = boxed_value(Value::File(file));
    expect_unit(super::aura_direct_file_close(file_ptr));
    expect_io_result_error(super::aura_direct_file_read_all(file_ptr));
    expect_io_result_error(super::aura_direct_file_read_bytes(file_ptr));
    expect_io_result_error(super::aura_direct_file_write_all(
        file_ptr,
        string_value("closed"),
    ));
    expect_io_result_error(super::aura_direct_file_write_bytes(
        file_ptr,
        int_vec(&[7, 8]),
    ));
    expect_io_result_error(super::aura_direct_file_flush(file_ptr));
    unsafe { release_value(file_ptr) };

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn native_runtime_direct_network_wrappers_cover_tcp_udp_http_success_paths() {
    let timeout = duration_value(5_000);

    let tcp_listener = match expect_result_ok_payload(super::aura_direct_net_listen(string_value(
        "127.0.0.1:0",
    ))) {
        Value::TcpListener(listener) => listener,
        other => panic!("expected net.TcpListener, found {:?}", other),
    };
    let tcp_address = expect_result_ok_string(super::aura_direct_tcp_listener_local_addr(
        boxed_value(Value::TcpListener(tcp_listener.clone())),
    ));
    let tcp_server_listener = tcp_listener.clone();
    let tcp_server = thread::spawn(move || {
        let accepted = match expect_result_ok_payload(super::aura_direct_tcp_listener_accept(
            boxed_value(Value::TcpListener(tcp_server_listener)),
            duration_value(5_000),
        )) {
            Value::TcpStream(stream) => stream,
            other => panic!("expected accepted net.TcpStream, found {:?}", other),
        };
        let accepted_ptr = boxed_value(Value::TcpStream(accepted.clone()));
        let line = expect_option_some_payload(expect_result_ok_payload(
            super::aura_direct_tcp_stream_read_line(accepted_ptr, duration_value(5_000)),
        ));
        assert_eq!(line, Value::String("ping".to_string()));
        expect_result_ok_unit(super::aura_direct_tcp_stream_write_bytes(
            accepted_ptr,
            int_vec(&[112, 111, 110, 103]),
            duration_value(5_000),
        ));
        expect_result_ok_unit(super::aura_direct_tcp_stream_flush(accepted_ptr));
        assert!(
            expect_result_ok_string(super::aura_direct_tcp_stream_local_addr(accepted_ptr))
                .contains("127.0.0.1")
        );
        assert!(
            expect_result_ok_string(super::aura_direct_tcp_stream_peer_addr(accepted_ptr))
                .contains("127.0.0.1")
        );
        expect_unit(super::aura_direct_tcp_stream_close(accepted_ptr));
    });
    let tcp_client = match expect_result_ok_payload(super::aura_direct_net_connect(string_value(
        &tcp_address,
    ))) {
        Value::TcpStream(stream) => stream,
        other => panic!("expected connected net.TcpStream, found {:?}", other),
    };
    let tcp_client_ptr = boxed_value(Value::TcpStream(tcp_client));
    expect_result_ok_unit(super::aura_direct_tcp_stream_write_all(
        tcp_client_ptr,
        string_value("ping\n"),
        timeout,
    ));
    expect_result_ok_unit(super::aura_direct_tcp_stream_shutdown_write(tcp_client_ptr));
    assert_eq!(
        expect_result_ok_vec_ints(super::aura_direct_tcp_stream_read_exact(
            tcp_client_ptr,
            int_value(4),
            timeout,
        )),
        vec![112, 111, 110, 103]
    );
    expect_unit(super::aura_direct_tcp_stream_close(tcp_client_ptr));
    tcp_server
        .join()
        .expect("tcp direct wrapper server should join");
    expect_unit(super::aura_direct_tcp_listener_close(boxed_value(
        Value::TcpListener(tcp_listener),
    )));

    let shutdown_listener = match expect_result_ok_payload(super::aura_direct_net_listen(
        string_value("127.0.0.1:0"),
    )) {
        Value::TcpListener(listener) => listener,
        other => panic!("expected shutdown net.TcpListener, found {:?}", other),
    };
    let shutdown_address = expect_result_ok_string(super::aura_direct_tcp_listener_local_addr(
        boxed_value(Value::TcpListener(shutdown_listener.clone())),
    ));
    let shutdown_server_listener = shutdown_listener.clone();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let shutdown_server = thread::spawn(move || {
        let accepted = match expect_result_ok_payload(super::aura_direct_tcp_listener_accept(
            boxed_value(Value::TcpListener(shutdown_server_listener)),
            duration_value(5_000),
        )) {
            Value::TcpStream(stream) => stream,
            other => panic!(
                "expected shutdown accepted net.TcpStream, found {:?}",
                other
            ),
        };
        let accepted_ptr = boxed_value(Value::TcpStream(accepted));
        accepted_tx
            .send(())
            .expect("shutdown server should signal accepted connection");
        done_rx
            .recv_timeout(StdDuration::from_secs(5))
            .expect("shutdown client should finish");
        expect_unit(super::aura_direct_tcp_stream_close(accepted_ptr));
    });
    let shutdown_client = match expect_result_ok_payload(super::aura_direct_net_connect(
        string_value(&shutdown_address),
    )) {
        Value::TcpStream(stream) => stream,
        other => panic!("expected shutdown client net.TcpStream, found {:?}", other),
    };
    let shutdown_client_ptr = boxed_value(Value::TcpStream(shutdown_client));
    accepted_rx
        .recv_timeout(StdDuration::from_secs(5))
        .expect("shutdown server should accept connection");
    let _ = unsafe {
        take_value(super::aura_direct_tcp_stream_shutdown_read(
            shutdown_client_ptr,
        ))
    };
    let _ = unsafe {
        take_value(super::aura_direct_tcp_stream_shutdown_both(
            shutdown_client_ptr,
        ))
    };
    expect_unit(super::aura_direct_tcp_stream_close(shutdown_client_ptr));
    done_tx
        .send(())
        .expect("shutdown client should signal completion");
    shutdown_server
        .join()
        .expect("tcp shutdown wrapper server should join");
    expect_unit(super::aura_direct_tcp_listener_close(boxed_value(
        Value::TcpListener(shutdown_listener),
    )));

    let udp_sender = match expect_result_ok_payload(super::aura_direct_net_udp_bind(string_value(
        "127.0.0.1:0",
    ))) {
        Value::UdpSocket(socket) => socket,
        other => panic!("expected sender net.UdpSocket, found {:?}", other),
    };
    let udp_receiver = match expect_result_ok_payload(super::aura_direct_net_udp_bind(
        string_value("127.0.0.1:0"),
    )) {
        Value::UdpSocket(socket) => socket,
        other => panic!("expected receiver net.UdpSocket, found {:?}", other),
    };
    let udp_receiver_address = expect_result_ok_string(super::aura_direct_udp_socket_local_addr(
        boxed_value(Value::UdpSocket(udp_receiver.clone())),
    ));
    expect_result_ok_unit(super::aura_direct_udp_socket_send_text(
        boxed_value(Value::UdpSocket(udp_sender.clone())),
        string_value(&udp_receiver_address),
        string_value("hello"),
        timeout,
    ));
    let datagram = match expect_option_some_payload(expect_result_ok_payload(
        super::aura_direct_udp_socket_recv_from(
            boxed_value(Value::UdpSocket(udp_receiver.clone())),
            int_value(64),
            timeout,
        ),
    )) {
        Value::UdpDatagram(datagram) => datagram,
        other => panic!("expected net.UdpDatagram, found {:?}", other),
    };
    let reply_address = expect_string(super::aura_direct_udp_datagram_address(boxed_value(
        Value::UdpDatagram(datagram.clone()),
    )));
    assert_eq!(
        expect_vec_ints(super::aura_direct_udp_datagram_bytes(boxed_value(
            Value::UdpDatagram(datagram.clone()),
        ))),
        vec![104, 101, 108, 108, 111]
    );
    assert_eq!(
        expect_result_ok_string(super::aura_direct_udp_datagram_text(boxed_value(
            Value::UdpDatagram(datagram),
        ))),
        "hello"
    );
    expect_result_ok_unit(super::aura_direct_udp_socket_send_bytes(
        boxed_value(Value::UdpSocket(udp_receiver.clone())),
        string_value(&reply_address),
        int_vec(&[111, 107]),
        timeout,
    ));
    let udp_reply = expect_option_some_payload(expect_result_ok_payload(
        super::aura_direct_udp_socket_recv(
            boxed_value(Value::UdpSocket(udp_sender.clone())),
            int_value(64),
            timeout,
        ),
    ));
    assert_eq!(expect_vec_ints(boxed_value(udp_reply)), vec![111, 107]);
    let udp_peer_error = expect_result_err_payload(super::aura_direct_udp_socket_peer_addr(
        boxed_value(Value::UdpSocket(udp_sender.clone())),
    ));
    assert!(matches!(udp_peer_error, Value::EnumVariant(_)));
    expect_unit(super::aura_direct_udp_socket_close(boxed_value(
        Value::UdpSocket(udp_sender),
    )));
    expect_unit(super::aura_direct_udp_socket_close(boxed_value(
        Value::UdpSocket(udp_receiver),
    )));

    let http_listener = match expect_result_ok_payload(super::aura_direct_net_http_listen(
        string_value("127.0.0.1:0"),
    )) {
        Value::HttpListener(listener) => listener,
        other => panic!("expected net.HttpListener, found {:?}", other),
    };
    let http_address = expect_result_ok_string(super::aura_direct_http_listener_local_addr(
        boxed_value(Value::HttpListener(http_listener.clone())),
    ));
    let http_server_listener = http_listener.clone();
    let http_server = thread::spawn(move || {
        for (path, expected_body, response_body, use_bytes) in [
            ("/direct-text", "hello", "ack", false),
            ("/direct-bytes", "raw", "raw-ok", true),
        ] {
            let exchange = match expect_result_ok_payload(super::aura_direct_http_listener_accept(
                boxed_value(Value::HttpListener(http_server_listener.clone())),
                duration_value(5_000),
            )) {
                Value::HttpExchange(exchange) => exchange,
                other => panic!("expected net.HttpExchange, found {:?}", other),
            };
            assert_eq!(
                expect_string(super::aura_direct_http_exchange_method(boxed_value(
                    Value::HttpExchange(exchange.clone()),
                ))),
                "POST"
            );
            assert_eq!(
                expect_string(super::aura_direct_http_exchange_path(boxed_value(
                    Value::HttpExchange(exchange.clone()),
                ))),
                path
            );
            match unsafe {
                take_value(super::aura_direct_http_exchange_headers(boxed_value(
                    Value::HttpExchange(exchange.clone()),
                )))
            } {
                Value::Map(headers) => assert!(!headers.entries.is_empty()),
                other => panic!("expected HTTP header map, found {:?}", other),
            }
            assert_eq!(
                expect_result_ok_string(super::aura_direct_http_exchange_body_text(boxed_value(
                    Value::HttpExchange(exchange.clone()),
                ))),
                expected_body
            );
            assert_eq!(
                expect_vec_ints(super::aura_direct_http_exchange_body_bytes(boxed_value(
                    Value::HttpExchange(exchange.clone()),
                ))),
                expected_body
                    .as_bytes()
                    .iter()
                    .map(|byte| i128::from(*byte))
                    .collect::<Vec<_>>()
            );
            if use_bytes {
                expect_result_ok_unit(super::aura_direct_http_exchange_respond_bytes(
                    boxed_value(Value::HttpExchange(exchange)),
                    int_value(200),
                    int_vec(
                        &response_body
                            .as_bytes()
                            .iter()
                            .map(|byte| i64::from(*byte))
                            .collect::<Vec<_>>(),
                    ),
                    string_map(&[("x-direct", "bytes")]),
                ));
            } else {
                expect_result_ok_unit(super::aura_direct_http_exchange_respond_text(
                    boxed_value(Value::HttpExchange(exchange)),
                    int_value(200),
                    string_value(response_body),
                    string_map(&[("x-direct", "text")]),
                ));
            }
        }
    });
    let text_response = match expect_result_ok_payload(super::aura_direct_net_http_request_text(
        string_value("POST"),
        string_value(&format!("http://{http_address}/direct-text")),
        string_value("hello"),
        string_map(&[("x-client", "text")]),
    )) {
        Value::HttpResponse(response) => response,
        other => panic!("expected net.HttpResponse, found {:?}", other),
    };
    assert_eq!(
        super::aura_direct_http_response_status(boxed_value(Value::HttpResponse(
            text_response.clone(),
        ))),
        200
    );
    assert_eq!(
        expect_string(super::aura_direct_http_response_reason(boxed_value(
            Value::HttpResponse(text_response.clone()),
        ))),
        "OK"
    );
    match unsafe {
        take_value(super::aura_direct_http_response_headers(boxed_value(
            Value::HttpResponse(text_response.clone()),
        )))
    } {
        Value::Map(headers) => assert!(!headers.entries.is_empty()),
        other => panic!("expected HTTP response header map, found {:?}", other),
    }
    assert_eq!(
        expect_result_ok_string(super::aura_direct_http_response_text(boxed_value(
            Value::HttpResponse(text_response.clone()),
        ))),
        "ack"
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_http_response_bytes(boxed_value(
            Value::HttpResponse(text_response),
        ))),
        vec![97, 99, 107]
    );

    let bytes_response =
        match expect_result_ok_payload(super::aura_direct_net_http_request_bytes_timeout(
            string_value("POST"),
            string_value(&format!("http://{http_address}/direct-bytes")),
            int_vec(&[114, 97, 119]),
            string_map(&[("x-client", "bytes")]),
            timeout,
        )) {
            Value::HttpResponse(response) => response,
            other => panic!("expected net.HttpResponse, found {:?}", other),
        };
    assert_eq!(
        expect_vec_ints(super::aura_direct_http_response_bytes(boxed_value(
            Value::HttpResponse(bytes_response),
        ))),
        vec![114, 97, 119, 45, 111, 107]
    );
    http_server
        .join()
        .expect("http direct wrapper server should join");
    expect_unit(super::aura_direct_http_listener_close(boxed_value(
        Value::HttpListener(http_listener),
    )));

    let websocket_listener = match expect_result_ok_payload(
        super::aura_direct_net_websocket_listen(string_value("127.0.0.1:0")),
    ) {
        Value::WebSocketListener(listener) => listener,
        other => panic!("expected net.WebSocketListener, found {:?}", other),
    };
    let websocket_address =
        expect_result_ok_string(super::aura_direct_websocket_listener_local_addr(
            boxed_value(Value::WebSocketListener(websocket_listener.clone())),
        ));
    let websocket_server_listener = websocket_listener.clone();
    let websocket_server = thread::spawn(move || {
        let server_socket =
            match expect_result_ok_payload(super::aura_direct_websocket_listener_accept(
                boxed_value(Value::WebSocketListener(websocket_server_listener)),
                duration_value(5_000),
            )) {
                Value::WebSocket(socket) => socket,
                other => panic!("expected server net.WebSocket, found {:?}", other),
            };
        let server_ptr = boxed_value(Value::WebSocket(server_socket));
        let text = expect_option_some_payload(expect_result_ok_payload(
            super::aura_direct_websocket_recv_text(server_ptr, duration_value(5_000)),
        ));
        assert_eq!(text, Value::String("hello websocket".to_string()));
        expect_result_ok_unit(super::aura_direct_websocket_send_bytes(
            server_ptr,
            int_vec(&[111, 107]),
            duration_value(5_000),
        ));
        let bytes = expect_option_some_payload(expect_result_ok_payload(
            super::aura_direct_websocket_recv_bytes(server_ptr, duration_value(5_000)),
        ));
        assert_eq!(expect_vec_ints(boxed_value(bytes)), vec![1, 2, 3]);
        expect_result_ok_unit(super::aura_direct_websocket_send_text(
            server_ptr,
            string_value("done"),
            duration_value(5_000),
        ));
        expect_unit(super::aura_direct_websocket_close(server_ptr));
    });
    let websocket_client =
        match expect_result_ok_payload(super::aura_direct_net_websocket_connect_timeout(
            string_value(&format!("ws://{websocket_address}")),
            timeout,
        )) {
            Value::WebSocket(socket) => socket,
            other => panic!("expected client net.WebSocket, found {:?}", other),
        };
    let websocket_client_ptr = boxed_value(Value::WebSocket(websocket_client));
    expect_result_ok_unit(super::aura_direct_websocket_send_text(
        websocket_client_ptr,
        string_value("hello websocket"),
        timeout,
    ));
    let websocket_reply = expect_option_some_payload(expect_result_ok_payload(
        super::aura_direct_websocket_recv_bytes(websocket_client_ptr, timeout),
    ));
    assert_eq!(
        expect_vec_ints(boxed_value(websocket_reply)),
        vec![111, 107]
    );
    expect_result_ok_unit(super::aura_direct_websocket_send_bytes(
        websocket_client_ptr,
        int_vec(&[1, 2, 3]),
        timeout,
    ));
    let websocket_done = expect_option_some_payload(expect_result_ok_payload(
        super::aura_direct_websocket_recv_text(websocket_client_ptr, timeout),
    ));
    assert_eq!(websocket_done, Value::String("done".to_string()));
    expect_unit(super::aura_direct_websocket_close(websocket_client_ptr));
    websocket_server
        .join()
        .expect("websocket direct wrapper server should join");

    #[cfg(unix)]
    {
        let unix_socket_path = format!(
            "/tmp/a-ndw-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
                % 1_000_000
        );
        let _ = std::fs::remove_file(&unix_socket_path);
        let unix_listener = match expect_result_ok_payload(super::aura_direct_net_unix_listen(
            string_value(&unix_socket_path),
        )) {
            Value::UnixListener(listener) => listener,
            other => panic!("expected net.UnixListener, found {:?}", other),
        };
        let unix_server_listener = unix_listener.clone();
        let unix_server = thread::spawn(move || {
            let server_stream =
                match expect_result_ok_payload(super::aura_direct_unix_listener_accept(
                    boxed_value(Value::UnixListener(unix_server_listener)),
                    duration_value(5_000),
                )) {
                    Value::UnixStream(stream) => stream,
                    other => panic!("expected server net.UnixStream, found {:?}", other),
                };
            let server_ptr = boxed_value(Value::UnixStream(server_stream));
            let line = expect_option_some_payload(expect_result_ok_payload(
                super::aura_direct_unix_stream_read_line(server_ptr, duration_value(5_000)),
            ));
            assert_eq!(line, Value::String("hello unix".to_string()));
            expect_result_ok_unit(super::aura_direct_unix_stream_write_all(
                server_ptr,
                string_value("unix-ok"),
                duration_value(5_000),
            ));
            expect_unit(super::aura_direct_unix_stream_close(server_ptr));
        });
        let unix_client = match expect_result_ok_payload(
            super::aura_direct_net_unix_connect_timeout(string_value(&unix_socket_path), timeout),
        ) {
            Value::UnixStream(stream) => stream,
            other => panic!("expected client net.UnixStream, found {:?}", other),
        };
        let unix_client_ptr = boxed_value(Value::UnixStream(unix_client));
        expect_result_ok_unit(super::aura_direct_unix_stream_write_all(
            unix_client_ptr,
            string_value("hello unix\n"),
            timeout,
        ));
        assert_eq!(
            expect_result_ok_vec_ints(super::aura_direct_unix_stream_read_exact(
                unix_client_ptr,
                int_value(7),
                timeout,
            )),
            vec![117, 110, 105, 120, 45, 111, 107]
        );
        expect_unit(super::aura_direct_unix_stream_close(unix_client_ptr));
        unix_server
            .join()
            .expect("unix direct wrapper server should join");
        expect_unit(super::aura_direct_unix_listener_close(boxed_value(
            Value::UnixListener(unix_listener),
        )));
        let _ = std::fs::remove_file(&unix_socket_path);
    }
}

#[test]
fn native_runtime_direct_network_wrappers_cover_timeout_and_error_results() {
    fn expect_io_result_error(ptr: *mut OpaqueValue) {
        assert!(matches!(
            expect_result_err_payload(ptr),
            Value::EnumVariant(_)
        ));
    }

    expect_io_result_error(super::aura_direct_net_connect(string_value("127.0.0.1:0")));
    expect_io_result_error(super::aura_direct_net_connect_timeout(
        string_value("127.0.0.1:0"),
        duration_value(1),
    ));
    expect_io_result_error(super::aura_direct_net_listen(string_value(
        "127.0.0.1:not-a-port",
    )));

    let tcp_listener = match expect_result_ok_payload(super::aura_direct_net_listen(string_value(
        "127.0.0.1:0",
    ))) {
        Value::TcpListener(listener) => listener,
        other => panic!("expected timeout net.TcpListener, found {:?}", other),
    };
    expect_io_result_error(super::aura_direct_tcp_listener_accept(
        boxed_value(Value::TcpListener(tcp_listener.clone())),
        duration_value(0),
    ));
    expect_unit(super::aura_direct_tcp_listener_close(boxed_value(
        Value::TcpListener(tcp_listener),
    )));

    expect_io_result_error(super::aura_direct_net_udp_bind(string_value(
        "127.0.0.1:not-a-port",
    )));
    let udp_socket = match expect_result_ok_payload(super::aura_direct_net_udp_bind(string_value(
        "127.0.0.1:0",
    ))) {
        Value::UdpSocket(socket) => socket,
        other => panic!("expected timeout net.UdpSocket, found {:?}", other),
    };
    let udp_recv = expect_result_ok_payload(super::aura_direct_udp_socket_recv(
        boxed_value(Value::UdpSocket(udp_socket.clone())),
        int_value(16),
        duration_value(0),
    ));
    assert!(matches!(
        udp_recv,
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None"
    ));
    let udp_recv_from = expect_result_ok_payload(super::aura_direct_udp_socket_recv_from(
        boxed_value(Value::UdpSocket(udp_socket.clone())),
        int_value(16),
        duration_value(0),
    ));
    assert!(matches!(
        udp_recv_from,
        Value::EnumVariant(variant)
            if variant.enum_name == "Option" && variant.variant_name == "None"
    ));
    expect_unit(super::aura_direct_udp_socket_close(boxed_value(
        Value::UdpSocket(udp_socket),
    )));

    expect_io_result_error(super::aura_direct_net_http_listen(string_value(
        "127.0.0.1:not-a-port",
    )));
    let http_listener = match expect_result_ok_payload(super::aura_direct_net_http_listen(
        string_value("127.0.0.1:0"),
    )) {
        Value::HttpListener(listener) => listener,
        other => panic!("expected timeout net.HttpListener, found {:?}", other),
    };
    expect_io_result_error(super::aura_direct_http_listener_accept(
        boxed_value(Value::HttpListener(http_listener.clone())),
        duration_value(0),
    ));
    expect_unit(super::aura_direct_http_listener_close(boxed_value(
        Value::HttpListener(http_listener),
    )));

    expect_io_result_error(super::aura_direct_net_http_request_text_timeout(
        string_value("GET"),
        string_value("not-a-url"),
        string_value(""),
        string_map(&[]),
        duration_value(1),
    ));
    expect_io_result_error(super::aura_direct_net_http_request_bytes(
        string_value("POST"),
        string_value("not-a-url"),
        int_vec(&[1, 2]),
        string_map(&[]),
    ));

    expect_io_result_error(super::aura_direct_net_tls_listen(
        string_value("127.0.0.1:0"),
        string_value("/tmp/aura-missing-cert.pem"),
        string_value("/tmp/aura-missing-key.pem"),
    ));
    expect_io_result_error(super::aura_direct_net_tls_connect(
        string_value("127.0.0.1:0"),
        string_value("localhost"),
        string_value("/tmp/aura-missing-ca.pem"),
    ));
    expect_io_result_error(super::aura_direct_net_tls_connect_timeout(
        string_value("127.0.0.1:0"),
        string_value("localhost"),
        string_value("/tmp/aura-missing-ca.pem"),
        duration_value(1),
    ));

    expect_io_result_error(super::aura_direct_net_websocket_listen(string_value(
        "127.0.0.1:not-a-port",
    )));
    expect_io_result_error(super::aura_direct_net_websocket_connect(string_value(
        "not-a-url",
    )));

    let websocket_listener = match expect_result_ok_payload(
        super::aura_direct_net_websocket_listen(string_value("127.0.0.1:0")),
    ) {
        Value::WebSocketListener(listener) => listener,
        other => panic!("expected timeout net.WebSocketListener, found {:?}", other),
    };
    expect_io_result_error(super::aura_direct_websocket_listener_accept(
        boxed_value(Value::WebSocketListener(websocket_listener.clone())),
        duration_value(0),
    ));
    expect_unit(super::aura_direct_close_value(
        boxed_value(Value::WebSocketListener(websocket_listener)),
        0,
    ));

    #[cfg(unix)]
    {
        let unix_socket_path = format!(
            "/tmp/a-ndw-error-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
                % 1_000_000
        );
        let _ = std::fs::remove_file(&unix_socket_path);
        let unix_listener = match expect_result_ok_payload(super::aura_direct_net_unix_listen(
            string_value(&unix_socket_path),
        )) {
            Value::UnixListener(listener) => listener,
            other => panic!("expected timeout net.UnixListener, found {:?}", other),
        };
        expect_io_result_error(super::aura_direct_unix_listener_accept(
            boxed_value(Value::UnixListener(unix_listener.clone())),
            duration_value(0),
        ));
        expect_unit(super::aura_direct_unix_listener_close(boxed_value(
            Value::UnixListener(unix_listener),
        )));
        let _ = std::fs::remove_file(&unix_socket_path);
        expect_io_result_error(super::aura_direct_net_unix_connect(string_value(
            &unix_socket_path,
        )));
    }
}

#[test]
fn sqrt_helper_matches_standard_library() {
    assert_eq!(super::aura_direct_sqrt_f64(25.0), 5.0);
}

#[test]
fn direct_runtime_string_and_numeric_helpers_cover_builtin_surface() {
    assert_eq!(
        super::aura_direct_string_len(string_value("é🎉e\u{301}")),
        4
    );
    assert_eq!(
        super::aura_direct_string_byte_len(string_value("é🎉e\u{301}")),
        9
    );
    assert_eq!(
        super::aura_direct_string_contains(string_value("  Aura Repo  "), string_value("Repo"),),
        1
    );
    assert_eq!(
        super::aura_direct_string_starts_with(string_value("  Aura Repo  "), string_value("  A"),),
        1
    );
    assert_eq!(
        super::aura_direct_string_ends_with(string_value("  Aura Repo  "), string_value("o  "),),
        1
    );
    assert_eq!(
        expect_vec_strings(super::aura_direct_string_split(
            string_value("a,b,c"),
            string_value(","),
        )),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(
        expect_string(super::aura_direct_string_replace(
            string_value("Aura compiler"),
            string_value("compiler"),
            string_value("runtime"),
        )),
        "Aura runtime"
    );
    assert_eq!(
        expect_string(super::aura_direct_string_to_lower(string_value("AuRa"))),
        "aura"
    );
    assert_eq!(
        expect_string(super::aura_direct_string_to_upper(string_value("AuRa"))),
        "AURA"
    );
    assert_eq!(
        expect_option_some_string(super::aura_direct_string_strip_prefix(
            string_value("prefix-core"),
            string_value("prefix-"),
        )),
        "core"
    );
    expect_option_none(super::aura_direct_string_strip_prefix(
        string_value("prefix-core"),
        string_value("core"),
    ));
    assert_eq!(
        expect_option_some_string(super::aura_direct_string_strip_suffix(
            string_value("core-suffix"),
            string_value("-suffix"),
        )),
        "core"
    );
    expect_option_none(super::aura_direct_string_strip_suffix(
        string_value("core-suffix"),
        string_value("prefix"),
    ));
    assert_eq!(
        expect_string(super::aura_direct_string_trim(string_value(" \tAura\n"))),
        "Aura"
    );
    assert_eq!(
        expect_string(super::aura_direct_string_join(
            string_value(", "),
            string_vec(&["Ada", "Linus", "Grace"]),
        )),
        "Ada, Linus, Grace"
    );
    assert_eq!(expect_int(super::aura_direct_abs(int_value(-7))), 7);
    assert_eq!(expect_int(super::aura_direct_abs(int_value(7))), 7);
    assert_eq!(expect_float(super::aura_direct_abs(float_value(-3.5))), 3.5);
    assert_eq!(
        expect_int(super::aura_direct_min(int_value(4), int_value(9))),
        4
    );
    assert_eq!(
        expect_int(super::aura_direct_min(int_value(9), int_value(4))),
        4
    );
    assert_eq!(
        expect_float(super::aura_direct_min(float_value(4.5), float_value(9.5))),
        4.5
    );
    assert_eq!(
        expect_float(super::aura_direct_min(float_value(9.5), float_value(4.5))),
        4.5
    );
    assert_eq!(
        expect_int(super::aura_direct_max(int_value(4), int_value(9))),
        9
    );
    assert_eq!(
        expect_int(super::aura_direct_max(int_value(9), int_value(4))),
        9
    );
    assert_eq!(
        expect_float(super::aura_direct_max(float_value(4.5), float_value(9.5))),
        9.5
    );
    assert_eq!(
        expect_float(super::aura_direct_max(float_value(9.5), float_value(4.5))),
        9.5
    );
    assert_eq!(
        expect_float(super::aura_direct_sqrt(float_value(81.0))),
        9.0
    );
    assert_eq!(
        expect_result_ok_int(super::aura_direct_parse_int32(string_value("123"))),
        123
    );
    assert_eq!(
        expect_result_ok_int(super::aura_direct_parse_int64(string_value("-456"))),
        -456
    );
    assert_eq!(
        expect_result_ok_float(super::aura_direct_parse_float64(string_value("1.5e2"))),
        150.0
    );
    assert!(
        expect_result_err_string(super::aura_direct_parse_int32(string_value("oops")))
            .contains("invalid")
    );
    assert!(
        expect_result_err_string(super::aura_direct_parse_int64(string_value("oops")))
            .contains("invalid")
    );
    assert!(
        expect_result_err_string(super::aura_direct_parse_float64(string_value("oops")))
            .contains("invalid")
    );
    assert!(
        expect_result_err_string(super::aura_direct_parse_float64(string_value("inf")))
            .contains("float must be finite")
    );
    assert_eq!(
        expect_string(super::aura_direct_stringify_value(bool_value(true))),
        "true"
    );
    expect_unit(super::aura_direct_box_unit());
    assert_eq!(
        expect_int(super::aura_direct_box_uint_literal(b"42".as_ptr(), 2)),
        42
    );
    assert_eq!(
        expect_string(super::aura_direct_stringify_value(duration_value(5))),
        "5ms"
    );
}

#[test]
fn direct_runtime_round_and_divmod_use_shared_checked_numeric_contracts() {
    assert_eq!(expect_int(super::aura_direct_round(int_value(7))), 7);
    for (value, expected) in [(1.5, 2), (2.5, 2), (-1.5, -2), (-2.5, -2)] {
        assert_eq!(
            expect_int(super::aura_direct_round(float_value(value))),
            expected
        );
    }

    let pair = super::aura_direct_divmod(int_value(-7), int_value(3));
    let pair_value = unsafe { value_ref(pair) };
    assert_eq!(
        pair_value,
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64"), Type::named("int64")],
            elements: vec![
                Value::Int(IntegerValue::from_i64(-3)),
                Value::Int(IntegerValue::from_i64(2)),
            ],
        })
    );
    unsafe { release_value(pair) };

    let round_error = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_round(float_value(f64::INFINITY));
            Ok(Value::Unit)
        })
    })
    .expect_err("non-finite round must trap");
    assert_eq!(round_error.code, "AU4002");

    let divmod_error = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_divmod(int_value(1), int_value(0));
            Ok(Value::Unit)
        })
    })
    .expect_err("zero divmod divisor must trap");
    assert_eq!(divmod_error.code, "AU4004");
    assert_eq!(divmod_error.message, "`divmod(...)` divisor cannot be zero");
}

#[test]
fn direct_owned_slice_runtime_copies_values_and_preserves_au4003_spans() {
    let source = int_vec(&[10, 20, 30, 40]);
    let source_elements = unsafe {
        super::with_value(source, |value| match value {
            Value::Vec(vector) => vector.elements.as_ptr(),
            other => panic!("expected Vec, found {other:?}"),
        })
    };
    let middle = super::aura_direct_vec_slice(source, -3, 1, -1, 1, 8, 13);
    unsafe {
        super::with_value(middle, |value| match value {
            Value::Vec(vector) => {
                assert_eq!(vector.element_type, Type::named("uint8"));
                assert_ne!(
                    vector.elements.as_ptr(),
                    source_elements,
                    "a direct Vec slice must own fresh element storage"
                );
            }
            other => panic!("expected Vec slice, found {other:?}"),
        });
    }
    expect_unit(super::aura_direct_vec_set_index_in_place(
        source,
        1,
        int_value(99),
        8,
        13,
    ));
    assert_eq!(expect_vec_ints(middle), vec![20, 30]);
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(source)),
        vec![10, 99, 30, 40],
        "mutating the source must not mutate the owned slice"
    );
    unsafe { release_value(source) };

    let text = string_value("aé🎉e\u{301}");
    let text_address = unsafe {
        super::with_value(text, |value| match value {
            Value::String(text) => text.as_ptr(),
            other => panic!("expected str, found {other:?}"),
        })
    };
    let text_slice = super::aura_direct_string_slice(text, 1, 1, 4, 1, 9, 7);
    unsafe {
        super::with_value(text_slice, |value| match value {
            Value::String(slice) => assert_ne!(
                slice.as_ptr(),
                text_address,
                "a direct str slice must own fresh UTF-8 storage"
            ),
            other => panic!("expected str slice, found {other:?}"),
        });
    }
    assert_eq!(expect_string(text_slice), "é🎉e");
    assert_eq!(expect_string(text), "aé🎉e\u{301}");

    let source = string_value("abc");
    let source_address = source as usize;
    let out_of_range = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_string_slice(
                source_address as *mut OpaqueValue,
                0,
                0,
                4,
                1,
                11,
                5,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("direct str slicing must reject rather than clamp");
    unsafe { release_value(source) };
    assert_eq!(out_of_range.code, "AU4003");
    assert_eq!(out_of_range.message, "slice end `4` is outside `0..=3`");
    assert_eq!(out_of_range.span, Some(Span::new(11, 5)));

    let source = int_vec(&[1, 2, 3]);
    let source_address = source as usize;
    let reversed = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ =
                super::aura_direct_vec_slice(source_address as *mut OpaqueValue, 3, 1, 1, 1, 12, 4);
            Ok(Value::Unit)
        })
    })
    .expect_err("direct Vec slicing must reject reversed normalized bounds");
    unsafe { release_value(source) };
    assert_eq!(reversed.code, "AU4003");
    assert_eq!(
        reversed.message,
        "slice start `3` is greater than slice end `1`"
    );
    assert_eq!(reversed.span, Some(Span::new(12, 4)));

    let wrong_string_receiver = int_value(7);
    let wrong_string_receiver_address = wrong_string_receiver as usize;
    let wrong_receiver = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_string_slice(
                wrong_string_receiver_address as *mut OpaqueValue,
                0,
                0,
                0,
                0,
                13,
                2,
            );
            Ok(Value::Unit)
        })
    })
    .expect_err("direct str slicing must reject the wrong receiver type");
    unsafe { release_value(wrong_string_receiver) };
    assert_eq!(wrong_receiver.message, "expected `str`, found `integer`");
}

#[test]
fn direct_runtime_vec_helpers_cover_collection_surface() {
    let vec = super::aura_direct_vec_empty();
    assert_eq!(super::aura_direct_vec_len(vec), 0);
    assert_eq!(super::aura_direct_vec_is_empty(vec), 1);

    expect_unit(super::aura_direct_vec_push_in_place(vec, int_value(1)));
    expect_unit(super::aura_direct_vec_push_in_place(vec, int_value(2)));
    expect_unit(super::aura_direct_vec_push_in_place(vec, int_value(3)));
    assert_eq!(super::aura_direct_vec_len(vec), 3);
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_pop_in_place(vec)),
        3
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_get(vec, 1)),
        2
    );
    assert_eq!(
        expect_int(super::aura_direct_vec_set_in_place(vec, 1, int_value(5))),
        2
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_remove_in_place(vec, 0)),
        1
    );
    assert_eq!(super::aura_direct_vec_contains(vec, int_value(5)), 1);
    assert_eq!(
        super::aura_direct_vec_insert_in_place(vec, 1, int_value(8)),
        1
    );
    assert_eq!(super::aura_direct_vec_swap_in_place(vec, 0, 1), 1);
    expect_unit(super::aura_direct_vec_reverse_in_place(vec));
    assert_eq!(expect_int(super::aura_direct_vec_index(vec, 0, 1, 1)), 5);
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_index_option(vec, 1)),
        8
    );
    expect_unit(super::aura_direct_vec_set_index_in_place(
        vec,
        1,
        int_value(42),
        1,
        1,
    ));
    expect_unit(super::aura_direct_vec_extend_in_place(
        vec,
        int_vec(&[7, 9]),
    ));
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(vec)),
        vec![5, 42, 7, 9]
    );
    expect_unit(super::aura_direct_vec_clear_in_place(vec));
    assert_eq!(super::aura_direct_vec_len(vec), 0);
    expect_option_none(super::aura_direct_vec_pop_in_place(vec));
    expect_option_none(super::aura_direct_vec_index_option(vec, 0));

    let draining_vec = int_vec(&[10]);
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_take_index_in_place(draining_vec, 0,)),
        10
    );
    expect_option_none(super::aura_direct_vec_take_index_in_place(draining_vec, 0));
}

#[test]
fn direct_runtime_vec_helpers_normalize_negative_indices_uniformly() {
    let vec = int_vec(&[10, 20, 30, 40]);

    assert_eq!(expect_int(super::aura_direct_vec_index(vec, -1, 1, 1)), 40);
    expect_unit(super::aura_direct_vec_set_index_in_place(
        vec,
        -2,
        int_value(35),
        1,
        1,
    ));
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_get(vec, -2)),
        35
    );
    expect_option_none(super::aura_direct_vec_get(vec, -5));
    assert_eq!(
        expect_int(super::aura_direct_vec_set_in_place(vec, -4, int_value(11),)),
        10
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_remove_in_place(vec, -2)),
        35
    );
    assert_eq!(super::aura_direct_vec_swap_in_place(vec, -1, -3), 1);
    assert_eq!(
        super::aura_direct_vec_insert_in_place(vec, -1, int_value(99)),
        1
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(vec)),
        vec![40, 20, 99, 11]
    );

    let clamped = int_vec(&[1, 2]);
    assert_eq!(
        super::aura_direct_vec_insert_in_place(clamped, -100, int_value(0)),
        1
    );
    assert_eq!(
        super::aura_direct_vec_insert_in_place(clamped, 100, int_value(3)),
        1
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(clamped)),
        vec![0, 1, 2, 3]
    );
}

#[test]
fn direct_runtime_map_and_set_helpers_cover_collection_surface() {
    let map = super::aura_direct_map_empty();
    assert_eq!(super::aura_direct_map_len(map), 0);
    assert_eq!(super::aura_direct_map_is_empty(map), 1);
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("name"),
        int_value(1),
    ));
    assert_eq!(
        expect_option_some_int(super::aura_direct_map_set_in_place(
            map,
            string_value("name"),
            int_value(2),
        )),
        1
    );
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("count"),
        int_value(3),
    ));
    assert_eq!(
        expect_option_some_int(super::aura_direct_map_get(map, string_value("name"))),
        2
    );
    assert_eq!(
        super::aura_direct_map_contains_key(map, string_value("count")),
        1
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_map_remove_in_place(
            map,
            string_value("count"),
        )),
        3
    );
    assert_eq!(
        expect_vec_strings(super::aura_direct_map_keys(map)),
        vec!["name".to_string()]
    );
    assert_eq!(expect_vec_ints(super::aura_direct_map_values(map)), vec![2]);

    let entries = unsafe { take_value(super::aura_direct_map_items(map)) };
    match entries {
        Value::Vec(values) => {
            assert_eq!(values.elements.len(), 1);
            let Value::Tuple(entry) = &values.elements[0] else {
                panic!("expected map item tuple");
            };
            assert_eq!(
                entry.elements.first(),
                Some(&Value::String("name".to_string()))
            );
            assert_eq!(
                entry.elements.get(1),
                Some(&Value::Int(IntegerValue::from_signed(2)))
            );
        }
        other => panic!("expected vec of map entries, found {:?}", other),
    }
    assert_eq!(
        expect_int(super::aura_direct_map_index(
            map,
            string_value("name"),
            1,
            1,
        )),
        2
    );
    expect_unit(super::aura_direct_map_set_index_in_place(
        map,
        string_value("status"),
        int_value(7),
        1,
        1,
    ));
    expect_unit(super::aura_direct_map_extend_in_place(map, {
        let other = super::aura_direct_map_empty();
        expect_option_none(super::aura_direct_map_set_in_place(
            other,
            string_value("status"),
            int_value(9),
        ));
        other
    }));
    assert_eq!(
        expect_vec_ints(super::aura_direct_map_values(map)),
        vec![2, 9]
    );
    expect_unit(super::aura_direct_map_clear_in_place(map));
    assert_eq!(super::aura_direct_map_len(map), 0);
    expect_option_none(super::aura_direct_map_get(map, string_value("missing")));
    expect_option_none(super::aura_direct_map_remove_in_place(
        map,
        string_value("missing"),
    ));
    assert_eq!(
        super::aura_direct_map_contains_key(map, string_value("missing")),
        0
    );

    let set = super::aura_direct_set_empty();
    assert_eq!(super::aura_direct_set_len(set), 0);
    assert_eq!(super::aura_direct_set_is_empty(set), 1);
    assert_eq!(super::aura_direct_set_insert_in_place(set, int_value(3)), 1);
    assert_eq!(super::aura_direct_set_insert_in_place(set, int_value(3)), 0);
    assert_eq!(super::aura_direct_set_contains(set, int_value(3)), 1);
    assert_eq!(
        expect_option_some_int(super::aura_direct_set_index_option(set, 0)),
        3
    );
    assert_eq!(super::aura_direct_set_remove_in_place(set, int_value(3)), 1);
    expect_option_none(super::aura_direct_set_index_option(set, 0));
    assert_eq!(super::aura_direct_set_remove_in_place(set, int_value(3)), 0);
}

unsafe extern "C-unwind" fn test_native_thunk(args: *const i64, len: usize) -> *mut OpaqueValue {
    let args = std::slice::from_raw_parts(args, len);
    let total = args
        .iter()
        .map(|arg| match value_ref(*arg as *mut OpaqueValue) {
            Value::Int(value) => value.as_i128().expect("expected signed integer") as i64,
            other => panic!("expected int arg, found {:?}", other),
        })
        .sum();
    for argument in args.iter().copied() {
        super::aura_direct_release_value(argument as *mut OpaqueValue);
    }
    super::aura_direct_box_i64(total)
}

unsafe extern "C-unwind" fn direct_closure_add_and_increment_mut_arg(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert_eq!(len, 2, "expected one capture and one public argument");
    let args = unsafe { std::slice::from_raw_parts_mut(args as *mut i64, len) };
    let capture = std::mem::replace(&mut args[0], 0) as *mut OpaqueValue;
    let public = std::mem::replace(&mut args[1], 0) as *mut OpaqueValue;
    let capture_value = match unsafe { value_ref(capture) } {
        Value::Int(value) => value.as_i128().expect("expected signed capture"),
        other => panic!("expected int capture, found {other:?}"),
    };
    let public_value = match unsafe { value_ref(public) } {
        Value::Int(value) => value.as_i128().expect("expected signed public argument"),
        other => panic!("expected int public argument, found {other:?}"),
    };
    unsafe {
        release_value(capture);
        release_value(public);
    }
    args[1] = int_value((public_value + 1) as i64) as i64;
    int_value((capture_value + public_value) as i64)
}

unsafe extern "C-unwind" fn direct_closure_returns_capture(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert_eq!(len, 1, "expected one captured argument");
    let args = unsafe { std::slice::from_raw_parts_mut(args as *mut i64, len) };
    let capture = std::mem::replace(&mut args[0], 0) as *mut OpaqueValue;
    let captured = match unsafe { value_ref(capture) } {
        Value::Int(value) => value.as_i128().expect("expected signed capture"),
        other => panic!("expected int capture, found {other:?}"),
    };
    unsafe {
        release_value(capture);
    }
    int_value(captured as i64)
}

unsafe extern "C-unwind" fn direct_closure_consumes_owned_and_writes_mut(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert_eq!(len, 3, "expected one capture and two public arguments");
    let args = unsafe { std::slice::from_raw_parts_mut(args as *mut i64, len) };
    let capture = std::mem::replace(&mut args[0], 0) as *mut OpaqueValue;
    let owned = std::mem::replace(&mut args[1], 0) as *mut OpaqueValue;
    let mutable = std::mem::replace(&mut args[2], 0) as *mut OpaqueValue;
    let read_int = |value| match unsafe { value_ref(value) } {
        Value::Int(value) => value.as_i128().expect("expected signed integer"),
        other => panic!("expected int closure argument, found {other:?}"),
    };
    let capture_value = read_int(capture);
    let owned_value = read_int(owned);
    let mutable_value = read_int(mutable);
    unsafe {
        release_value(capture);
        release_value(owned);
        release_value(mutable);
    }
    args[2] = int_value((mutable_value + 5) as i64) as i64;
    int_value((capture_value + owned_value + mutable_value) as i64)
}

unsafe extern "C-unwind" fn direct_zero_capture_closure(
    _args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert_eq!(len, 0, "zero-capture closure received hidden arguments");
    int_value(42)
}

unsafe extern "C-unwind" fn direct_test_default_binder(
    args: *mut i64,
    len: usize,
    transfer_defaults: i64,
) {
    assert_eq!(len, 1, "default binder received the wrong arity");
    assert_eq!(
        transfer_defaults, 0,
        "this callback probe keeps its default in the active ownership ledger"
    );
    if unsafe { *args } == 0 {
        unsafe {
            *args = int_value(41) as i64;
        }
    }
}

#[test]
fn native_runtime_task_result_handoff_clones_copy_values_and_moves_noncopy_values() {
    let external_duration = duration_value(125);
    let external_queue = boxed_value(Value::Channel(ChannelValue::new()));
    let external_duration_address = external_duration as usize;
    let external_queue_address = external_queue as usize;

    let result = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let external_duration = external_duration_address as *mut OpaqueValue;
            let duration_result = unsafe {
                super::consume_direct_task_result(
                    super::aura_direct_retain_value(external_duration),
                    true,
                )
            };
            assert_eq!(
                duration_result,
                Value::Duration(125 * crate::runtime_value::NANOS_PER_MILLISECOND)
            );
            assert_eq!(
                unsafe { value_ref(external_duration) },
                Value::Duration(125 * crate::runtime_value::NANOS_PER_MILLISECOND),
                "copy-result handoff must not empty a shared Duration wrapper"
            );

            let external_queue = external_queue_address as *mut OpaqueValue;
            let queue_result = unsafe {
                super::consume_direct_task_result(
                    super::aura_direct_retain_value(external_queue),
                    true,
                )
            };
            let Value::Channel(returned_queue) = queue_result else {
                panic!("copy-result handoff should preserve Queue values");
            };
            returned_queue
                .send(Value::Int(IntegerValue::from_signed(9)))
                .expect("returned Queue alias should remain open");
            let received = unsafe {
                super::with_value(external_queue, |value| {
                    let Value::Channel(queue) = value else {
                        panic!("external Queue wrapper should remain intact");
                    };
                    queue.recv_with_cancellation(None, None)
                })
            }
            .expect("external Queue alias should receive the returned alias's value");
            assert_eq!(received, Some(Value::Int(IntegerValue::from_signed(9))));

            let owned_string = string_value("allocation identity");
            let allocation = unsafe {
                super::with_value(owned_string, |value| match value {
                    Value::String(value) => value.as_ptr(),
                    other => panic!("expected str, found {other:?}"),
                })
            };
            let moved = unsafe { super::consume_direct_task_result(owned_string, false) };
            let Value::String(moved) = moved else {
                panic!("noncopy result handoff should move str values");
            };
            assert_eq!(
                moved.as_ptr(),
                allocation,
                "noncopy result handoff must preserve the owned allocation"
            );

            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "task-result handoff must balance every opaque wrapper reference"
                );
            });
            Ok(Value::Unit)
        })
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    unsafe {
        release_value(external_duration);
        release_value(external_queue);
    }
}

unsafe extern "C-unwind" fn direct_task_fresh_duration(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null() || arg_count == 0);
    assert_eq!(arg_count, 0);
    duration_value(125)
}

unsafe extern "C-unwind" fn direct_task_sends_to_captured_queue(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert_eq!(arg_count, 1);
    assert!(!args.is_null());
    let queue = unsafe { *args } as *mut OpaqueValue;
    let sent = super::aura_direct_channel_send(queue, int_value(41));
    unsafe {
        super::aura_direct_release_value(sent);
        super::aura_direct_release_value(queue);
    }
    super::aura_direct_box_unit()
}

unsafe extern "C-unwind" fn direct_task_violates_owned_ledger_invariant(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null() || arg_count == 0);
    assert_eq!(arg_count, 0);
    let _intentionally_unreleased = string_value("late task-scope unwind");
    int_value(0)
}

unsafe extern "C-unwind" fn direct_task_panics_before_result_handoff(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert_eq!(arg_count, 1);
    assert!(!args.is_null());
    panic!("ordinary task panic before result handoff")
}

#[test]
fn native_runtime_direct_yield_allows_a_queued_task_to_make_observable_progress() {
    let child_ran = Arc::new(AtomicBool::new(false));
    let child_probe = child_ran.clone();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(1, move || {
        let child = spawn_lightweight_task(move || {
            child_probe.store(true, Ordering::Release);
            Ok(Value::Unit)
        })?;
        assert!(
            !child_ran.load(Ordering::Acquire),
            "the only worker remains in the root task before it yields"
        );

        super::aura_direct_yield_now();
        assert!(
            child_ran.load(Ordering::Acquire),
            "the direct yield wrapper must let the queued child execute"
        );
        match child
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Ok(Value::Unit)) => Ok(Value::Unit),
            other => Err(Diagnostic::new(format!(
                "yielded child should complete successfully, found {other:?}"
            ))),
        }
    });
    assert_eq!(
        result.expect("direct yield scheduling probe should complete"),
        Value::Unit
    );
}

static DIRECT_ROOT_TEST_OWNED_VALUE: AtomicUsize = AtomicUsize::new(0);
static DIRECT_TEARDOWN_TASK_STARTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C-unwind" fn direct_root_returns_unit(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null());
    assert_eq!(arg_count, 0);
    super::aura_direct_box_unit()
}

unsafe extern "C-unwind" fn direct_root_traps_with_owned_value(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null());
    assert_eq!(arg_count, 0);
    let owned = DIRECT_ROOT_TEST_OWNED_VALUE.load(Ordering::Acquire) as *mut OpaqueValue;
    assert!(!owned.is_null());
    unsafe {
        super::aura_direct_retain_value(owned);
    }
    super::aura_direct_fail_division_by_zero(0, 0)
}

unsafe extern "C-unwind" fn direct_root_cancels_with_owned_value(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null());
    assert_eq!(arg_count, 0);
    let owned = DIRECT_ROOT_TEST_OWNED_VALUE.load(Ordering::Acquire) as *mut OpaqueValue;
    assert!(!owned.is_null());
    unsafe {
        super::aura_direct_retain_value(owned);
    }
    super::task_runtime_boundary(|| std::panic::panic_any(TaskCancelledSignal));
    unreachable!("the cancellation boundary must force-exit the direct root")
}

unsafe extern "C-unwind" fn direct_task_must_not_start(
    _args: *const i64,
    _arg_count: usize,
) -> *mut OpaqueValue {
    panic!("queued direct task unexpectedly started before root completion")
}

unsafe extern "C-unwind" fn direct_task_waits_while_holding_arguments(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert_eq!(arg_count, 2);
    DIRECT_TEARDOWN_TASK_STARTED.store(true, Ordering::Release);
    let queue = unsafe { *args.add(1) } as *mut OpaqueValue;
    let received = super::aura_direct_channel_recv(queue);
    unsafe {
        release_value(received);
    }
    super::aura_direct_box_unit()
}

#[test]
fn native_runtime_direct_root_forced_exit_discards_state_once_but_normal_return_does_not_clean() {
    let normal_cleanup_count = Arc::new(AtomicUsize::new(0));
    let normal_cleanup_probe = normal_cleanup_count.clone();
    let normal = unsafe {
        super::run_direct_root_task_with_forced_exit_cleanup(direct_root_returns_unit, move || {
            normal_cleanup_probe.fetch_add(1, Ordering::SeqCst);
        })
    };
    assert_eq!(
        normal.expect("normal direct root should complete"),
        Value::Unit
    );
    assert_eq!(
        normal_cleanup_count.load(Ordering::SeqCst),
        0,
        "normal root return must unwind its scope and must not run forced cleanup"
    );

    let external = string_value("owned by a trapping direct root frame");
    DIRECT_ROOT_TEST_OWNED_VALUE.store(external as usize, Ordering::Release);
    let forced_cleanup_count = Arc::new(AtomicUsize::new(0));
    let forced_cleanup_probe = forced_cleanup_count.clone();
    let failed = unsafe {
        super::run_direct_root_task_with_forced_exit_cleanup(
            direct_root_traps_with_owned_value,
            move || {
                forced_cleanup_probe.fetch_add(1, Ordering::SeqCst);
                super::discard_current_direct_task_runtime_state();
            },
        )
    }
    .expect_err("trapping direct root should fail through the scheduler boundary");
    assert_eq!(failed.message, "division by zero");
    assert_eq!(
        forced_cleanup_count.load(Ordering::SeqCst),
        1,
        "a forced direct-root exit must run scheduler-owned cleanup exactly once"
    );
    assert_eq!(
        unsafe { &*external }.ref_count.load(Ordering::Acquire),
        1,
        "root forced cleanup must release the reference retained by the abandoned generated frame"
    );
    DIRECT_ROOT_TEST_OWNED_VALUE.store(0, Ordering::Release);
    unsafe {
        release_value(external);
    }

    let cancelled_external = string_value("owned by a cancelled direct root frame");
    DIRECT_ROOT_TEST_OWNED_VALUE.store(cancelled_external as usize, Ordering::Release);
    let cancellation_cleanup_count = Arc::new(AtomicUsize::new(0));
    let cancellation_cleanup_probe = cancellation_cleanup_count.clone();
    let cancelled = unsafe {
        super::run_direct_root_task_with_forced_exit_cleanup(
            direct_root_cancels_with_owned_value,
            move || {
                cancellation_cleanup_probe.fetch_add(1, Ordering::SeqCst);
                super::discard_current_direct_task_runtime_state();
            },
        )
    }
    .expect_err("cancelled direct root should exit through the scheduler boundary");
    assert_eq!(cancelled.message, "root Aura task was cancelled");
    assert_eq!(
        cancellation_cleanup_count.load(Ordering::SeqCst),
        1,
        "direct-root cancellation must run scheduler-owned cleanup exactly once"
    );
    assert_eq!(
        unsafe { &*cancelled_external }
            .ref_count
            .load(Ordering::Acquire),
        1,
        "root cancellation cleanup must release the abandoned frame's retained reference"
    );
    DIRECT_ROOT_TEST_OWNED_VALUE.store(0, Ordering::Release);
    unsafe {
        release_value(cancelled_external);
    }
}

#[test]
fn native_runtime_scheduler_teardown_releases_unstarted_direct_task_external_state() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("queued direct task argument");
    unsafe {
        super::retain_untracked_value(argument);
    }
    let args_address = Box::into_raw(Box::new(vec![argument as i64])) as usize;
    let claim_flag_address = super::allocate_direct_task_claim_flag();

    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(1, move || {
        let task = unsafe {
            super::spawn_direct_task_with_external_state(
                CancellationContext::default(),
                direct_task_must_not_start,
                args_address,
                claim_flag_address,
                true,
                None,
                |_| {},
            )?
        };
        drop(task);
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        unsafe { &*argument }.ref_count.load(Ordering::Acquire),
        1,
        "teardown must release a queued direct task's unclaimed argument-buffer reference"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "teardown must free a queued direct task's claim flag"
    );
    unsafe {
        release_value(argument);
    }
}

#[test]
fn native_runtime_scheduler_teardown_releases_started_direct_task_ledger_exactly_once() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    DIRECT_TEARDOWN_TASK_STARTED.store(false, Ordering::Release);
    let argument = string_value("started direct task argument");
    let queue = boxed_value(Value::Channel(ChannelValue::new()));
    unsafe {
        super::retain_untracked_value(argument);
        super::retain_untracked_value(queue);
    }
    let args_address = Box::into_raw(Box::new(vec![argument as i64, queue as i64])) as usize;
    let claim_flag_address = super::allocate_direct_task_claim_flag();

    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(1, move || {
        let task = unsafe {
            super::spawn_direct_task_with_external_state(
                CancellationContext::default(),
                direct_task_waits_while_holding_arguments,
                args_address,
                claim_flag_address,
                true,
                None,
                |_| {},
            )?
        };
        drop(task);
        crate::runtime_value::yield_now_with_runtime_scheduler();
        assert!(
            DIRECT_TEARDOWN_TASK_STARTED.load(Ordering::Acquire),
            "the child must claim its ledger before root completion triggers teardown"
        );
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        unsafe { &*argument }.ref_count.load(Ordering::Acquire),
        1,
        "teardown must release the claimed argument exactly once"
    );
    assert_eq!(
        unsafe { &*queue }.ref_count.load(Ordering::Acquire),
        1,
        "teardown must release the claimed queue exactly once"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "teardown must free a started direct task's claim flag"
    );
    unsafe {
        release_value(argument);
        release_value(queue);
    }
}

#[test]
fn native_runtime_normal_direct_task_completion_releases_external_state_once() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let argument = int_value(17);
    unsafe {
        super::retain_untracked_value(argument);
    }
    let args_address = Box::into_raw(Box::new(vec![argument as i64])) as usize;
    let claim_flag_address = super::allocate_direct_task_claim_flag();

    let result = run_lightweight_root_task(move || {
        let task = unsafe {
            super::spawn_direct_task_with_external_state(
                CancellationContext::default(),
                test_native_thunk,
                args_address,
                claim_flag_address,
                true,
                None,
                |_| {},
            )?
        };
        match task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Ok(Value::Int(value))) => {
                assert_eq!(value.as_i128(), Some(17));
            }
            other => {
                return Err(Diagnostic::new(format!(
                    "normal direct child did not return its result: {other:?}"
                )));
            }
        }
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        unsafe { &*argument }.ref_count.load(Ordering::Acquire),
        1,
        "normal completion must release the transferred argument exactly once"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "normal completion must free its external claim flag exactly once"
    );
    unsafe {
        release_value(argument);
    }
}

#[test]
fn native_runtime_direct_task_claim_flag_is_released_after_normal_completion() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let result = run_lightweight_root_task(move || {
        let args = super::aura_direct_arg_buffer_new(0);
        let group = super::aura_direct_task_group_new();
        let task = unsafe {
            super::aura_direct_start_task_call(
                direct_task_fresh_duration as *const () as usize as i64,
                args,
                0,
                1,
                group,
                1,
                1,
                crate::call::MIN_TASK_STACK_BYTES,
            )
        };
        let joined = super::aura_direct_task_join(task);
        match unsafe { take_value(joined) } {
            Value::EnumVariant(variant)
                if variant.enum_name == "TaskResult" && variant.variant_name == "Ready" =>
            {
                assert_eq!(
                    variant.single_payload(),
                    Some(&Value::Duration(
                        125 * crate::runtime_value::NANOS_PER_MILLISECOND
                    ))
                );
            }
            other => panic!("expected ready Duration task result, found {other:?}"),
        }
        unsafe {
            release_value(joined);
            release_value(task);
            release_value(group);
        }
        assert_eq!(
            super::direct_task_claim_flag_live_count(),
            baseline,
            "normal task completion must free its externally owned claim flag"
        );
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(super::direct_task_claim_flag_live_count(), baseline);
}

#[test]
fn native_runtime_direct_task_registers_captured_queue_before_submission() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let result = run_lightweight_root_task(move || {
        let queue = super::aura_direct_channel_new(std::ptr::null_mut());
        let args = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(args, 0, queue as i64);
        let group = super::aura_direct_task_group_new();
        let task = unsafe {
            super::aura_direct_start_task_call(
                direct_task_sends_to_captured_queue as *const () as usize as i64,
                args,
                1,
                1,
                group,
                1,
                0,
                0,
            )
        };

        assert_eq!(
            expect_queue_receive_item_int(
                super::aura_direct_channel_recv_with_registered_producers(queue)
            ),
            41,
            "registered-producer iteration must wait for the admitted direct task instead of reporting a premature close"
        );
        assert_eq!(
            expect_variant_ptr(super::aura_direct_task_join(task), "TaskResult", "Ready",),
            vec![Value::Unit]
        );
        assert!(expect_variant_ptr(
            super::aura_direct_channel_recv_with_registered_producers(queue),
            "QueueReceive",
            "Closed",
        )
        .is_empty());

        unsafe {
            release_value(task);
            release_value(group);
            release_value(queue);
        }
        Ok(Value::Unit)
    });

    assert_eq!(
        result.expect("captured Queue producer registration should complete"),
        Value::Unit
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "normal completion must release the direct task claim flag"
    );
}

#[test]
fn native_runtime_invalid_task_stack_releases_transferred_arguments() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("invalid-stack retained argument");
    let argument_address = argument as usize;
    let group = super::aura_direct_task_group_new();
    let group_address = group as usize;
    let result = run_lightweight_root_task(move || {
        let argument = argument_address as *mut OpaqueValue;
        let group = group_address as *mut OpaqueValue;
        let args = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(args, 0, argument as i64);
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| unsafe {
                super::aura_direct_start_task_call(
                    direct_task_fresh_duration as *const () as usize as i64,
                    args,
                    1,
                    1,
                    group,
                    1,
                    1,
                    crate::call::MIN_TASK_STACK_BYTES - 1,
                )
            })
        });
        Ok(Value::Unit)
    });
    let error = result.expect_err("an invalid explicit stack must trap");
    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "task stack size must be between 262144 and 67108864 bytes, found 262143"
    );
    assert_eq!(
        unsafe { &*argument }.ref_count.load(Ordering::Acquire),
        1,
        "rejecting the stack size must release the argument-buffer retain"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "rejecting the stack size must release its external-state claim flag"
    );
    unsafe {
        release_value(argument);
        release_value(group);
    }
}

fn rejected_direct_task_start(
    argument: *mut OpaqueValue,
    group: *mut OpaqueValue,
    stack_size_present: i64,
    stack_size: i64,
) -> Diagnostic {
    let argument_address = argument as usize;
    let group_address = group as usize;
    run_lightweight_root_task(move || {
        let args = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(args, 0, argument_address as *mut OpaqueValue as i64);
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| unsafe {
                super::aura_direct_start_task_call(
                    direct_task_fresh_duration as *const () as usize as i64,
                    args,
                    1,
                    1,
                    group_address as *mut OpaqueValue,
                    1,
                    stack_size_present,
                    stack_size,
                )
            })
        });
        Ok(Value::Unit)
    })
    .expect_err("the invalid direct task start should trap")
}

#[test]
fn native_runtime_task_start_validation_releases_owned_abi_state() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let cases = [
        (
            std::ptr::null_mut(),
            1,
            crate::call::MIN_TASK_STACK_BYTES,
            "task starting requires a `TaskGroup`".to_string(),
            None,
        ),
        (
            bool_value(true),
            1,
            crate::call::MIN_TASK_STACK_BYTES,
            "expected `TaskGroup`, found `bool`".to_string(),
            None,
        ),
        (
            super::aura_direct_task_group_new(),
            1,
            crate::call::MAX_TASK_STACK_BYTES + 1,
            format!(
                "task stack size must be between {} and {} bytes, found {}",
                crate::call::MIN_TASK_STACK_BYTES,
                crate::call::MAX_TASK_STACK_BYTES,
                crate::call::MAX_TASK_STACK_BYTES + 1
            ),
            Some("AU4005"),
        ),
        (
            super::aura_direct_task_group_new(),
            2,
            crate::call::MIN_TASK_STACK_BYTES,
            "invalid task-start stack-presence flag".to_string(),
            None,
        ),
    ];

    for (group, presence, size, expected_message, expected_code) in cases {
        let argument = string_value("rejected task-start argument");
        let error = rejected_direct_task_start(argument, group, presence, size);
        assert_eq!(error.message, expected_message);
        if let Some(expected_code) = expected_code {
            assert_eq!(error.code, expected_code);
        }
        assert_eq!(
            unsafe { &*argument }.ref_count.load(Ordering::Acquire),
            1,
            "every rejected ABI path must release the argument-buffer retain"
        );
        assert_eq!(
            super::direct_task_claim_flag_live_count(),
            baseline,
            "every rejected ABI path must release its external-state claim flag"
        );
        unsafe {
            release_value(argument);
            if !group.is_null() {
                release_value(group);
            }
        }
    }
}

#[test]
fn native_runtime_task_stack_allocation_failure_releases_transferred_arguments() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("allocation-failure retained argument");
    let argument_address = argument as usize;
    let group = super::aura_direct_task_group_new();
    let group_address = group as usize;
    let result = run_lightweight_root_task(move || {
        let argument = argument_address as *mut OpaqueValue;
        let group = group_address as *mut OpaqueValue;
        let args = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(args, 0, argument as i64);
        crate::runtime_value::fail_next_lightweight_task_stack_allocation();
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| unsafe {
                super::aura_direct_start_task_call(
                    direct_task_fresh_duration as *const () as usize as i64,
                    args,
                    1,
                    1,
                    group,
                    1,
                    1,
                    crate::call::MIN_TASK_STACK_BYTES,
                )
            })
        });
        Ok(Value::Unit)
    });
    let error = result.expect_err("injected stack allocation failure must trap");
    assert_eq!(error.code, "AU4005");
    assert_eq!(error.message, "injected Aura task stack allocation failure");
    assert_eq!(
        unsafe { &*argument }.ref_count.load(Ordering::Acquire),
        1,
        "allocation failure must release the argument-buffer retain"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "allocation failure must release its external-state claim flag"
    );
    unsafe {
        release_value(argument);
        release_value(group);
    }
}

#[cfg(unix)]
#[test]
fn lightweight_task_stack_allocation_rounds_up_and_includes_a_guard_page() {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    assert!(page_size.is_power_of_two());
    let requested = usize::try_from(crate::call::MIN_TASK_STACK_BYTES)
        .expect("minimum task stack size fits usize")
        + 1;
    let stack = crate::runtime_value::allocate_lightweight_task_stack(requested)
        .expect("minimum non-page-aligned task stack should allocate");
    let reservation = stack.base().get() - stack.limit().get();
    let expected_reservation = requested
        .checked_add(page_size + page_size - 1)
        .expect("test stack reservation should not overflow")
        & !(page_size - 1);
    assert_eq!(
        reservation, expected_reservation,
        "the reservation must page-round the requested usable bytes plus one guard page"
    );
    assert!(reservation >= requested + page_size);

    let maximum = usize::try_from(crate::call::MAX_TASK_STACK_BYTES)
        .expect("maximum task stack size fits usize");
    let maximum_stack = crate::runtime_value::allocate_lightweight_task_stack(maximum)
        .expect("maximum accepted task stack should allocate");
    assert!(maximum_stack.base().get() - maximum_stack.limit().get() >= maximum + page_size);
}

#[test]
fn native_runtime_direct_task_claim_flag_is_released_when_spawn_fails() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let external = string_value("spawn failure argument");
    unsafe {
        super::retain_untracked_value(external);
    }
    let args_address = Box::into_raw(Box::new(vec![external as i64])) as usize;
    let claim_flag_address = super::allocate_direct_task_claim_flag();

    let error = unsafe {
        super::spawn_direct_task_with_external_state(
            CancellationContext::default(),
            test_native_thunk,
            args_address,
            claim_flag_address,
            true,
            None,
            |_| {},
        )
    }
    .expect_err("starting outside a scheduler should fail");
    assert!(error.message.contains("requires an active task scheduler"));
    assert_eq!(
        unsafe { &*external }.ref_count.load(Ordering::Acquire),
        1,
        "spawn failure must release the raw argument-buffer owner"
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        baseline,
        "spawn failure must free its externally owned claim flag"
    );
    unsafe {
        release_value(external);
    }
}

#[test]
fn native_runtime_direct_task_claim_flag_survives_late_scope_unwind() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let result = run_lightweight_root_task(move || {
        let args = super::aura_direct_arg_buffer_new(0);
        let group = super::aura_direct_task_group_new();
        let task = unsafe {
            super::aura_direct_start_task_call(
                direct_task_violates_owned_ledger_invariant as *const () as usize as i64,
                args,
                0,
                1,
                group,
                1,
                0,
                0,
            )
        };
        let joined = super::aura_direct_task_join(task);
        let error = expect_task_result_error_message(joined);
        assert!(
            error.contains("normally completed direct task retained owned opaque values"),
            "late scope invariant should become a task error, found {error:?}"
        );
        unsafe {
            release_value(joined);
            release_value(task);
            release_value(group);
        }
        assert_eq!(
            super::direct_task_claim_flag_live_count(),
            baseline,
            "ordinary unwinding after the tracked scope must free external task state once"
        );
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(super::direct_task_claim_flag_live_count(), baseline);
}

#[test]
fn native_runtime_direct_task_external_state_survives_panic_before_result_handoff() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("panic-path argument");
    let argument_address = argument as usize;
    let result = run_lightweight_root_task(move || {
        let argument = argument_address as *mut OpaqueValue;
        let args = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(args, 0, argument as i64);
        let group = super::aura_direct_task_group_new();
        let task = unsafe {
            super::aura_direct_start_task_call(
                direct_task_panics_before_result_handoff as *const () as usize as i64,
                args,
                1,
                1,
                group,
                1,
                0,
                0,
            )
        };
        let joined = super::aura_direct_task_join(task);
        assert_eq!(
            expect_task_result_error_message(joined),
            "internal error: Aura task panicked: ordinary task panic before result handoff"
        );
        unsafe {
            release_value(joined);
            release_value(task);
            release_value(group);
        }
        assert_eq!(
            unsafe { &*argument }.ref_count.load(Ordering::Acquire),
            1,
            "ordinary panic must release the child task's raw argument owner"
        );
        assert_eq!(
            super::direct_task_claim_flag_live_count(),
            baseline,
            "ordinary panic must free the externally owned claim flag"
        );
        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(super::direct_task_claim_flag_live_count(), baseline);
    unsafe {
        release_value(argument);
    }
}

#[test]
fn direct_runtime_scalar_and_concurrency_helpers_cover_remaining_surface() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    assert_eq!(super::aura_direct_unbox_i64(int_value(17)), 17);
    assert_eq!(super::aura_direct_unbox_f64(float_value(2.5)), 2.5);
    assert_eq!(super::aura_direct_unbox_bool(bool_value(true)), 1);
    assert_eq!(super::aura_direct_value_as_condition(bool_value(true)), 1);
    assert_eq!(super::aura_direct_value_as_condition(int_value(0)), 0);
    assert_eq!(super::aura_direct_value_as_condition(int_value(2)), 1);
    assert_eq!(
        super::aura_direct_value_as_condition(super::aura_direct_box_unit()),
        0
    );
    assert_eq!(
        expect_int(super::aura_direct_unary_value(0, int_value(-7))),
        7
    );
    assert!(!expect_bool_boxed(super::aura_direct_unary_value(
        1,
        bool_value(true),
    )));
    assert_eq!(
        expect_bool_boxed(super::aura_direct_unary_value_at(
            1,
            bool_value(false),
            1,
            1
        )),
        true
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            0,
            int_value(4),
            int_value(5)
        )),
        9
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            1,
            int_value(9),
            int_value(4),
        )),
        5
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            2,
            int_value(6),
            int_value(7),
        )),
        42
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            3,
            int_value(9),
            int_value(2),
        )),
        4
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            4,
            int_value(9),
            int_value(4),
        )),
        1
    );
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        5,
        int_value(4),
        int_value(4),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        6,
        int_value(4),
        int_value(5),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        7,
        int_value(4),
        int_value(5),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        8,
        int_value(5),
        int_value(5),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        9,
        int_value(6),
        int_value(5),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        10,
        int_value(6),
        int_value(6),
    )));
    assert!(!expect_bool_boxed(super::aura_direct_binary_value(
        11,
        bool_value(true),
        bool_value(false),
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        12,
        bool_value(true),
        bool_value(false),
    )));
    assert_eq!(
        expect_string(super::aura_direct_binary_value_at(
            0,
            string_value("aura"),
            string_value(" repo"),
            0,
            1,
            1,
        )),
        "aura repo"
    );
    for (op, left, right, expected) in [
        (6, int_value(4), int_value(5), true),
        (7, int_value(4), int_value(5), true),
        (8, int_value(5), int_value(5), true),
        (9, int_value(6), int_value(5), true),
        (10, int_value(6), int_value(6), true),
        (11, bool_value(true), bool_value(false), false),
        (12, bool_value(false), bool_value(true), true),
    ] {
        assert_eq!(
            expect_bool_boxed(super::aura_direct_binary_value_at(op, left, right, 0, 2, 3)),
            expected
        );
    }
    assert_eq!(
        expect_float(super::aura_direct_cast_value(
            int_value(9),
            b"float64".as_ptr(),
            "float64".len(),
        )),
        9.0
    );
    assert_eq!(
        expect_int(super::aura_direct_cast_value_at(
            float_value(9.8),
            b"int32".as_ptr(),
            "int32".len(),
            1,
            1,
        )),
        9
    );
    assert_eq!(
        super::aura_direct_value_type_matches(string_value("aura"), b"str".as_ptr(), "str".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(bool_value(false), b"bool".as_ptr(), "bool".len()),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            super::aura_direct_vec_empty(),
            b"list".as_ptr(),
            "list".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            super::aura_direct_set_empty(),
            b"set".as_ptr(),
            "set".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            super::aura_direct_map_empty(),
            b"dict".as_ptr(),
            "dict".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            duration_value(5),
            b"Duration".as_ptr(),
            "Duration".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::Range(RangeValue { start: 1, end: 4 })),
            b"Range".as_ptr(),
            "Range".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::Channel(ChannelValue::new())),
            b"Queue".as_ptr(),
            "Queue".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::Channel(ChannelValue::new())),
            b"Queue".as_ptr(),
            "Queue".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(
                Value::Unit
            ))))),
            b"Task".as_ptr(),
            "Task".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::TaskGroup(TaskGroupValue::new(
                &CancellationContext::default()
            ))),
            b"TaskGroup".as_ptr(),
            "TaskGroup".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            boxed_value(Value::ModuleNamespace(ModuleNamespaceValue {
                path: "pkg.tools".to_string(),
            })),
            b"module pkg.tools".as_ptr(),
            "module pkg.tools".len(),
        ),
        1
    );

    let ready = super::aura_direct_enum_variant(
        b"Status".as_ptr(),
        "Status".len(),
        b"Ready".as_ptr(),
        "Ready".len(),
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(
        super::aura_direct_variant_matches(
            ready,
            b"Status".as_ptr(),
            "Status".len(),
            b"Ready".as_ptr(),
            "Ready".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_variant_matches(
            int_value(1),
            b"Status".as_ptr(),
            "Status".len(),
            b"Ready".as_ptr(),
            "Ready".len(),
        ),
        0
    );
    let payloads = super::aura_direct_arg_buffer_new(1);
    super::aura_direct_arg_buffer_store(payloads, 0, string_value("payload") as i64);
    let boxed_payload = super::aura_direct_enum_variant(
        b"Option".as_ptr(),
        "Option".len(),
        b"Some".as_ptr(),
        "Some".len(),
        payloads,
        1,
    );
    assert_eq!(
        expect_string(super::aura_direct_variant_payload(boxed_payload, 0)),
        "payload"
    );

    let field_names = [b"value".as_ptr()];
    let field_name_lengths = ["value".len()];
    let field_values = [int_value(11)];
    let instance = super::aura_direct_instance_new(
        b"Counter".as_ptr(),
        "Counter".len(),
        field_names.as_ptr(),
        field_name_lengths.as_ptr(),
        field_values.as_ptr(),
        1,
    );
    assert_eq!(
        expect_int(super::aura_direct_instance_get_field(
            instance,
            b"value".as_ptr(),
            "value".len(),
        )),
        11
    );
    let empty_instance = super::aura_direct_instance_empty(b"Counter".as_ptr(), "Counter".len());
    assert_eq!(
        expect_int(super::aura_direct_instance_get_field(
            super::aura_direct_instance_set_field(
                empty_instance,
                b"value".as_ptr(),
                "value".len(),
                int_value(13),
            ),
            b"value".as_ptr(),
            "value".len(),
        )),
        13
    );

    let buffer = super::aura_direct_arg_buffer_new(2);
    super::aura_direct_arg_buffer_store(buffer, 0, int_value(20) as i64);
    super::aura_direct_arg_buffer_store(buffer, 1, int_value(22) as i64);
    let buffer_address = buffer as usize;
    let started_sum = run_lightweight_root_task(move || {
        let buffer = buffer_address as *mut i64;
        let group = super::aura_direct_task_group_new();
        let task = unsafe {
            take_value(super::aura_direct_start_task_call(
                test_native_thunk as *const () as usize as i64,
                buffer,
                2,
                1,
                group,
                1,
                0,
                0,
            ))
        };
        let Value::Task(task) = task else {
            panic!("task start should return a task value");
        };
        Ok(unsafe { take_value(super::aura_direct_task_join(boxed_value(Value::Task(task)))) })
    })
    .expect("task start should run inside lightweight scheduler");
    assert_eq!(expect_task_result_ready_int(boxed_value(started_sum)), 42);

    let join_error = run_lightweight_root_task(move || {
        Ok(unsafe {
            take_value(super::aura_direct_task_join(boxed_value(Value::Task(
                TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("boom")))),
            ))))
        })
    })
    .expect("task join error should run inside lightweight scheduler");
    assert_eq!(
        expect_task_result_error_message(boxed_value(join_error)),
        "boom"
    );

    let channel = super::aura_direct_channel_new(std::ptr::null_mut());
    let send_ok = unsafe { take_value(super::aura_direct_channel_send(channel, int_value(9))) };
    match send_ok {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Ok" => {}
        other => panic!("expected Result.Ok(Unit), found {:?}", other),
    }
    assert_eq!(
        expect_queue_receive_item_int(super::aura_direct_channel_recv(channel)),
        9
    );
    expect_unit(super::aura_direct_channel_close(channel));
    match unsafe { take_value(super::aura_direct_channel_send(channel, int_value(7))) } {
        Value::EnumVariant(variant)
            if variant.enum_name == "Result" && variant.variant_name == "Err" => {}
        other => panic!(
            "expected Result.Err(SendError.Closed(...)), found {:?}",
            other
        ),
    }
    let closed_try_send =
        expect_result_err_payload(super::aura_direct_channel_try_send(channel, int_value(8)));
    assert_eq!(
        expect_variant_value(closed_try_send, "SendError", "Closed").len(),
        1
    );
    let closed_timeout_send = expect_result_err_payload(
        super::aura_direct_channel_send_timeout_value(channel, int_value(10), duration_value(0)),
    );
    assert_eq!(
        expect_variant_value(closed_timeout_send, "SendError", "Closed").len(),
        1
    );
    expect_queue_receive_closed(super::aura_direct_channel_recv(channel));
    expect_queue_receive_closed(super::aura_direct_channel_recv_timeout_value(
        channel,
        duration_value(0),
    ));

    let timeout_channel = super::aura_direct_channel_new(std::ptr::null_mut());
    expect_result_ok_unit(super::aura_direct_channel_try_send(
        timeout_channel,
        int_value(15),
    ));
    assert_eq!(
        expect_queue_receive_item_int(super::aura_direct_channel_recv_timeout_value(
            timeout_channel,
            duration_value(0),
        )),
        15
    );

    let bounded_channel = super::aura_direct_channel_new(int_value(1));
    expect_result_ok_unit(super::aura_direct_channel_send_timeout_value(
        bounded_channel,
        int_value(11),
        duration_value(0),
    ));
    let full_send = expect_result_err_payload(super::aura_direct_channel_try_send(
        bounded_channel,
        int_value(12),
    ));
    assert!(expect_variant_value(full_send, "SendError", "Full").len() == 1);
    assert_eq!(
        expect_queue_receive_item_int(super::aura_direct_channel_recv(bounded_channel)),
        11
    );
    expect_result_ok_unit(super::aura_direct_channel_try_send(
        bounded_channel,
        int_value(13),
    ));
    assert_eq!(
        expect_queue_receive_item_int(super::aura_direct_channel_recv(bounded_channel)),
        13
    );
    expect_unit(super::aura_direct_close_value(bounded_channel, 0));
    expect_unit(super::aura_direct_close_value(boxed_value(Value::Unit), 0));

    let group = super::aura_direct_task_group_new();
    expect_unit(super::aura_direct_task_group_cancel(group));
    assert_eq!(super::aura_direct_cancelled(), 0);
    expect_unit(super::aura_direct_task_group_close(group, 0));
    expect_unit(super::aura_direct_close_value(
        super::aura_direct_task_group_new(),
        1,
    ));
    let group = boxed_value(Value::TaskGroup(TaskGroupValue::new(
        &CancellationContext::default(),
    )));
    if let Value::TaskGroup(group_value) = unsafe { value_ref(group) } {
        group_value.register_task(TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit))));
    }
    expect_unit(super::aura_direct_task_group_close(group, 1));
    super::aura_direct_sleep_ms(0);
    expect_unit(super::aura_direct_sleep_value(duration_value(0)));
    super::aura_direct_sleep_value_void(duration_value(0));
    let first = super::aura_direct_monotonic_time_ms();
    let second = super::aura_direct_monotonic_time_ms();
    assert!(
        second >= first,
        "the direct monotonic clock must not move backwards"
    );
}

#[test]
fn direct_enum_owned_payload_buffer_moves_string_vec_and_map_allocations() {
    enum Allocation {
        String(*const u8),
        Vec(*const Value),
        Map(*const (Value, Value)),
    }

    let text = "owned enum payload".repeat(32);
    let text_ptr = text.as_ptr();
    let elements = vec![Value::Bool(true), Value::Bool(false)];
    let elements_ptr = elements.as_ptr();
    let entries = vec![(Value::String("key".to_string()), Value::Bool(true))];
    let entries_ptr = entries.as_ptr();
    let cases = [
        ("str", Value::String(text), Allocation::String(text_ptr)),
        (
            "Array",
            Value::Vec(VecValue {
                element_type: Type::named("json.Value"),
                elements,
            }),
            Allocation::Vec(elements_ptr),
        ),
        (
            "Object",
            Value::Map(MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("json.Value"),
                entries,
            }),
            Allocation::Map(entries_ptr),
        ),
    ];

    for (variant_name, payload, allocation) in cases {
        let payload = boxed_value(payload);
        let mut handles = vec![payload as i64];
        let payload_buffer = handles.as_mut_ptr();
        std::mem::forget(handles);

        let encoded = super::aura_direct_enum_variant(
            b"json.Value".as_ptr(),
            "json.Value".len(),
            variant_name.as_ptr(),
            variant_name.len(),
            payload_buffer,
            1,
        );
        unsafe {
            super::with_value(encoded, |value| {
                let Value::EnumVariant(variant) = value else {
                    panic!("expected json.Value.{variant_name}, found {value:?}");
                };
                match (&variant.payloads[..], &allocation) {
                    ([Value::String(value)], Allocation::String(expected)) => {
                        assert_eq!(value.as_ptr(), *expected)
                    }
                    ([Value::Vec(value)], Allocation::Vec(expected)) => {
                        assert_eq!(value.elements.as_ptr(), *expected)
                    }
                    ([Value::Map(value)], Allocation::Map(expected)) => {
                        assert_eq!(value.entries.as_ptr(), *expected)
                    }
                    (payloads, _) => {
                        panic!("unexpected json.Value.{variant_name} payloads: {payloads:?}")
                    }
                }
            });
            release_value(encoded);
        }
    }
}

#[test]
fn direct_instance_take_field_moves_nested_value_and_preserves_container() {
    let text = "nested direct move".repeat(32);
    let text_ptr = text.as_ptr();
    let holder = boxed_value(Value::Instance(InstanceValue {
        class_name: "Holder".to_string(),
        fields: BTreeMap::from([(
            "inner".to_string(),
            Value::Instance(InstanceValue {
                class_name: "Inner".to_string(),
                fields: BTreeMap::from([("value".to_string(), Value::String(text))]),
            }),
        )]),
    }));

    let moved = super::aura_direct_instance_take_field(
        holder,
        b"inner.value".as_ptr(),
        "inner.value".len(),
    );
    unsafe {
        super::with_value(moved, |value| match value {
            Value::String(value) => assert_eq!(value.as_ptr(), text_ptr),
            other => panic!("expected moved str, found {other:?}"),
        });
        super::with_value(holder, |value| match value {
            Value::Instance(holder) => match holder.fields.get("inner") {
                Some(Value::Instance(inner)) => {
                    assert!(!inner.fields.contains_key("value"));
                }
                other => panic!("expected nested Inner instance, found {other:?}"),
            },
            other => panic!("expected Holder instance, found {other:?}"),
        });
        release_value(moved);
        release_value(holder);
    }
}

#[test]
fn direct_projected_instance_helpers_report_paths_precisely_without_mutating_on_failure() {
    fn instance(class_name: &str, fields: BTreeMap<String, Value>) -> Value {
        Value::Instance(InstanceValue {
            class_name: class_name.to_string(),
            fields,
        })
    }

    let mut non_instance = Value::String("text".to_string());
    assert_eq!(
        super::take_direct_instance_field(&mut non_instance, &["value"], "value")
            .expect_err("moving a field from a non-instance must fail"),
        "cannot move field `value` from non-instance `str`"
    );
    assert_eq!(non_instance, Value::String("text".to_string()));

    let mut empty_move = instance("Holder", BTreeMap::new());
    assert_eq!(
        super::take_direct_instance_field(&mut empty_move, &[], "")
            .expect_err("an empty internal move path must fail"),
        "direct runtime received an empty instance field path"
    );

    let mut missing_leaf = instance(
        "Holder",
        BTreeMap::from([("sibling".to_string(), Value::Bool(true))]),
    );
    let missing_leaf_before = missing_leaf.clone();
    assert_eq!(
        super::take_direct_instance_field(&mut missing_leaf, &["missing"], "missing")
            .expect_err("a missing move leaf must fail"),
        "class `Holder` has no field `missing` in move path `missing`"
    );
    assert_eq!(
        missing_leaf, missing_leaf_before,
        "a failed leaf move must preserve every sibling"
    );

    let mut missing_nested = instance("Holder", BTreeMap::new());
    let missing_nested_before = missing_nested.clone();
    assert_eq!(
        super::take_direct_instance_field(&mut missing_nested, &["inner", "value"], "inner.value",)
            .expect_err("a missing intermediate field must fail"),
        "class `Holder` has no field `inner` in move path `inner.value`"
    );
    assert_eq!(missing_nested, missing_nested_before);

    let mut non_instance_nested = instance(
        "Holder",
        BTreeMap::from([("inner".to_string(), Value::String("text".to_string()))]),
    );
    let non_instance_nested_before = non_instance_nested.clone();
    assert_eq!(
        super::take_direct_instance_field(
            &mut non_instance_nested,
            &["inner", "value"],
            "inner.value",
        )
        .expect_err("a non-instance intermediate value must fail"),
        "cannot move field `inner.value` from non-instance `str`"
    );
    assert_eq!(non_instance_nested, non_instance_nested_before);

    let mut non_instance_assignment = Value::Bool(false);
    assert_eq!(
        super::set_direct_instance_field_owned(
            &mut non_instance_assignment,
            &["value"],
            "value",
            Value::String("new".to_string()),
        )
        .expect_err("owned assignment on a non-instance must fail"),
        "cannot assign field `value` on non-instance `bool`"
    );
    assert_eq!(non_instance_assignment, Value::Bool(false));

    let mut empty_assignment = instance("Holder", BTreeMap::new());
    assert_eq!(
        super::set_direct_instance_field_owned(
            &mut empty_assignment,
            &[],
            "",
            Value::String("new".to_string()),
        )
        .expect_err("an empty internal assignment path must fail"),
        "direct runtime received an empty instance assignment path"
    );

    let mut missing_assignment = instance(
        "Holder",
        BTreeMap::from([("sibling".to_string(), Value::Bool(true))]),
    );
    let missing_assignment_before = missing_assignment.clone();
    assert_eq!(
        super::set_direct_instance_field_owned(
            &mut missing_assignment,
            &["inner", "value"],
            "inner.value",
            Value::String("new".to_string()),
        )
        .expect_err("a missing assignment intermediate must fail"),
        "class `Holder` has no field `inner` in assignment path `inner.value`"
    );
    assert_eq!(
        missing_assignment, missing_assignment_before,
        "a failed owned assignment must preserve the target"
    );

    let mut nested_assignment = instance(
        "Holder",
        BTreeMap::from([(
            "inner".to_string(),
            instance(
                "Inner",
                BTreeMap::from([("sibling".to_string(), Value::Bool(true))]),
            ),
        )]),
    );
    let assigned = "nested owned assignment".repeat(16);
    let assigned_storage = assigned.as_ptr();
    super::set_direct_instance_field_owned(
        &mut nested_assignment,
        &["inner", "value"],
        "inner.value",
        Value::String(assigned),
    )
    .expect("an existing instance path should accept a new leaf");
    let Value::Instance(holder) = &nested_assignment else {
        panic!("expected Holder instance");
    };
    let Some(Value::Instance(inner)) = holder.fields.get("inner") else {
        panic!("expected Inner instance");
    };
    assert_eq!(inner.fields.get("sibling"), Some(&Value::Bool(true)));
    match inner.fields.get("value") {
        Some(Value::String(value)) => assert_eq!(value.as_ptr(), assigned_storage),
        other => panic!("expected nested owned str, found {other:?}"),
    }
}

#[test]
fn direct_projected_instance_wrappers_preserve_targets_and_obey_owned_consumption() {
    fn instance(class_name: &str, fields: BTreeMap<String, Value>) -> Value {
        Value::Instance(InstanceValue {
            class_name: class_name.to_string(),
            fields,
        })
    }
    fn assert_au4001(diagnostic: Diagnostic, message: &str) {
        assert_eq!(diagnostic.code, "AU4001");
        assert_eq!(diagnostic.message, message);
    }
    fn capture_wrapper_failure(work: impl FnOnce() + Send + 'static) -> Diagnostic {
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                work();
                Ok(Value::Unit)
            })
        })
        .expect_err("the direct wrapper should fail the active task")
    }

    for (target, path, expected) in [
        (
            Value::String("text".to_string()),
            "value",
            "cannot move field `value` from non-instance `str`",
        ),
        (
            instance("Holder", BTreeMap::new()),
            "missing",
            "class `Holder` has no field `missing` in move path `missing`",
        ),
        (
            instance("Holder", BTreeMap::new()),
            "inner.value",
            "class `Holder` has no field `inner` in move path `inner.value`",
        ),
        (
            instance(
                "Holder",
                BTreeMap::from([("inner".to_string(), Value::Bool(false))]),
            ),
            "inner.value",
            "cannot move field `inner.value` from non-instance `bool`",
        ),
        (
            instance(
                "Holder",
                BTreeMap::from([(
                    "inner".to_string(),
                    instance(
                        "Inner",
                        BTreeMap::from([("value".to_string(), Value::String("kept".to_string()))]),
                    ),
                )]),
            ),
            "inner..value",
            "invalid instance move path `inner..value`",
        ),
    ] {
        let before = target.clone();
        let target = boxed_value(target);
        let target_address = target as usize;
        let owned_path = path.to_string();
        let diagnostic = capture_wrapper_failure(move || {
            let _ = super::aura_direct_instance_take_field(
                target_address as *mut OpaqueValue,
                owned_path.as_ptr(),
                owned_path.len(),
            );
        });
        assert_au4001(diagnostic, expected);
        unsafe {
            super::with_value(target, |value| {
                assert_eq!(
                    value, &before,
                    "failed projected move `{path}` must preserve its target"
                )
            });
            release_value(target);
        }
    }

    for (target, path, expected) in [
        (
            Value::String("text".to_string()),
            "value",
            "cannot assign field `value` on non-instance `str`",
        ),
        (
            instance("Holder", BTreeMap::new()),
            "inner.value",
            "class `Holder` has no field `inner` in assignment path `inner.value`",
        ),
        (
            instance(
                "Holder",
                BTreeMap::from([("inner".to_string(), Value::Bool(false))]),
            ),
            "inner.value",
            "cannot assign field `inner.value` on non-instance `bool`",
        ),
    ] {
        let before = target.clone();
        let target = boxed_value(target);
        let owned = boxed_value(Value::String("consumed".to_string()));
        unsafe {
            retain_value(owned);
        }
        let target_address = target as usize;
        let owned_address = owned as usize;
        let owned_path = path.to_string();
        let diagnostic = capture_wrapper_failure(move || {
            super::aura_direct_instance_set_field_owned(
                target_address as *mut OpaqueValue,
                owned_path.as_ptr(),
                owned_path.len(),
                owned_address as *mut OpaqueValue,
            );
        });
        assert_au4001(diagnostic, expected);
        unsafe {
            super::with_value(target, |value| {
                assert_eq!(
                    value, &before,
                    "failed projected assignment `{path}` must preserve its target"
                )
            });
            super::with_value(owned, |value| {
                assert_eq!(
                    value,
                    &Value::Unit,
                    "an owned assignment argument must be consumed after path validation"
                )
            });
            release_value(target);
            release_value(owned);
        }
    }

    let invalid_target = boxed_value(instance("Holder", BTreeMap::new()));
    let invalid_owned = boxed_value(Value::String("still-owned".to_string()));
    unsafe {
        retain_value(invalid_owned);
    }
    let invalid_path = "inner..value";
    let invalid_target_address = invalid_target as usize;
    let invalid_owned_address = invalid_owned as usize;
    let owned_invalid_path = invalid_path.to_string();
    let diagnostic = capture_wrapper_failure(move || {
        super::aura_direct_instance_set_field_owned(
            invalid_target_address as *mut OpaqueValue,
            owned_invalid_path.as_ptr(),
            owned_invalid_path.len(),
            invalid_owned_address as *mut OpaqueValue,
        );
    });
    assert_au4001(
        diagnostic,
        "invalid instance assignment path `inner..value`",
    );
    unsafe {
        super::with_value(invalid_target, |value| {
            assert_eq!(value, &instance("Holder", BTreeMap::new()))
        });
        super::with_value(invalid_owned, |value| {
            assert_eq!(
                value,
                &Value::String("still-owned".to_string()),
                "the wrapper must reject malformed paths before consuming the owned argument"
            )
        });
        release_value(invalid_target);
        release_value(invalid_owned);
        release_value(invalid_owned);
    }

    let plain_target = boxed_value(Value::Bool(false));
    let plain_new_value = boxed_value(Value::String("preserved".to_string()));
    let plain_path = "value";
    let plain_target_address = plain_target as usize;
    let plain_new_value_address = plain_new_value as usize;
    let owned_plain_path = plain_path.to_string();
    let diagnostic = capture_wrapper_failure(move || {
        let _ = super::aura_direct_instance_set_field(
            plain_target_address as *mut OpaqueValue,
            owned_plain_path.as_ptr(),
            owned_plain_path.len(),
            plain_new_value_address as *mut OpaqueValue,
        );
    });
    assert_au4001(
        diagnostic,
        "cannot assign field `value` on non-instance `bool`",
    );
    unsafe {
        super::with_value(plain_target, |value| assert_eq!(value, &Value::Bool(false)));
        super::with_value(plain_new_value, |value| {
            assert_eq!(value, &Value::String("preserved".to_string()))
        });
        release_value(plain_target);
        release_value(plain_new_value);
    }
}

#[test]
fn direct_instance_owned_field_set_preserves_payload_allocation_identity() {
    let text = "owned class field".repeat(32);
    let text_ptr = text.as_ptr();
    let instance = super::aura_direct_instance_empty(b"Holder".as_ptr(), "Holder".len());
    super::aura_direct_instance_set_field_owned(
        instance,
        b"value".as_ptr(),
        "value".len(),
        boxed_value(Value::String(text)),
    );

    unsafe {
        super::with_value(instance, |value| match value {
            Value::Instance(instance) => match instance.fields.get("value") {
                Some(Value::String(value)) => assert_eq!(value.as_ptr(), text_ptr),
                other => panic!("expected owned str field, found {other:?}"),
            },
            other => panic!("expected Holder instance, found {other:?}"),
        });
        release_value(instance);
    }
}

#[test]
fn direct_variant_take_payload_preserves_allocation_and_consumes_slot() {
    let text = "owned direct match".repeat(32);
    let text_ptr = text.as_ptr();
    let packet = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Packet".to_string(),
        variant_name: "Text".to_string(),
        payloads: vec![Value::String(text)],
    }));
    let moved = super::aura_direct_variant_take_payload(packet, 0);

    unsafe {
        super::with_value(moved, |value| match value {
            Value::String(value) => assert_eq!(value.as_ptr(), text_ptr),
            other => panic!("expected moved str, found {other:?}"),
        });
        super::with_value(packet, |value| match value {
            Value::EnumVariant(variant) => assert_eq!(variant.payloads, vec![Value::Unit]),
            other => panic!("expected Packet enum, found {other:?}"),
        });
        release_value(moved);
        release_value(packet);
    }
}

#[test]
fn direct_owned_collection_and_queue_adapters_preserve_allocation_identity() {
    fn string_storage(value: *mut OpaqueValue) -> *const u8 {
        unsafe {
            super::with_value(value, |value| match value {
                Value::String(value) => value.as_ptr(),
                other => panic!("expected str, found {other:?}"),
            })
        }
    }

    let vector = super::aura_direct_vec_empty();
    let vector_item = string_value(&"vector item".repeat(32));
    let vector_storage = string_storage(vector_item);
    expect_unit(super::aura_direct_vec_push_in_place(vector, vector_item));
    unsafe {
        super::with_value(vector, |value| match value {
            Value::Vec(vector) => match vector.elements.as_slice() {
                [Value::String(value)] => assert_eq!(value.as_ptr(), vector_storage),
                other => panic!("expected one str vector element, found {other:?}"),
            },
            other => panic!("expected Vec, found {other:?}"),
        });
    }
    let vector_taken = super::aura_direct_vec_remove_in_place(vector, 0);
    unsafe {
        super::with_value(vector_taken, |value| match value {
            Value::EnumVariant(option)
                if option.enum_name == "Option" && option.variant_name == "Some" =>
            {
                match option.payloads.as_slice() {
                    [Value::String(value)] => assert_eq!(value.as_ptr(), vector_storage),
                    other => panic!("expected taken vector str payload, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(str), found {other:?}"),
        });
    }

    let map = super::aura_direct_map_empty();
    let map_key = string_value(&"map key".repeat(32));
    let map_value = string_value(&"map value".repeat(32));
    let map_key_storage = string_storage(map_key);
    let map_value_storage = string_storage(map_value);
    expect_option_none(super::aura_direct_map_set_in_place(map, map_key, map_value));
    unsafe {
        super::with_value(map, |value| match value {
            Value::Map(map) => match map.entries.as_slice() {
                [(Value::String(key), Value::String(value))] => {
                    assert_eq!(key.as_ptr(), map_key_storage);
                    assert_eq!(value.as_ptr(), map_value_storage);
                }
                other => panic!("expected one str map entry, found {other:?}"),
            },
            other => panic!("expected Map, found {other:?}"),
        });
    }

    let set = super::aura_direct_set_empty();
    let set_item = string_value(&"set item".repeat(32));
    let set_storage = string_storage(set_item);
    assert_eq!(super::aura_direct_set_insert_in_place(set, set_item), 1);
    unsafe {
        super::with_value(set, |value| match value {
            Value::Set(set) => match set.elements.as_slice() {
                [Value::String(value)] => assert_eq!(value.as_ptr(), set_storage),
                other => panic!("expected one str set element, found {other:?}"),
            },
            other => panic!("expected Set, found {other:?}"),
        });
    }
    let taken = super::aura_direct_set_take_index_in_place(set, 0);
    unsafe {
        super::with_value(taken, |value| match value {
            Value::EnumVariant(option)
                if option.enum_name == "Option" && option.variant_name == "Some" =>
            {
                match option.payloads.as_slice() {
                    [Value::String(value)] => assert_eq!(value.as_ptr(), set_storage),
                    other => panic!("expected taken str payload, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(str), found {other:?}"),
        });
        super::with_value(set, |value| match value {
            Value::Set(set) => assert!(set.elements.is_empty()),
            other => panic!("expected Set, found {other:?}"),
        });
    }

    let queue = super::aura_direct_channel_new(std::ptr::null_mut());
    let queued = string_value(&"queued item".repeat(32));
    let queued_storage = string_storage(queued);
    expect_result_ok_unit(super::aura_direct_channel_try_send(queue, queued));
    let received = super::aura_direct_channel_recv_or_none(queue);
    unsafe {
        super::with_value(received, |value| match value {
            Value::EnumVariant(option)
                if option.enum_name == "Option" && option.variant_name == "Some" =>
            {
                match option.payloads.as_slice() {
                    [Value::String(value)] => assert_eq!(value.as_ptr(), queued_storage),
                    other => panic!("expected queued str payload, found {other:?}"),
                }
            }
            other => panic!("expected Option.Some(str), found {other:?}"),
        });
        for value in [vector, vector_taken, map, set, taken, queue, received] {
            release_value(value);
        }
    }
}

#[test]
fn direct_owned_index_and_fallback_adapters_preserve_allocation_identity() {
    fn string_storage(value: *mut OpaqueValue) -> *const u8 {
        unsafe {
            super::with_value(value, |value| match value {
                Value::String(value) => value.as_ptr(),
                other => panic!("expected str, found {other:?}"),
            })
        }
    }

    let vector = super::aura_direct_vec_empty();
    expect_unit(super::aura_direct_vec_push_in_place(
        vector,
        string_value("old"),
    ));
    let replacement = string_value(&"replacement".repeat(32));
    let replacement_storage = string_storage(replacement);
    expect_unit(super::aura_direct_vec_set_index_in_place(
        vector,
        0,
        replacement,
        0,
        0,
    ));
    unsafe {
        super::with_value(vector, |value| match value {
            Value::Vec(vector) => match vector.elements.as_slice() {
                [Value::String(value)] => assert_eq!(value.as_ptr(), replacement_storage),
                other => panic!("expected one replacement str, found {other:?}"),
            },
            other => panic!("expected Vec, found {other:?}"),
        });
    }

    let closed_queue = super::aura_direct_channel_new(std::ptr::null_mut());
    expect_unit(super::aura_direct_channel_close(closed_queue));
    let queue_default = string_value(&"queue default".repeat(32));
    let queue_default_storage = string_storage(queue_default);
    let queue_fallback = super::aura_direct_channel_recv_or_value(closed_queue, queue_default);
    assert_eq!(string_storage(queue_fallback), queue_default_storage);

    let pending_task = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(100));
        Ok(Value::Unit)
    }))));
    let task_default = string_value(&"task default".repeat(32));
    let task_default_storage = string_storage(task_default);
    let task_fallback = super::aura_direct_task_join_or_value(pending_task, task_default);
    assert_eq!(string_storage(task_fallback), task_default_storage);

    unsafe {
        for value in [
            vector,
            closed_queue,
            queue_fallback,
            pending_task,
            task_fallback,
        ] {
            release_value(value);
        }
    }
}

#[test]
fn direct_task_producer_discovery_and_abandoned_args_do_not_clone_values() {
    let queue = ChannelValue::new();
    let nested = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Envelope".to_string(),
        variant_name: "Ready".to_string(),
        payloads: vec![Value::Vec(VecValue {
            element_type: Type::named("Queue"),
            elements: vec![Value::Channel(queue)],
        })],
    }));
    let clone_count = super::direct_value_clone_count();
    unsafe {
        super::with_value(nested, |value| {
            let mut queues = Vec::new();
            crate::runtime_value::collect_queue_values(value, &mut queues);
            assert_eq!(queues.len(), 1);
        });
    }
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count,
        "producer discovery must traverse task arguments by borrow"
    );

    let retained = string_value(&"abandoned task argument".repeat(32));
    let retained_storage = unsafe {
        super::with_value(retained, |value| match value {
            Value::String(value) => value.as_ptr(),
            other => panic!("expected str, found {other:?}"),
        })
    };
    let abandoned = unsafe { retain_value(retained) };
    let args_address = Box::into_raw(Box::new(vec![abandoned as i64])) as usize;
    unsafe {
        super::release_abandoned_direct_task_args(args_address);
        super::with_value(retained, |value| match value {
            Value::String(value) => assert_eq!(value.as_ptr(), retained_storage),
            other => panic!("expected retained str, found {other:?}"),
        });
        release_value(retained);
        release_value(nested);
    }
}

#[test]
fn direct_supervisor_start_consumes_every_value_argument_without_value_clones() {
    let supervisor = boxed_value(Value::ProcessSupervisor(ProcessSupervisorValue::new()));
    let clone_count = super::direct_value_clone_count();
    let result = super::aura_direct_process_supervisor_start(
        supervisor,
        string_value("empty"),
        super::aura_direct_vec_empty(),
        boxed_value(Value::Unit),
        super::aura_direct_map_empty(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        super::aura_direct_process_null(),
        process_restart_never_value(),
        duration_value(0),
        int_value(-1),
        bool_value(false),
    );
    assert_eq!(
        super::direct_value_clone_count(),
        clone_count + 1,
        "only the borrowed Supervisor receiver may be cloned by the direct runtime wrapper"
    );
    assert!(
        expect_variant_value(expect_result_err_payload(result), "Error", "NoCommand").is_empty()
    );
    unsafe {
        release_value(result);
        release_value(supervisor);
    }
}

#[test]
fn direct_json_rejects_inexact_int_array_object_and_indent_metadata() {
    fn malformed_json_call(name: &'static str, values: Vec<*mut OpaqueValue>) -> String {
        let addresses = values
            .iter()
            .map(|value| *value as usize)
            .collect::<Vec<_>>();
        let message = run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                let values = addresses
                    .iter()
                    .map(|address| *address as *mut OpaqueValue)
                    .collect::<Vec<_>>();
                let _ = direct_host_builtin_call(name, &values);
                Ok(Value::Unit)
            })
        })
        .expect_err("malformed JSON metadata should fail the active task")
        .message;
        unsafe {
            for value in values {
                release_value(value);
            }
        }
        message
    }

    let int = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "json.Value".to_string(),
        variant_name: "Int".to_string(),
        payloads: vec![Value::Int(
            IntegerValue::from_typed_signed(7, IntegerKind::Int32).expect("7 fits int32"),
        )],
    }));
    assert!(malformed_json_call("json::as_int", vec![int])
        .contains("malformed runtime `json.Value.Int` payload"));

    let array = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "json.Value".to_string(),
        variant_name: "Array".to_string(),
        payloads: vec![Value::Vec(VecValue {
            element_type: Type::named("Unknown"),
            elements: vec![],
        })],
    }));
    assert!(malformed_json_call("json::into_array", vec![array])
        .contains("malformed runtime `json.Value.Array` payload"));

    let object = boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "json.Value".to_string(),
        variant_name: "Object".to_string(),
        payloads: vec![Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("Unknown"),
            entries: vec![],
        })],
    }));
    assert!(malformed_json_call("json::into_object", vec![object])
        .contains("malformed runtime `json.Value.Object` payload"));

    let source = direct_json_value(crate::json_codec::JsonValue::Null);
    let indent = boxed_value(crate::runtime_value::option_some(Value::Int(
        IntegerValue::from_typed_signed(2, IntegerKind::Int32).expect("2 fits int32"),
    )));
    assert!(malformed_json_call("json::dumps", vec![source, indent])
        .contains("expects `indent` to contain an `int64`"));
}

#[test]
fn native_runtime_direct_queue_and_task_fallback_wrappers_cover_option_default_paths() {
    let channel = super::aura_direct_channel_new(std::ptr::null_mut());
    expect_option_none(super::aura_direct_channel_recv_or_none(channel));
    assert_eq!(
        expect_int(super::aura_direct_channel_recv_or_value(
            channel,
            int_value(5)
        )),
        5
    );
    expect_variant_ptr(
        super::aura_direct_channel_recv_timeout_value(channel, duration_value(0)),
        "QueueReceive",
        "TimedOut",
    );
    expect_option_none(super::aura_direct_channel_recv_or_none_timeout_value(
        channel,
        duration_value(0),
    ));
    assert_eq!(
        expect_int(super::aura_direct_channel_recv_or_value_timeout_value(
            channel,
            int_value(6),
            duration_value(0),
        )),
        6
    );

    expect_result_ok_unit(super::aura_direct_channel_try_send(channel, int_value(21)));
    assert_eq!(
        expect_option_some_int(super::aura_direct_channel_recv_or_none(channel)),
        21
    );
    expect_result_ok_unit(super::aura_direct_channel_try_send(channel, int_value(22)));
    assert_eq!(
        expect_int(super::aura_direct_channel_recv_or_value(
            channel,
            int_value(7)
        )),
        22
    );
    expect_result_ok_unit(super::aura_direct_channel_try_send(channel, int_value(23)));
    assert_eq!(
        expect_option_some_int(super::aura_direct_channel_recv_or_none_timeout_value(
            channel,
            duration_value(0),
        )),
        23
    );
    expect_result_ok_unit(super::aura_direct_channel_try_send(channel, int_value(24)));
    assert_eq!(
        expect_int(super::aura_direct_channel_recv_or_value_timeout_value(
            channel,
            int_value(8),
            duration_value(0),
        )),
        24
    );
    expect_unit(super::aura_direct_channel_close(channel));
    expect_option_none(super::aura_direct_channel_recv_or_none(channel));
    assert_eq!(
        expect_int(super::aura_direct_channel_recv_or_value(
            channel,
            int_value(9)
        )),
        9
    );

    let bounded = super::aura_direct_channel_new(int_value(1));
    expect_result_ok_unit(super::aura_direct_channel_try_send(bounded, int_value(31)));
    let timed_out = expect_result_err_payload(super::aura_direct_channel_send_timeout_value(
        bounded,
        int_value(32),
        duration_value(0),
    ));
    assert_eq!(
        expect_variant_value(timed_out, "SendError", "TimedOut").len(),
        1
    );
    expect_unit(super::aura_direct_channel_close(bounded));

    let slow_task = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(200));
        Ok(Value::Int(IntegerValue::from_signed(77)))
    }))));
    expect_option_none(super::aura_direct_task_join_or_none(slow_task));
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value(
            slow_task,
            int_value(50)
        )),
        50
    );
    expect_option_none(super::aura_direct_task_join_or_none_timeout_value(
        slow_task,
        duration_value(0),
    ));
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value_timeout_value(
            slow_task,
            int_value(51),
            duration_value(0),
        )),
        51
    );
    assert_eq!(
        expect_task_result_ready_int(super::aura_direct_task_join(slow_task)),
        77
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_task_join_or_none(slow_task)),
        77
    );
    assert_eq!(
        expect_option_some_int(super::aura_direct_task_join_or_none_timeout_value(
            slow_task,
            duration_value(0),
        )),
        77
    );
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value(
            slow_task,
            int_value(52)
        )),
        77
    );
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value_timeout_value(
            slow_task,
            int_value(53),
            duration_value(0),
        )),
        77
    );

    let error_task = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
        Err(Diagnostic::new("task failed"))
    }))));
    assert_eq!(
        expect_task_result_error_message(super::aura_direct_task_join(error_task)),
        "task failed"
    );
    expect_option_none(super::aura_direct_task_join_or_none(error_task));
    expect_option_none(super::aura_direct_task_join_or_none_timeout_value(
        error_task,
        duration_value(0),
    ));
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value(
            error_task,
            int_value(54)
        )),
        54
    );
    assert_eq!(
        expect_int(super::aura_direct_task_join_or_value_timeout_value(
            error_task,
            int_value(55),
            duration_value(0),
        )),
        55
    );
}

#[test]
fn native_runtime_direct_concurrency_wrappers_cover_cancelled_paths() {
    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancellation = group.child_cancellation();
    group.cancel();

    let task = with_cancellation_scope(cancellation, || {
        assert_eq!(super::aura_direct_cancelled(), 1);

        let bounded = super::aura_direct_channel_new(int_value(1));
        expect_result_ok_unit(super::aura_direct_channel_try_send(bounded, int_value(1)));

        let cancelled =
            expect_result_err_payload(super::aura_direct_channel_send(bounded, int_value(2)));
        assert_eq!(
            expect_variant_value(cancelled, "SendError", "Cancelled").len(),
            1
        );
        let cancelled = expect_result_err_payload(super::aura_direct_channel_send_timeout_value(
            bounded,
            int_value(3),
            duration_value(1000),
        ));
        assert_eq!(
            expect_variant_value(cancelled, "SendError", "Cancelled").len(),
            1
        );

        let empty = super::aura_direct_channel_new(std::ptr::null_mut());
        expect_variant_ptr(
            super::aura_direct_channel_recv(empty),
            "QueueReceive",
            "Cancelled",
        );
        expect_variant_ptr(
            super::aura_direct_channel_recv_timeout_value(empty, duration_value(1000)),
            "QueueReceive",
            "Cancelled",
        );
        expect_option_none(super::aura_direct_channel_recv_or_none(empty));
        expect_option_none(super::aura_direct_channel_recv_or_none_timeout_value(
            empty,
            duration_value(1000),
        ));
        assert_eq!(
            expect_int(super::aura_direct_channel_recv_or_value(
                empty,
                int_value(4)
            )),
            4
        );
        assert_eq!(
            expect_int(super::aura_direct_channel_recv_or_value_timeout_value(
                empty,
                int_value(5),
                duration_value(1000),
            )),
            5
        );

        let task_value = TaskValue::from_handle(thread::spawn(|| {
            thread::sleep(StdDuration::from_millis(50));
            Ok(Value::Int(IntegerValue::from_signed(6)))
        }));
        let task = boxed_value(Value::Task(task_value.clone()));
        expect_variant_ptr(
            super::aura_direct_task_join(task),
            "TaskResult",
            "Cancelled",
        );
        expect_variant_ptr(
            super::aura_direct_task_join_timeout_value(task, duration_value(1000)),
            "TaskResult",
            "Cancelled",
        );
        expect_option_none(super::aura_direct_task_join_or_none(task));
        expect_option_none(super::aura_direct_task_join_or_none_timeout_value(
            task,
            duration_value(1000),
        ));
        assert_eq!(
            expect_int(super::aura_direct_task_join_or_value(task, int_value(7))),
            7
        );
        assert_eq!(
            expect_int(super::aura_direct_task_join_or_value_timeout_value(
                task,
                int_value(8),
                duration_value(1000),
            )),
            8
        );

        expect_variant_ptr(
            super::aura_direct_wait_any(task_vec(&[task_value.clone()])),
            "WaitAny",
            "Cancelled",
        );
        expect_variant_ptr(
            super::aura_direct_wait_all(task_vec(&[task_value.clone()])),
            "WaitAll",
            "Cancelled",
        );

        task
    });
    assert_eq!(
        expect_task_result_ready_int(super::aura_direct_task_join(task)),
        6
    );
}

#[test]
fn division_by_zero_helper_exits_with_error() {
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() == Ok("divzero") {
        super::aura_direct_runtime_init(
            b"/virtual/test.au".as_ptr(),
            b"/virtual/test.au".len(),
            b"def main() -> int32:\n    print(1 // 0)\n".as_ptr(),
            b"def main() -> int32:\n    print(1 // 0)\n".len(),
        );
        super::aura_direct_fail_division_by_zero(2, 11);
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg("native_runtime::tests::division_by_zero_helper_exits_with_error")
        .arg("--nocapture")
        .env("AURA_DIRECT_RUNTIME_HELPER", "divzero")
        .output()
        .expect("child test process should run");

    assert!(
        !output.status.success(),
        "division helper should exit with failure"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("division by zero"),
        "division helper stderr should mention division by zero"
    );
}

#[test]
fn io_read_line_reports_end_of_input_consistently_through_both_runtime_surfaces() {
    const HELPER_ENV: &str = "AURA_DIRECT_RUNTIME_READ_LINE_EOF_HELPER";
    if std::env::var(HELPER_ENV).as_deref() == Ok("1") {
        assert_eq!(
            crate::runtime_value::io_read_line().expect("reading closed stdin should succeed"),
            None,
            "the shared runtime helper must represent EOF as absence, not an empty line"
        );

        let payload = expect_result_ok_payload(super::aura_direct_io_read_line());
        assert!(
            expect_variant_value(payload, "Option", "None").is_empty(),
            "the direct runtime must expose EOF as Result.Ok(Option.None)"
        );
        return;
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg(
            "native_runtime::tests::io_read_line_reports_end_of_input_consistently_through_both_runtime_surfaces",
        )
        .arg("--nocapture")
        .env(HELPER_ENV, "1")
        .stdin(Stdio::null())
        .output()
        .expect("EOF helper process should run");

    assert!(
        output.status.success(),
        "EOF behavior should agree across runtime surfaces\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn metrics_state_and_diagnostics_are_observable_from_each_fresh_entry_path() {
    const HELPER_ENV: &str = "AURA_RUNTIME_METRICS_BEHAVIOR_HELPER";
    if let Ok(helper) = std::env::var(HELPER_ENV) {
        let call = |name, args| crate::runtime_value::evaluate_host_builtin(name, args);
        let metric = |name: &str| Value::String(name.to_string());
        let int = |value| Value::Int(IntegerValue::from_signed(value));

        match helper.as_str() {
            "get-first" => {
                assert_eq!(
                    call("metrics::get", vec![metric("never-created")])
                        .expect("reading a missing metric should succeed"),
                    int(0),
                    "missing metrics must read as zero"
                );
            }
            "increment-first" => {
                assert_eq!(
                    call("metrics::increment", vec![metric("requests"), int(0)])
                        .expect("a zero increment should create a stable zero metric"),
                    Value::Unit
                );
                assert_eq!(
                    call("metrics::get", vec![metric("requests")])
                        .expect("the incremented metric should be readable"),
                    int(0),
                    "zero increments must remain observable as zero"
                );
                assert_eq!(
                    call("metrics::increment", vec![metric("requests"), int(2)])
                        .expect("a later increment should update the metric"),
                    Value::Unit
                );
                assert_eq!(
                    call("metrics::get", vec![metric("requests")])
                        .expect("the updated metric should be readable"),
                    int(2)
                );
            }
            "reset-first" => {
                assert_eq!(
                    call("metrics::reset", vec![]).expect("reset should succeed"),
                    Value::Unit
                );
                assert_eq!(
                    call("metrics::get", vec![metric("missing")])
                        .expect("a missing metric should be readable"),
                    int(0)
                );

                let wrong_type = call("metrics::increment", vec![metric("requests"), Value::Unit])
                    .expect_err("metric increments require int64 values");
                assert_eq!(
                    wrong_type.message,
                    "`metrics.increment` expects `int64` for `value`"
                );

                let outside_int64 = call(
                    "metrics::increment",
                    vec![metric("requests"), int(i128::from(i64::MAX) + 1)],
                )
                .expect_err("metric increments outside int64 must be rejected");
                assert_eq!(
                    outside_int64.message,
                    "metric increment does not fit in `int64`"
                );

                assert_eq!(
                    call(
                        "metrics::increment",
                        vec![metric("requests"), int(i128::from(i64::MAX))]
                    )
                    .expect("the int64 maximum should remain a valid metric value"),
                    Value::Unit
                );
                let overflow = call("metrics::increment", vec![metric("requests"), int(1)])
                    .expect_err("metric addition overflow must be diagnosed");
                assert_eq!(overflow.message, "metric value overflowed `int64`");
                assert_eq!(
                    call("metrics::get", vec![metric("requests")])
                        .expect("a failed increment must preserve the old value"),
                    int(i128::from(i64::MAX))
                );

                assert_eq!(
                    call("metrics::reset", vec![]).expect("reset should clear all metrics"),
                    Value::Unit
                );
                assert_eq!(
                    call("metrics::get", vec![metric("requests")])
                        .expect("a reset metric should read as missing"),
                    int(0),
                    "reset must make an existing metric observable as zero"
                );
            }
            other => panic!("unknown metrics helper `{other}`"),
        }
        return;
    }

    for helper in ["get-first", "increment-first", "reset-first"] {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg(
                "native_runtime::tests::metrics_state_and_diagnostics_are_observable_from_each_fresh_entry_path",
            )
            .arg("--nocapture")
            .env(HELPER_ENV, helper)
            .output()
            .expect("metrics helper process should run");

        assert!(
            output.status.success(),
            "metrics behavior helper `{helper}` failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn int32_overflow_helper_exits_with_error() {
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() == Ok("overflow") {
        super::aura_direct_runtime_init(
            b"/virtual/test.au".as_ptr(),
            b"/virtual/test.au".len(),
            b"def main() -> int32:\n    value: int32 = 999\n".as_ptr(),
            b"def main() -> int32:\n    value: int32 = 999\n".len(),
        );
        super::aura_direct_fail_int32_overflow(999, 2, 20);
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg("native_runtime::tests::int32_overflow_helper_exits_with_error")
        .arg("--nocapture")
        .env("AURA_DIRECT_RUNTIME_HELPER", "overflow")
        .output()
        .expect("child test process should run");

    assert!(
        !output.status.success(),
        "overflow helper should exit with failure"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("integer value `999` does not fit in `int32`"),
        "overflow helper stderr should mention the failing int32 value"
    );
}

#[test]
fn wide_integer_overflow_and_cast_helpers_report_precise_diagnostics() {
    const HELPER_ENV: &str = "AURA_DIRECT_RUNTIME_WIDE_INTEGER_ERROR_HELPER";
    if let Ok(helper) = std::env::var(HELPER_ENV) {
        match helper.as_str() {
            "signed-overflow-with-span" => {
                let source = b"def main() -> int32:\n    value = 9223372036854775807 + 1\n";
                super::aura_direct_runtime_init(
                    b"/virtual/wide.au".as_ptr(),
                    b"/virtual/wide.au".len(),
                    source.as_ptr(),
                    source.len(),
                );
                super::aura_direct_fail_integer_overflow(0, 0, i64::MAX as u64, 1, 2, 13);
            }
            "unsigned-underflow-without-span" => {
                super::aura_direct_fail_integer_overflow(1, 1, 0, 1, 0, 0);
            }
            "integer-cast-with-span" => {
                let source = b"def main() -> int32:\n    value = high as int64\n";
                super::aura_direct_runtime_init(
                    b"/virtual/cast.au".as_ptr(),
                    b"/virtual/cast.au".len(),
                    source.as_ptr(),
                    source.len(),
                );
                super::aura_direct_cast_integer_to_integer(u64::MAX, 1, 1, 2, 13);
            }
            "float-cast-without-span" => {
                super::aura_direct_cast_float_to_integer(4_294_967_296.75, 0, 0, 0);
            }
            other => panic!("unknown wide-integer error helper `{other}`"),
        }
    }

    for (helper, expected_message, expected_location) in [
        (
            "signed-overflow-with-span",
            "integer value `9223372036854775808` does not fit in `int64`",
            Some(" --> /virtual/wide.au:2:13"),
        ),
        (
            "unsigned-underflow-without-span",
            "integer value `-1` does not fit in `uint64`",
            None,
        ),
        (
            "integer-cast-with-span",
            "integer value `18446744073709551615` does not fit in `int64`",
            Some(" --> /virtual/cast.au:2:13"),
        ),
        (
            "float-cast-without-span",
            "integer value `4294967296` does not fit in `int32`",
            None,
        ),
    ] {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg(
                "native_runtime::tests::wide_integer_overflow_and_cast_helpers_report_precise_diagnostics",
            )
            .arg("--nocapture")
            .env(HELPER_ENV, helper)
            .output()
            .expect("child test process should run");

        assert!(
            !output.status.success(),
            "{helper} should exit with a diagnostic"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(&format!("error[AU4002]: {expected_message}\n")),
            "unexpected diagnostic for {helper}:\n{stderr}"
        );
        match expected_location {
            Some(location) => assert!(
                stderr.contains(location),
                "diagnostic for {helper} should include `{location}`:\n{stderr}"
            ),
            None => assert_eq!(
                stderr,
                format!("error[AU4002]: {expected_message}\n"),
                "spanless diagnostic for {helper} should not invent a source location"
            ),
        }
    }
}

#[test]
fn direct_root_entrypoint_helper_exits_for_invalid_thunks_and_return_types() {
    if let Ok(helper) = std::env::var("AURA_DIRECT_RUNTIME_HELPER") {
        match helper.as_str() {
            "direct-root-null" => unsafe {
                super::aura_direct_run_root(0);
            },
            "direct-root-string" => {
                unsafe extern "C-unwind" fn returns_string(
                    _args: *const i64,
                    _arg_count: usize,
                ) -> *mut OpaqueValue {
                    super::aura_direct_string_literal(b"not-int32".as_ptr(), b"not-int32".len())
                }
                unsafe {
                    super::aura_direct_run_root(returns_string as *const () as usize as i64);
                }
            }
            "direct-call-depth" => unsafe {
                for _ in 0..=super::DIRECT_MAX_CALL_DEPTH {
                    super::aura_direct_enter_call(2, 3, b"recurse".as_ptr(), b"recurse".len());
                }
            },
            _ => {}
        }
    }

    for (helper, expected) in [
        ("direct-root-null", "invalid direct root thunk pointer"),
        (
            "direct-root-string",
            "direct main entry must return `int32` or `None`, found `str`",
        ),
        (
            "direct-call-depth",
            "maximum call depth of 256 exceeded while calling `recurse`",
        ),
    ] {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg(
                "native_runtime::tests::direct_root_entrypoint_helper_exits_for_invalid_thunks_and_return_types",
            )
            .arg("--nocapture")
            .env("AURA_DIRECT_RUNTIME_HELPER", helper)
            .output()
            .expect("child test process should run");

        assert!(
            !output.status.success(),
            "direct root helper should exit with failure for {helper}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "direct root helper stderr should mention {expected}"
        );
    }
}

#[test]
fn native_runtime_entrypoint_guards_invalid_inputs() {
    assert_eq!(
        unsafe {
            crate::mir_runtime::aura_native_run(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        },
        1
    );

    let mir_json = b"{}";
    let invalid_path = [0xff_u8];
    let source = b"def main() -> int32:\n    return 0\n";
    assert_eq!(
        unsafe {
            crate::mir_runtime::aura_native_run(
                mir_json.as_ptr(),
                mir_json.len(),
                invalid_path.as_ptr(),
                invalid_path.len(),
                source.as_ptr(),
                source.len(),
            )
        },
        1
    );

    let source_path = b"/tmp/test.au";
    let invalid_source = [0xff_u8];
    assert_eq!(
        unsafe {
            crate::mir_runtime::aura_native_run(
                mir_json.as_ptr(),
                mir_json.len(),
                source_path.as_ptr(),
                source_path.len(),
                invalid_source.as_ptr(),
                invalid_source.len(),
            )
        },
        1
    );

    assert_eq!(
        render_runtime_diagnostic(crate::diag::Diagnostic::new("oops")),
        "error[AU2999]: oops"
    );
}

#[test]
fn native_runtime_private_value_decoders_cover_success_paths() {
    assert_eq!(
        super::expect_string_value(&Value::String("aura".to_string()), "text"),
        "aura"
    );
    assert_eq!(
        super::expect_bytes_value(
            &Value::Vec(VecValue {
                element_type: crate::sema::Type::named("Unknown"),
                elements: vec![
                    Value::Int(IntegerValue::from_signed(65)),
                    Value::Int(IntegerValue::from_signed(66)),
                ],
            }),
            "bytes",
        ),
        vec![65, 66]
    );
    assert!(super::expect_bool_value(&Value::Bool(true), "flag"));
    assert_eq!(
        super::expect_i32_value(&Value::Int(IntegerValue::from_signed(123)), "count"),
        123
    );
    assert_eq!(
        super::expect_headers_map(
            &Value::Map(MapValue {
                key_type: crate::sema::Type::named("Unknown"),
                value_type: crate::sema::Type::named("Unknown"),
                entries: vec![(
                    Value::String("content-type".to_string()),
                    Value::String("text/plain".to_string()),
                )],
            }),
            "headers",
        ),
        vec![("content-type".to_string(), "text/plain".to_string())]
    );
    assert_eq!(
        super::optional_timeout_from_ptr(std::ptr::null_mut(), "timeout"),
        None
    );
    let timeout = duration_value(12);
    assert_eq!(
        super::optional_timeout_from_ptr(timeout, "timeout"),
        Some(StdDuration::from_millis(12))
    );
    unsafe {
        release_value(timeout);
    }
    let unit_timeout = boxed_value(Value::Unit);
    assert_eq!(
        super::process_optional_timeout_from_ptr(unit_timeout, "timeout"),
        None
    );
    unsafe {
        release_value(unit_timeout);
    }
    let negative_timeout = duration_value(-1);
    assert_eq!(
        super::process_optional_timeout_result_from_ptr(negative_timeout, "timeout")
            .expect_err("negative process timeout should be rejected")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    unsafe {
        release_value(negative_timeout);
    }
    let process_timeout = duration_value(34);
    assert_eq!(
        super::process_optional_timeout_from_ptr(process_timeout, "timeout"),
        Some(StdDuration::from_millis(34))
    );
    unsafe {
        release_value(process_timeout);
    }
    let duration = duration_value(56);
    assert_eq!(
        super::duration_from_ptr(duration, "duration"),
        StdDuration::from_millis(56)
    );
    unsafe {
        release_value(duration);
    }
    let unlimited_restarts = int_value(-1);
    assert_eq!(
        super::supervisor_max_restarts_from_ptr(unlimited_restarts, "max_restarts"),
        None
    );
    unsafe {
        release_value(unlimited_restarts);
    }
    let limited_restarts = int_value(3);
    assert_eq!(
        super::supervisor_max_restarts_from_ptr(limited_restarts, "max_restarts"),
        Some(3)
    );
    unsafe {
        release_value(limited_restarts);
    }
    assert_eq!(
        super::expect_command_vec(
            &Value::Vec(VecValue {
                element_type: crate::sema::Type::named("Unknown"),
                elements: vec![
                    Value::String("/bin/echo".to_string()),
                    Value::String("ok".to_string()),
                ],
            }),
            "command",
        ),
        vec!["/bin/echo".to_string(), "ok".to_string()]
    );
    assert_eq!(
        super::expect_optional_string_value(&Value::Unit, "cwd"),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(
            &Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "None".to_string(),
                payloads: Vec::new(),
            }),
            "cwd",
        ),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(
            &Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: vec![Value::String("/tmp".to_string())],
            }),
            "cwd",
        ),
        Some("/tmp".to_string())
    );
}

#[test]
fn direct_runtime_helper_errors_surface_expected_diagnostics() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    if let Ok(case) = std::env::var("AURA_DIRECT_RUNTIME_CASE") {
        match case.as_str() {
            "bytes-value-type" => {
                super::expect_bytes_value(&Value::String("bytes".to_string()), "bytes");
            }
            "bytes-element-range" => {
                super::expect_bytes_value(
                    &Value::Vec(VecValue {
                        element_type: crate::sema::Type::named("uint8"),
                        elements: vec![Value::Int(IntegerValue::from_signed(300))],
                    }),
                    "bytes",
                );
            }
            "bool-value-type" => {
                super::expect_bool_value(&Value::String("flag".to_string()), "flag");
            }
            "i32-overflow" => {
                super::expect_i32_value(
                    &Value::Int(IntegerValue::from_signed(i128::from(i32::MAX) + 1)),
                    "count",
                );
            }
            "i32-value-type" => {
                super::expect_i32_value(&Value::String("count".to_string()), "count");
            }
            "headers-map-type" => {
                super::expect_headers_map(&Value::String("headers".to_string()), "headers");
            }
            "headers-key-type" => {
                super::expect_headers_map(
                    &Value::Map(MapValue {
                        key_type: crate::sema::Type::named("Unknown"),
                        value_type: crate::sema::Type::named("Unknown"),
                        entries: vec![(
                            Value::Int(IntegerValue::from_signed(1)),
                            Value::String("value".to_string()),
                        )],
                    }),
                    "headers",
                );
            }
            "optional-timeout-type" => {
                super::optional_timeout_from_ptr(string_value("slow"), "timeout");
            }
            "optional-timeout-negative" => {
                super::optional_timeout_from_ptr(duration_value(-1), "timeout");
            }
            "process-timeout-type" => {
                super::process_optional_timeout_from_ptr(string_value("slow"), "timeout");
            }
            "duration-type" => {
                super::duration_from_ptr(string_value("slow"), "duration");
            }
            "duration-negative" => {
                super::duration_from_ptr(duration_value(-1), "duration");
            }
            "supervisor-max-too-low" => {
                super::supervisor_max_restarts_from_ptr(int_value(-2), "max_restarts");
            }
            "command-vec-type" => {
                super::expect_command_vec(&Value::String("command".to_string()), "command");
            }
            "command-element-type" => {
                super::expect_command_vec(
                    &Value::Vec(VecValue {
                        element_type: crate::sema::Type::named("str"),
                        elements: vec![Value::Int(IntegerValue::from_signed(1))],
                    }),
                    "command",
                );
            }
            "optional-string-malformed" => {
                super::expect_optional_string_value(
                    &Value::EnumVariant(EnumVariantValue {
                        enum_name: "Option".to_string(),
                        variant_name: "Some".to_string(),
                        payloads: Vec::new(),
                    }),
                    "cwd",
                );
            }
            "optional-string-payload-type" => {
                super::expect_optional_string_value(
                    &Value::EnumVariant(EnumVariantValue {
                        enum_name: "Option".to_string(),
                        variant_name: "Some".to_string(),
                        payloads: vec![Value::Bool(true)],
                    }),
                    "cwd",
                );
            }
            "optional-string-type" => {
                super::expect_optional_string_value(
                    &Value::Int(IntegerValue::from_signed(1)),
                    "cwd",
                );
            }
            "process-start-command-type" => {
                super::aura_direct_process_start(
                    bool_value(true),
                    boxed_value(Value::Unit),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    bool_value(false),
                );
            }
            "process-start-cwd-type" => {
                super::aura_direct_process_start(
                    string_vec(&["/bin/echo", "ok"]),
                    bool_value(true),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    bool_value(false),
                );
            }
            "process-start-env-type" => {
                super::aura_direct_process_start(
                    string_vec(&["/bin/echo", "ok"]),
                    boxed_value(Value::Unit),
                    bool_value(true),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    bool_value(false),
                );
            }
            "process-start-group-type" => {
                super::aura_direct_process_start(
                    string_vec(&["/bin/echo", "ok"]),
                    boxed_value(Value::Unit),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    string_value("group"),
                );
            }
            "process-run-command-type" => {
                super::aura_direct_process_run(
                    bool_value(true),
                    boxed_value(Value::Unit),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    duration_value(1),
                    bool_value(false),
                );
            }
            "process-run-timeout-type" => {
                super::aura_direct_process_run(
                    string_vec(&["/bin/echo", "ok"]),
                    boxed_value(Value::Unit),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    string_value("slow"),
                    bool_value(false),
                );
            }
            "process-run-group-type" => {
                super::aura_direct_process_run(
                    string_vec(&["/bin/echo", "ok"]),
                    boxed_value(Value::Unit),
                    super::aura_direct_map_empty(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    duration_value(1),
                    string_value("group"),
                );
            }
            "process-supervisor-start-stdin-type" => {
                start_supervisor_diagnostic_case(
                    bool_value(true),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    process_restart_never_value(),
                );
            }
            "process-supervisor-start-stdout-type" => {
                start_supervisor_diagnostic_case(
                    super::aura_direct_process_null(),
                    bool_value(true),
                    super::aura_direct_process_null(),
                    process_restart_never_value(),
                );
            }
            "process-supervisor-start-stderr-type" => {
                start_supervisor_diagnostic_case(
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    bool_value(true),
                    process_restart_never_value(),
                );
            }
            "process-supervisor-start-restart-type" => {
                start_supervisor_diagnostic_case(
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    super::aura_direct_process_null(),
                    bool_value(true),
                );
            }
            "arg-buffer-negative-size" => {
                super::aura_direct_arg_buffer_new(-1);
            }
            "arg-buffer-negative-index" => {
                let buffer = super::aura_direct_arg_buffer_new(1);
                super::aura_direct_arg_buffer_store(buffer, -1, int_value(1) as i64);
            }
            "task-start-negative-arg-count" => unsafe {
                super::aura_direct_start_task_call(
                    direct_task_fresh_duration as *const () as usize as i64,
                    std::ptr::null(),
                    -1,
                    1,
                    std::ptr::null_mut(),
                    1,
                    0,
                    0,
                );
            },
            "cleanup-negative-arg-count" => {
                super::aura_direct_register_cleanup(1, std::ptr::null_mut(), -1);
            }
            "cleanup-null-thunk" => {
                super::aura_direct_register_cleanup(0, std::ptr::null_mut(), 0);
            }
            "cleanup-refresh-negative-arg-count" => {
                super::aura_direct_refresh_cleanup(1, 0, 1, std::ptr::null_mut(), -1);
            }
            "cleanup-refresh-null-thunk" => {
                super::aura_direct_refresh_cleanup(1, 0, 0, std::ptr::null_mut(), 0);
            }
            "queue-capacity-zero" => {
                super::aura_direct_channel_new(int_value(0));
            }
            "queue-send-type" => {
                super::aura_direct_channel_send(bool_value(true), int_value(1));
            }
            "queue-send-timeout-negative" => {
                super::aura_direct_channel_send_timeout_value(
                    super::aura_direct_channel_new(std::ptr::null_mut()),
                    int_value(1),
                    duration_value(-1),
                );
            }
            "queue-try-send-type" => {
                super::aura_direct_channel_try_send(bool_value(true), int_value(1));
            }
            "queue-recv-type" => {
                super::aura_direct_channel_recv(bool_value(true));
            }
            "queue-recv-timeout-negative" => {
                super::aura_direct_channel_recv_timeout_value(
                    super::aura_direct_channel_new(std::ptr::null_mut()),
                    duration_value(-1),
                );
            }
            "queue-recv-or-none-timeout-negative" => {
                super::aura_direct_channel_recv_or_none_timeout_value(
                    super::aura_direct_channel_new(std::ptr::null_mut()),
                    duration_value(-1),
                );
            }
            "queue-recv-or-none-type" => {
                super::aura_direct_channel_recv_or_none(bool_value(true));
            }
            "queue-recv-or-value-timeout-negative" => {
                super::aura_direct_channel_recv_or_value_timeout_value(
                    super::aura_direct_channel_new(std::ptr::null_mut()),
                    int_value(0),
                    duration_value(-1),
                );
            }
            "queue-recv-or-value-type" => {
                super::aura_direct_channel_recv_or_value(bool_value(true), int_value(0));
            }
            "queue-close-type" => {
                super::aura_direct_channel_close(bool_value(true));
            }
            "queue-recv-in-task-group-queue-type" => {
                super::aura_direct_channel_recv_in_task_group(
                    bool_value(true),
                    super::aura_direct_task_group_new(),
                );
            }
            "queue-recv-in-task-group-group-type" => {
                super::aura_direct_channel_recv_in_task_group(
                    super::aura_direct_channel_new(std::ptr::null_mut()),
                    bool_value(true),
                );
            }
            "queue-recv-registered-producers-type" => {
                super::aura_direct_channel_recv_with_registered_producers(bool_value(true));
            }
            "select-container-type" => {
                super::aura_direct_select(bool_value(true));
            }
            "select-source-type" => {
                super::aura_direct_select(select_sources(
                    vec![Type::named("str")],
                    vec![Value::String("not a source".to_string())],
                ));
            }
            "select-metadata-arity" => {
                super::aura_direct_select(select_sources(Vec::new(), vec![Value::Duration(0)]));
            }
            "select-metadata-arity-plural" => {
                super::aura_direct_select(select_sources(
                    Vec::new(),
                    vec![Value::Duration(0), Value::Duration(1)],
                ));
            }
            "select-metadata-kind" => {
                super::aura_direct_select(select_sources(
                    vec![Type::named("Duration"), Type::named("Duration")],
                    vec![Value::Channel(ChannelValue::new()), Value::Duration(0)],
                ));
            }
            "select-metadata-queue-shape" => {
                super::aura_direct_select(select_sources(
                    vec![Type::Tuple(vec![Type::named("str")])],
                    vec![Value::Channel(ChannelValue::new())],
                ));
            }
            "select-metadata-queue-name" => {
                super::aura_direct_select(select_sources(
                    vec![Type::Named("Task".to_string(), vec![Type::named("str")])],
                    vec![Value::Channel(ChannelValue::new())],
                ));
            }
            "select-metadata-queue-payload" => {
                super::aura_direct_select(select_sources(
                    vec![
                        Type::Named("Queue".to_string(), vec![Type::named("str")]),
                        Type::Named("Queue".to_string(), vec![Type::named("int32")]),
                        Type::named("Duration"),
                    ],
                    vec![
                        Value::Channel(ChannelValue::new()),
                        Value::Channel(ChannelValue::new()),
                        Value::Duration(0),
                    ],
                ));
            }
            "select-metadata-task-shape" => {
                super::aura_direct_select(select_sources(
                    vec![Type::Tuple(vec![Type::named("str")])],
                    vec![Value::Task(TaskValue::from_handle(thread::spawn(|| {
                        Ok(Value::Unit)
                    })))],
                ));
            }
            "select-metadata-task-arity" => {
                super::aura_direct_select(select_sources(
                    vec![Type::named("Task")],
                    vec![Value::Task(TaskValue::from_handle(thread::spawn(|| {
                        Ok(Value::Unit)
                    })))],
                ));
            }
            "select-metadata-task-name" => {
                super::aura_direct_select(select_sources(
                    vec![Type::Named("Queue".to_string(), vec![Type::Unit])],
                    vec![Value::Task(TaskValue::from_handle(thread::spawn(|| {
                        Ok(Value::Unit)
                    })))],
                ));
            }
            "select-metadata-task-result" => {
                super::aura_direct_select(select_sources(
                    vec![
                        Type::Named("Task".to_string(), vec![Type::named("str")]),
                        Type::Named("Task".to_string(), vec![Type::named("int32")]),
                        Type::named("Duration"),
                    ],
                    vec![
                        Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)))),
                        Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(Value::Unit)))),
                        Value::Duration(0),
                    ],
                ));
            }
            "select-metadata-duration-kind" => {
                super::aura_direct_select(select_sources(
                    vec![Type::named("str")],
                    vec![Value::Duration(0)],
                ));
            }
            "wait-any-timeout-negative" => {
                super::aura_direct_wait_any_timeout_value(
                    super::aura_direct_vec_empty(),
                    duration_value(-1),
                );
            }
            "wait-all-timeout-negative" => {
                super::aura_direct_wait_all_timeout_value(
                    super::aura_direct_vec_empty(),
                    duration_value(-1),
                );
            }
            "wait-any-tasks-type" => {
                super::aura_direct_wait_any(bool_value(true));
            }
            "task-result-type" => {
                super::aura_direct_task_join(bool_value(true));
            }
            "task-result-timeout-negative" => {
                super::aura_direct_task_join_timeout_value(bool_value(true), duration_value(-1));
            }
            "task-result-or-none-type" => {
                super::aura_direct_task_join_or_none(bool_value(true));
            }
            "task-result-or-none-timeout-negative" => {
                super::aura_direct_task_join_or_none_timeout_value(
                    bool_value(true),
                    duration_value(-1),
                );
            }
            "task-result-or-type" => {
                super::aura_direct_task_join_or_value(bool_value(true), int_value(0));
            }
            "task-result-or-timeout-negative" => {
                super::aura_direct_task_join_or_value_timeout_value(
                    bool_value(true),
                    int_value(0),
                    duration_value(-1),
                );
            }
            "task-group-cancel-type" => {
                super::aura_direct_task_group_cancel(bool_value(true));
            }
            "task-group-close-type" => {
                super::aura_direct_task_group_close(bool_value(true), 0);
            }
            "io-write-type" => {
                super::aura_direct_io_write(bool_value(true));
            }
            "fs-exists-type" => {
                super::aura_direct_fs_exists(bool_value(true));
            }
            "fs-read-to-string-type" => {
                super::aura_direct_fs_read_to_string(bool_value(true));
            }
            "fs-read-bytes-type" => {
                super::aura_direct_fs_read_bytes(bool_value(true));
            }
            "fs-write-string-path-type" => {
                super::aura_direct_fs_write_string(bool_value(true), string_value("text"));
            }
            "fs-write-string-text-type" => {
                super::aura_direct_fs_write_string(string_value("/tmp/unused"), bool_value(true));
            }
            "fs-write-bytes-path-type" => {
                super::aura_direct_fs_write_bytes(bool_value(true), int_vec(&[1, 2]));
            }
            "fs-append-string-path-type" => {
                super::aura_direct_fs_append_string(bool_value(true), string_value("text"));
            }
            "fs-append-string-text-type" => {
                super::aura_direct_fs_append_string(string_value("/tmp/unused"), bool_value(true));
            }
            "fs-append-bytes-path-type" => {
                super::aura_direct_fs_append_bytes(bool_value(true), int_vec(&[1, 2]));
            }
            "fs-append-bytes-bytes-type" => {
                super::aura_direct_fs_append_bytes(string_value("/tmp/unused"), bool_value(true));
            }
            "fs-create-dir-type" => {
                super::aura_direct_fs_create_dir(bool_value(true));
            }
            "fs-read-dir-type" => {
                super::aura_direct_fs_read_dir(bool_value(true));
            }
            "fs-remove-file-type" => {
                super::aura_direct_fs_remove_file(bool_value(true));
            }
            "fs-open-type" => {
                super::aura_direct_fs_open(bool_value(true));
            }
            "fs-create-type" => {
                super::aura_direct_fs_create(bool_value(true));
            }
            "fs-append-type" => {
                super::aura_direct_fs_append(bool_value(true));
            }
            "file-read-all-type" => {
                super::aura_direct_file_read_all(bool_value(true));
            }
            "file-read-bytes-type" => {
                super::aura_direct_file_read_bytes(bool_value(true));
            }
            "file-write-all-text-type" => {
                super::aura_direct_file_write_all(bool_value(true), bool_value(true));
            }
            "file-write-all-file-type" => {
                super::aura_direct_file_write_all(bool_value(true), string_value("text"));
            }
            "file-write-bytes-file-type" => {
                super::aura_direct_file_write_bytes(bool_value(true), int_vec(&[1, 2]));
            }
            "file-flush-type" => {
                super::aura_direct_file_flush(bool_value(true));
            }
            "file-close-type" => {
                super::aura_direct_file_close(bool_value(true));
            }
            "contains-arg" => {
                super::aura_direct_string_contains(string_value("aura"), bool_value(true));
            }
            "contains-receiver" => {
                super::aura_direct_string_contains(bool_value(true), string_value("a"));
            }
            "starts-with-arg" => {
                super::aura_direct_string_starts_with(string_value("aura"), bool_value(true));
            }
            "starts-with-receiver" => {
                super::aura_direct_string_starts_with(bool_value(true), string_value("a"));
            }
            "ends-with-arg" => {
                super::aura_direct_string_ends_with(string_value("aura"), bool_value(true));
            }
            "ends-with-receiver" => {
                super::aura_direct_string_ends_with(bool_value(true), string_value("a"));
            }
            "split-arg" => {
                super::aura_direct_string_split(string_value("a,b"), bool_value(true));
            }
            "split-receiver" => {
                super::aura_direct_string_split(bool_value(true), string_value(","));
            }
            "replace-from" => {
                super::aura_direct_string_replace(
                    string_value("aura"),
                    bool_value(true),
                    string_value("x"),
                );
            }
            "replace-to" => {
                super::aura_direct_string_replace(
                    string_value("aura"),
                    string_value("a"),
                    bool_value(true),
                );
            }
            "replace-receiver" => {
                super::aura_direct_string_replace(
                    bool_value(true),
                    string_value("a"),
                    string_value("x"),
                );
            }
            "string-len-type" => {
                super::aura_direct_string_len(bool_value(true));
            }
            "invalid-uint-literal" => {
                super::aura_direct_box_uint_literal(b"abc".as_ptr(), 3);
            }
            "to-lower-receiver" => {
                super::aura_direct_string_to_lower(bool_value(true));
            }
            "to-upper-receiver" => {
                super::aura_direct_string_to_upper(bool_value(true));
            }
            "strip-prefix-arg" => {
                super::aura_direct_string_strip_prefix(string_value("prefix"), bool_value(true));
            }
            "strip-prefix-receiver" => {
                super::aura_direct_string_strip_prefix(bool_value(true), string_value("p"));
            }
            "strip-suffix-arg" => {
                super::aura_direct_string_strip_suffix(string_value("suffix"), bool_value(true));
            }
            "strip-suffix-receiver" => {
                super::aura_direct_string_strip_suffix(bool_value(true), string_value("x"));
            }
            "trim-receiver" => {
                super::aura_direct_string_trim(bool_value(true));
            }
            "join-part-element" => {
                let vec = super::aura_direct_vec_empty();
                super::aura_direct_vec_push_in_place(vec, int_value(1));
                super::aura_direct_string_join(string_value(", "), vec);
            }
            "join-parts" => {
                super::aura_direct_string_join(string_value(", "), int_value(1));
            }
            "join-separator" => {
                super::aura_direct_string_join(bool_value(true), string_vec(&["a", "b"]));
            }
            "abs-type" => {
                super::aura_direct_abs(string_value("oops"));
            }
            "min-mismatch" => {
                super::aura_direct_min(int_value(1), float_value(2.0));
            }
            "max-mismatch" => {
                super::aura_direct_max(int_value(1), float_value(2.0));
            }
            "sqrt-type" => {
                super::aura_direct_sqrt(int_value(9));
            }
            "parse-int32-type" => {
                super::aura_direct_parse_int32(bool_value(true));
            }
            "parse-int64-type" => {
                super::aura_direct_parse_int64(bool_value(true));
            }
            "parse-float64-type" => {
                super::aura_direct_parse_float64(bool_value(true));
            }
            "map-index-missing" => {
                let map = super::aura_direct_map_empty();
                expect_option_none(super::aura_direct_map_set_in_place(
                    map,
                    string_value("name"),
                    int_value(1),
                ));
                super::aura_direct_map_index(map, string_value("missing"), 2, 7);
            }
            "map-index-missing-no-span" => {
                super::aura_direct_map_index(
                    super::aura_direct_map_empty(),
                    string_value("missing"),
                    0,
                    0,
                );
            }
            "vec-extend-type" => {
                super::aura_direct_vec_extend_in_place(
                    super::aura_direct_vec_empty(),
                    int_value(1),
                );
            }
            "map-extend-type" => {
                let map = super::aura_direct_map_empty();
                super::aura_direct_map_extend_in_place(map, int_value(1));
            }
            "variant-payload-none" => {
                let ready = super::aura_direct_enum_variant(
                    b"Status".as_ptr(),
                    "Status".len(),
                    b"Ready".as_ptr(),
                    "Ready".len(),
                    std::ptr::null_mut(),
                    0,
                );
                super::aura_direct_variant_payload(ready, 0);
            }
            "variant-payload-type" => {
                super::aura_direct_variant_payload(int_value(1), 0);
            }
            "instance-get-missing" => {
                let empty = super::aura_direct_instance_empty(b"Counter".as_ptr(), "Counter".len());
                super::aura_direct_instance_get_field(empty, b"value".as_ptr(), "value".len());
            }
            "instance-get-type" => {
                super::aura_direct_instance_get_field(
                    int_value(1),
                    b"value".as_ptr(),
                    "value".len(),
                );
            }
            "range-current-type" => {
                super::aura_direct_range_current(int_value(1));
            }
            "range-current-overflow" => {
                let range = boxed_value(Value::Range(RangeValue {
                    start: i128::from(i64::MAX) + 1,
                    end: 0,
                }));
                super::aura_direct_range_current(range);
            }
            "range-end-type" => {
                super::aura_direct_range_end(int_value(1));
            }
            "range-end-overflow" => {
                let range = boxed_value(Value::Range(RangeValue {
                    start: 0,
                    end: i128::from(i64::MAX) + 1,
                }));
                super::aura_direct_range_end(range);
            }
            "range-advance-type" => {
                super::aura_direct_range_advance(int_value(1));
            }
            "vec-len-type" => {
                super::aura_direct_vec_len(int_value(1));
            }
            "vec-push-type" => {
                super::aura_direct_vec_push_in_place(int_value(1), int_value(2));
            }
            "map-len-type" => {
                super::aura_direct_map_len(int_value(1));
            }
            "map-index-type" => {
                super::aura_direct_map_index(int_value(1), string_value("name"), 0, 0);
            }
            "map-set-type" => {
                super::aura_direct_map_set_in_place(
                    int_value(1),
                    string_value("name"),
                    int_value(1),
                );
            }
            "map-set-index-type" => {
                super::aura_direct_map_set_index_in_place(
                    int_value(1),
                    string_value("name"),
                    int_value(1),
                    0,
                    0,
                );
            }
            "map-clear-type" => {
                super::aura_direct_map_clear_in_place(int_value(1));
            }
            "map-keys-type" => {
                super::aura_direct_map_keys(int_value(1));
            }
            "map-values-type" => {
                super::aura_direct_map_values(int_value(1));
            }
            "map-items-type" => {
                super::aura_direct_map_items(int_value(1));
            }
            "map-extend-target-type" => {
                super::aura_direct_map_extend_in_place(
                    int_value(1),
                    super::aura_direct_map_empty(),
                );
            }
            "set-len-type" => {
                super::aura_direct_set_len(int_value(1));
            }
            "set-is-empty-type" => {
                super::aura_direct_set_is_empty(int_value(1));
            }
            "set-contains-type" => {
                super::aura_direct_set_contains(int_value(1), int_value(2));
            }
            "set-insert-type" => {
                super::aura_direct_set_insert_in_place(int_value(1), int_value(2));
            }
            "set-remove-type" => {
                super::aura_direct_set_remove_in_place(int_value(1), int_value(2));
            }
            "set-index-type" => {
                super::aura_direct_set_index_option(int_value(1), 0);
            }
            "tcp-read-all-type" => {
                super::aura_direct_tcp_stream_read_all(bool_value(true), duration_value(1));
            }
            "tcp-read-line-type" => {
                super::aura_direct_tcp_stream_read_line(bool_value(true), duration_value(1));
            }
            "tcp-read-bytes-count-type" => {
                super::aura_direct_tcp_stream_read_bytes(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tcp-read-bytes-type" => {
                super::aura_direct_tcp_stream_read_bytes(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "tcp-read-bytes-negative-count" => {
                super::aura_direct_tcp_stream_read_bytes(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "tcp-read-exact-count-type" => {
                super::aura_direct_tcp_stream_read_exact(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tcp-read-exact-type" => {
                super::aura_direct_tcp_stream_read_exact(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "tcp-read-exact-negative-count" => {
                super::aura_direct_tcp_stream_read_exact(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "tcp-write-all-text-type" => {
                super::aura_direct_tcp_stream_write_all(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tcp-write-all-type" => {
                super::aura_direct_tcp_stream_write_all(
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "tcp-write-bytes-bytes-type" => {
                super::aura_direct_tcp_stream_write_bytes(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tcp-write-bytes-type" => {
                super::aura_direct_tcp_stream_write_bytes(
                    bool_value(true),
                    int_vec(&[1, 2]),
                    duration_value(1),
                );
            }
            "tcp-shutdown-read-type" => {
                super::aura_direct_tcp_stream_shutdown_read(bool_value(true));
            }
            "tcp-shutdown-write-type" => {
                super::aura_direct_tcp_stream_shutdown_write(bool_value(true));
            }
            "tcp-shutdown-both-type" => {
                super::aura_direct_tcp_stream_shutdown_both(bool_value(true));
            }
            "tcp-flush-type" => {
                super::aura_direct_tcp_stream_flush(bool_value(true));
            }
            "tcp-local-addr-type" => {
                super::aura_direct_tcp_stream_local_addr(bool_value(true));
            }
            "tcp-peer-addr-type" => {
                super::aura_direct_tcp_stream_peer_addr(bool_value(true));
            }
            "tcp-close-type" => {
                super::aura_direct_tcp_stream_close(bool_value(true));
            }
            "udp-send-text-address-type" => {
                super::aura_direct_udp_socket_send_text(
                    bool_value(true),
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "udp-send-text-text-type" => {
                super::aura_direct_udp_socket_send_text(
                    bool_value(true),
                    string_value("127.0.0.1:9"),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "udp-send-text-type" => {
                super::aura_direct_udp_socket_send_text(
                    bool_value(true),
                    string_value("127.0.0.1:9"),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "udp-send-bytes-address-type" => {
                super::aura_direct_udp_socket_send_bytes(
                    bool_value(true),
                    bool_value(true),
                    int_vec(&[1, 2]),
                    duration_value(1),
                );
            }
            "udp-send-bytes-bytes-type" => {
                super::aura_direct_udp_socket_send_bytes(
                    bool_value(true),
                    string_value("127.0.0.1:9"),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "udp-send-bytes-type" => {
                super::aura_direct_udp_socket_send_bytes(
                    bool_value(true),
                    string_value("127.0.0.1:9"),
                    int_vec(&[1, 2]),
                    duration_value(1),
                );
            }
            "udp-recv-count-type" => {
                super::aura_direct_udp_socket_recv(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "udp-recv-negative-count" => {
                super::aura_direct_udp_socket_recv(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "udp-recv-type" => {
                super::aura_direct_udp_socket_recv(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "udp-recv-from-count-type" => {
                super::aura_direct_udp_socket_recv_from(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "udp-recv-from-negative-count" => {
                super::aura_direct_udp_socket_recv_from(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "udp-recv-from-type" => {
                super::aura_direct_udp_socket_recv_from(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "udp-local-addr-type" => {
                super::aura_direct_udp_socket_local_addr(bool_value(true));
            }
            "udp-peer-addr-type" => {
                super::aura_direct_udp_socket_peer_addr(bool_value(true));
            }
            "udp-close-type" => {
                super::aura_direct_udp_socket_close(bool_value(true));
            }
            "udp-datagram-address-type" => {
                super::aura_direct_udp_datagram_address(bool_value(true));
            }
            "udp-datagram-bytes-type" => {
                super::aura_direct_udp_datagram_bytes(bool_value(true));
            }
            "udp-datagram-text-type" => {
                super::aura_direct_udp_datagram_text(bool_value(true));
            }
            "process-supervisor-wait-type" => {
                super::aura_direct_process_supervisor_wait(bool_value(true), duration_value(1));
            }
            "process-supervisor-wait-or-none-type" => {
                super::aura_direct_process_supervisor_wait_or_none(
                    bool_value(true),
                    duration_value(1),
                );
            }
            "process-supervisor-stop-type" => {
                super::aura_direct_process_supervisor_stop(bool_value(true));
            }
            "process-supervisor-is-empty-type" => {
                super::aura_direct_process_supervisor_is_empty(bool_value(true));
            }
            "process-supervisor-close-type" => {
                super::aura_direct_process_supervisor_close(bool_value(true));
            }
            "process-child-stdin-type" => {
                super::aura_direct_process_child_stdin(bool_value(true));
            }
            "process-child-stdout-type" => {
                super::aura_direct_process_child_stdout(bool_value(true));
            }
            "process-child-stderr-type" => {
                super::aura_direct_process_child_stderr(bool_value(true));
            }
            "process-child-wait-type" => {
                super::aura_direct_process_child_wait(bool_value(true), duration_value(1));
            }
            "process-child-wait-or-none-type" => {
                super::aura_direct_process_child_wait_or_none(bool_value(true), duration_value(1));
            }
            "process-child-wait-ok-type" => {
                super::aura_direct_process_child_wait_ok(bool_value(true), duration_value(1));
            }
            "process-child-kill-type" => {
                super::aura_direct_process_child_kill(bool_value(true));
            }
            "process-child-terminate-type" => {
                super::aura_direct_process_child_terminate(bool_value(true));
            }
            "process-child-close-type" => {
                super::aura_direct_process_child_close(bool_value(true));
            }
            "process-pipe-read-all-type" => {
                super::aura_direct_process_pipe_read_all(bool_value(true));
            }
            "process-pipe-read-line-type" => {
                super::aura_direct_process_pipe_read_line(bool_value(true), duration_value(1));
            }
            "process-pipe-read-bytes-count-type" => {
                super::aura_direct_process_pipe_read_bytes(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "process-pipe-read-bytes-negative-count" => {
                super::aura_direct_process_pipe_read_bytes(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "process-pipe-read-bytes-type" => {
                super::aura_direct_process_pipe_read_bytes(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "process-pipe-write-all-text-type" => {
                super::aura_direct_process_pipe_write_all(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "process-pipe-write-all-type" => {
                super::aura_direct_process_pipe_write_all(
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "process-pipe-write-bytes-bytes-type" => {
                super::aura_direct_process_pipe_write_bytes(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "process-pipe-write-bytes-type" => {
                super::aura_direct_process_pipe_write_bytes(
                    bool_value(true),
                    int_vec(&[1, 2]),
                    duration_value(1),
                );
            }
            "process-pipe-flush-type" => {
                super::aura_direct_process_pipe_flush(bool_value(true));
            }
            "process-pipe-close-type" => {
                super::aura_direct_process_pipe_close(bool_value(true));
            }
            "process-completed-status-type" => {
                super::aura_direct_process_completed_status(bool_value(true));
            }
            "process-completed-success-type" => {
                super::aura_direct_process_completed_success(bool_value(true));
            }
            "process-completed-stdout-type" => {
                super::aura_direct_process_completed_stdout(bool_value(true));
            }
            "process-completed-stderr-type" => {
                super::aura_direct_process_completed_stderr(bool_value(true));
            }
            "process-completed-stdout-bytes-type" => {
                super::aura_direct_process_completed_stdout_bytes(bool_value(true));
            }
            "process-completed-stderr-bytes-type" => {
                super::aura_direct_process_completed_stderr_bytes(bool_value(true));
            }
            "process-completed-check-type" => {
                super::aura_direct_process_completed_check(bool_value(true));
            }
            "net-connect-type" => {
                super::aura_direct_net_connect(bool_value(true));
            }
            "net-connect-timeout-type" => {
                super::aura_direct_net_connect_timeout(bool_value(true), duration_value(1));
            }
            "net-listen-type" => {
                super::aura_direct_net_listen(bool_value(true));
            }
            "net-udp-bind-type" => {
                super::aura_direct_net_udp_bind(bool_value(true));
            }
            "net-unix-listen-type" => {
                super::aura_direct_net_unix_listen(bool_value(true));
            }
            "net-unix-connect-type" => {
                super::aura_direct_net_unix_connect(bool_value(true));
            }
            "net-unix-connect-timeout-type" => {
                super::aura_direct_net_unix_connect_timeout(bool_value(true), duration_value(1));
            }
            "net-tls-listen-address-type" => {
                super::aura_direct_net_tls_listen(
                    bool_value(true),
                    string_value("/tmp/cert.pem"),
                    string_value("/tmp/key.pem"),
                );
            }
            "net-tls-connect-address-type" => {
                super::aura_direct_net_tls_connect(
                    bool_value(true),
                    string_value("localhost"),
                    string_value("/tmp/ca.pem"),
                );
            }
            "net-http-listen-type" => {
                super::aura_direct_net_http_listen(bool_value(true));
            }
            "net-websocket-listen-type" => {
                super::aura_direct_net_websocket_listen(bool_value(true));
            }
            "net-websocket-connect-type" => {
                super::aura_direct_net_websocket_connect(bool_value(true));
            }
            "net-websocket-connect-timeout-type" => {
                super::aura_direct_net_websocket_connect_timeout(
                    bool_value(true),
                    duration_value(1),
                );
            }
            "http-listener-accept-type" => {
                super::aura_direct_http_listener_accept(bool_value(true), duration_value(1));
            }
            "http-listener-local-addr-type" => {
                super::aura_direct_http_listener_local_addr(bool_value(true));
            }
            "http-listener-close-type" => {
                super::aura_direct_http_listener_close(bool_value(true));
            }
            "http-exchange-method-type" => {
                super::aura_direct_http_exchange_method(bool_value(true));
            }
            "http-exchange-path-type" => {
                super::aura_direct_http_exchange_path(bool_value(true));
            }
            "http-exchange-headers-type" => {
                super::aura_direct_http_exchange_headers(bool_value(true));
            }
            "http-exchange-body-text-type" => {
                super::aura_direct_http_exchange_body_text(bool_value(true));
            }
            "http-exchange-body-bytes-type" => {
                super::aura_direct_http_exchange_body_bytes(bool_value(true));
            }
            "http-exchange-respond-text-type" => {
                super::aura_direct_http_exchange_respond_text(
                    bool_value(true),
                    int_value(200),
                    string_value("ok"),
                    string_map(&[]),
                );
            }
            "http-exchange-respond-bytes-type" => {
                super::aura_direct_http_exchange_respond_bytes(
                    bool_value(true),
                    int_value(200),
                    int_vec(&[1, 2]),
                    string_map(&[]),
                );
            }
            "http-response-status-type" => {
                super::aura_direct_http_response_status(bool_value(true));
            }
            "http-response-reason-type" => {
                super::aura_direct_http_response_reason(bool_value(true));
            }
            "http-response-headers-type" => {
                super::aura_direct_http_response_headers(bool_value(true));
            }
            "http-response-text-type" => {
                super::aura_direct_http_response_text(bool_value(true));
            }
            "http-response-bytes-type" => {
                super::aura_direct_http_response_bytes(bool_value(true));
            }
            "websocket-listener-accept-type" => {
                super::aura_direct_websocket_listener_accept(bool_value(true), duration_value(1));
            }
            "websocket-listener-local-addr-type" => {
                super::aura_direct_websocket_listener_local_addr(bool_value(true));
            }
            "websocket-send-text-type" => {
                super::aura_direct_websocket_send_text(
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "websocket-send-bytes-type" => {
                super::aura_direct_websocket_send_bytes(
                    bool_value(true),
                    int_vec(&[1, 2]),
                    duration_value(1),
                );
            }
            "websocket-recv-text-type" => {
                super::aura_direct_websocket_recv_text(bool_value(true), duration_value(1));
            }
            "websocket-recv-bytes-type" => {
                super::aura_direct_websocket_recv_bytes(bool_value(true), duration_value(1));
            }
            "websocket-close-type" => {
                super::aura_direct_websocket_close(bool_value(true));
            }
            "unix-listener-accept-type" => {
                super::aura_direct_unix_listener_accept(bool_value(true), duration_value(1));
            }
            "unix-listener-close-type" => {
                super::aura_direct_unix_listener_close(bool_value(true));
            }
            "unix-stream-read-line-type" => {
                super::aura_direct_unix_stream_read_line(bool_value(true), duration_value(1));
            }
            "unix-stream-read-exact-count-type" => {
                super::aura_direct_unix_stream_read_exact(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "unix-stream-read-exact-negative-count" => {
                super::aura_direct_unix_stream_read_exact(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "unix-stream-read-exact-type" => {
                super::aura_direct_unix_stream_read_exact(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "unix-stream-write-all-text-type" => {
                super::aura_direct_unix_stream_write_all(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "unix-stream-write-all-type" => {
                super::aura_direct_unix_stream_write_all(
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "unix-stream-close-type" => {
                super::aura_direct_unix_stream_close(bool_value(true));
            }
            "tls-listener-accept-type" => {
                super::aura_direct_tls_listener_accept(bool_value(true), duration_value(1));
            }
            "tls-listener-local-addr-type" => {
                super::aura_direct_tls_listener_local_addr(bool_value(true));
            }
            "tls-listener-close-type" => {
                super::aura_direct_tls_listener_close(bool_value(true));
            }
            "tls-stream-read-line-type" => {
                super::aura_direct_tls_stream_read_line(bool_value(true), duration_value(1));
            }
            "tls-stream-read-exact-count-type" => {
                super::aura_direct_tls_stream_read_exact(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tls-stream-read-exact-negative-count" => {
                super::aura_direct_tls_stream_read_exact(
                    bool_value(true),
                    int_value(-1),
                    duration_value(1),
                );
            }
            "tls-stream-read-exact-type" => {
                super::aura_direct_tls_stream_read_exact(
                    bool_value(true),
                    int_value(1),
                    duration_value(1),
                );
            }
            "tls-stream-write-all-text-type" => {
                super::aura_direct_tls_stream_write_all(
                    bool_value(true),
                    bool_value(true),
                    duration_value(1),
                );
            }
            "tls-stream-write-all-type" => {
                super::aura_direct_tls_stream_write_all(
                    bool_value(true),
                    string_value("hello"),
                    duration_value(1),
                );
            }
            "tls-stream-close-type" => {
                super::aura_direct_tls_stream_close(bool_value(true));
            }
            "sleep-ms-negative" => {
                super::aura_direct_sleep_ms(-1);
            }
            "sleep-value-negative" => {
                super::aura_direct_sleep_value(duration_value(-1));
            }
            "sleep-value-void-negative" => {
                super::aura_direct_sleep_value_void(duration_value(-1));
            }
            "fail-division-no-span" => {
                super::aura_direct_fail_division_by_zero(0, 0);
            }
            "fail-int32-overflow-no-span" => {
                super::aura_direct_fail_int32_overflow(123, 0, 0);
            }
            "vec-index-oob-no-span" => {
                super::aura_direct_vec_index(int_vec(&[1]), 5, 0, 0);
            }
            "vec-set-oob-no-span" => {
                super::aura_direct_vec_set_index_in_place(int_vec(&[1]), 5, int_value(9), 0, 0);
            }
            "vec-set-oob-span" => {
                super::aura_direct_vec_set_index_in_place(int_vec(&[1]), 5, int_value(9), 2, 7);
            }
            "vec-method-set-too-negative" => {
                super::aura_direct_vec_set_in_place(int_vec(&[1, 2, 3, 4]), -5, int_value(9));
            }
            "vec-method-remove-too-negative" => {
                super::aura_direct_vec_remove_in_place(int_vec(&[1, 2, 3, 4]), -5);
            }
            "vec-method-swap-too-negative" => {
                super::aura_direct_vec_swap_in_place(int_vec(&[1, 2, 3, 4]), -5, -1);
            }
            "vec-indexed-write-too-negative" => {
                super::aura_direct_vec_set_index_in_place(
                    int_vec(&[1, 2, 3, 4]),
                    -5,
                    int_value(9),
                    3,
                    7,
                );
            }
            "unbox-i64-overflow" => {
                super::aura_direct_unbox_i64(boxed_value(Value::Int(IntegerValue::from_literal(
                    (i64::MAX as u128) + 1,
                ))));
            }
            "unbox-i64-type" => {
                super::aura_direct_unbox_i64(bool_value(true));
            }
            "unbox-int64-overflow" => {
                super::aura_direct_unbox_int64(boxed_value(Value::Int(
                    IntegerValue::from_literal((i64::MAX as u128) + 1),
                )));
            }
            "unbox-int64-type" => {
                super::aura_direct_unbox_int64(bool_value(true));
            }
            "unbox-u64-negative" => {
                super::aura_direct_unbox_u64(int_value(-1));
            }
            "unbox-u64-overflow" => {
                super::aura_direct_unbox_u64(boxed_value(Value::Int(IntegerValue::from_literal(
                    (u64::MAX as u128) + 1,
                ))));
            }
            "unbox-u64-type" => {
                super::aura_direct_unbox_u64(bool_value(true));
            }
            "unbox-f64-type" => {
                super::aura_direct_unbox_f64(int_value(1));
            }
            "unbox-bool-type" => {
                super::aura_direct_unbox_bool(int_value(1));
            }
            "condition-type" => {
                super::aura_direct_value_as_condition(string_value("aura"));
            }
            "unary-invalid-op" => {
                super::aura_direct_unary_value(99, int_value(1));
            }
            "unary-at-no-span" => {
                super::aura_direct_unary_value_at(0, string_value("aura"), 0, 0);
            }
            "unary-at-span" => {
                super::aura_direct_unary_value_at(0, string_value("aura"), 2, 7);
            }
            "binary-invalid-op" => {
                super::aura_direct_binary_value(99, int_value(1), int_value(2));
            }
            "binary-floor-zero-no-span" => {
                super::aura_direct_binary_value(13, int_value(1), int_value(0));
            }
            "binary-at-no-span" => {
                super::aura_direct_binary_value_at(
                    0,
                    string_value("aura"),
                    bool_value(true),
                    0,
                    0,
                    0,
                );
            }
            "binary-at-span" => {
                super::aura_direct_binary_value_at(
                    0,
                    string_value("aura"),
                    bool_value(true),
                    0,
                    2,
                    9,
                );
            }
            "cast-no-span" => {
                super::aura_direct_cast_value(
                    string_value("aura"),
                    b"int32".as_ptr(),
                    "int32".len(),
                );
            }
            "cast-at-span" => {
                super::aura_direct_cast_value_at(
                    string_value("aura"),
                    b"int32".as_ptr(),
                    "int32".len(),
                    4,
                    3,
                );
            }
            "task-join-error" => {
                let task = boxed_value(Value::Task(TaskValue::from_handle(thread::spawn(|| {
                    Err(Diagnostic::new("boom"))
                }))));
                let joined = super::aura_direct_task_join(task);
                assert_eq!(expect_task_result_error_message(joined), "boom");
                return;
            }
            other => panic!("unexpected runtime helper case: {other}"),
        }
        panic!("runtime helper case `{case}` unexpectedly returned without trapping");
    }

    for (case, expected) in [
        (
            "bytes-value-type",
            "`bytes` expects `list[uint8]`, found `str`",
        ),
        ("bytes-element-range", "`bytes` expects `list[uint8]`"),
        ("bool-value-type", "`flag` expects `bool`, found `str`"),
        ("i32-overflow", "`count` expects `int32`"),
        ("i32-value-type", "`count` expects `int32`, found `str`"),
        (
            "headers-map-type",
            "`headers` expects `dict[str, str]`, found `str`",
        ),
        (
            "headers-key-type",
            "`headers` expects `str`, found `integer`",
        ),
        (
            "optional-timeout-type",
            "`timeout` expects `Duration`, found `str`",
        ),
        ("optional-timeout-negative", "timeout must be non-negative"),
        (
            "process-timeout-type",
            "`timeout` expects `Duration`, found `str`",
        ),
        (
            "duration-type",
            "`duration` expects `Duration`, found `str`",
        ),
        ("duration-negative", "duration must be non-negative"),
        (
            "supervisor-max-too-low",
            "`max_restarts` expects `max_restarts` to be -1 or greater",
        ),
        (
            "command-vec-type",
            "`command` expects `list[str]`, found `str`",
        ),
        (
            "command-element-type",
            "`command` expects `str`, found `integer`",
        ),
        (
            "optional-string-malformed",
            "`cwd` expects `Option[str]`, found malformed option payload",
        ),
        (
            "optional-string-payload-type",
            "`cwd` expects `str`, found `bool`",
        ),
        (
            "optional-string-type",
            "`cwd` expects `Option[str]`, found `integer`",
        ),
        (
            "process-start-command-type",
            "`process.start(...)` expects `list[str]`, found `bool`",
        ),
        (
            "process-start-cwd-type",
            "`process.start(...)` expects `Option[str]`, found `bool`",
        ),
        (
            "process-start-env-type",
            "`process.start(...)` expects `dict[str, str]`, found `bool`",
        ),
        (
            "process-start-group-type",
            "`process.start(...)` expects `bool`, found `str`",
        ),
        (
            "process-run-command-type",
            "`process.run(...)` expects `list[str]`, found `bool`",
        ),
        (
            "process-run-timeout-type",
            "`process.run(...)` expects `Duration`, found `str`",
        ),
        (
            "process-run-group-type",
            "`process.run(...)` expects `bool`, found `str`",
        ),
        (
            "process-supervisor-start-stdin-type",
            "`start(...)` expects `process.Stdio`",
        ),
        (
            "process-supervisor-start-stdout-type",
            "`start(...)` expects `process.Stdio`",
        ),
        (
            "process-supervisor-start-stderr-type",
            "`start(...)` expects `process.Stdio`",
        ),
        (
            "process-supervisor-start-restart-type",
            "`start(...)` expects `process.RestartPolicy`",
        ),
        ("arg-buffer-negative-size", "invalid arg buffer size"),
        ("arg-buffer-negative-index", "invalid arg index"),
        (
            "task-start-negative-arg-count",
            "invalid task-start arg count",
        ),
        ("cleanup-negative-arg-count", "invalid cleanup arg count"),
        ("cleanup-null-thunk", "invalid cleanup thunk pointer"),
        (
            "cleanup-refresh-negative-arg-count",
            "invalid cleanup arg count",
        ),
        (
            "cleanup-refresh-null-thunk",
            "invalid cleanup thunk pointer",
        ),
        (
            "queue-capacity-zero",
            "`Queue(capacity=...)` expects a positive `int32`",
        ),
        ("queue-send-type", "expected `Queue`, found `bool`"),
        (
            "queue-send-timeout-negative",
            "put(timeout=...) must be non-negative",
        ),
        ("queue-try-send-type", "expected `Queue`, found `bool`"),
        ("queue-recv-type", "expected `Queue`, found `bool`"),
        (
            "queue-recv-timeout-negative",
            "get(timeout=...) must be non-negative",
        ),
        (
            "queue-recv-or-none-timeout-negative",
            "get_or_none(timeout=...) must be non-negative",
        ),
        ("queue-recv-or-none-type", "expected `Queue`, found `bool`"),
        (
            "queue-recv-or-value-timeout-negative",
            "get_or(timeout=...) must be non-negative",
        ),
        ("queue-recv-or-value-type", "expected `Queue`, found `bool`"),
        ("queue-close-type", "expected `Queue`, found `bool`"),
        (
            "queue-recv-in-task-group-queue-type",
            "expected `Queue`, found `bool`",
        ),
        (
            "queue-recv-in-task-group-group-type",
            "expected `TaskGroup`, found `bool`",
        ),
        (
            "queue-recv-registered-producers-type",
            "expected `Queue`, found `bool`",
        ),
        (
            "select-container-type",
            "expected an owned tuple of Queue, Task, or Duration sources, found `bool`",
        ),
        (
            "select-source-type",
            "select source 0 must be a Queue, Task, or Duration",
        ),
        (
            "select-metadata-arity",
            "direct `select` ABI tuple metadata has 0 element types for 1 source",
        ),
        (
            "select-metadata-arity-plural",
            "direct `select` ABI tuple metadata has 0 element types for 2 sources",
        ),
        (
            "select-metadata-kind",
            "direct `select` ABI source 0 is tagged `Duration` but contains `Queue`",
        ),
        (
            "select-metadata-queue-shape",
            "direct `select` ABI source 0 is tagged `(str,)` but contains `Queue`",
        ),
        (
            "select-metadata-queue-name",
            "direct `select` ABI source 0 is tagged `Task[str]` but contains `Queue`",
        ),
        (
            "select-metadata-queue-payload",
            "direct `select` ABI Queue sources must share one payload type",
        ),
        (
            "select-metadata-task-shape",
            "direct `select` ABI source 0 is tagged `(str,)` but contains `Task`",
        ),
        (
            "select-metadata-task-arity",
            "direct `select` ABI source 0 is tagged `Task` but contains `Task`",
        ),
        (
            "select-metadata-task-name",
            "direct `select` ABI source 0 is tagged `Queue[None]` but contains `Task`",
        ),
        (
            "select-metadata-task-result",
            "direct `select` ABI Task sources must share one result type",
        ),
        (
            "select-metadata-duration-kind",
            "direct `select` ABI source 0 is tagged `str` but contains `Duration`",
        ),
        (
            "wait-any-timeout-negative",
            "wait_any(timeout=...) must be non-negative",
        ),
        (
            "wait-all-timeout-negative",
            "wait_all(timeout=...) must be non-negative",
        ),
        (
            "wait-any-tasks-type",
            "expected `wait_any` to receive `list[Task]`, found `bool`",
        ),
        ("task-result-type", "expected `Task`, found `bool`"),
        (
            "task-result-timeout-negative",
            "result(timeout=...) must be non-negative",
        ),
        ("task-result-or-none-type", "expected `Task`, found `bool`"),
        (
            "task-result-or-none-timeout-negative",
            "result_or_none(timeout=...) must be non-negative",
        ),
        ("task-result-or-type", "expected `Task`, found `bool`"),
        (
            "task-result-or-timeout-negative",
            "result_or(timeout=...) must be non-negative",
        ),
        (
            "task-group-cancel-type",
            "expected `TaskGroup`, found `bool`",
        ),
        (
            "task-group-close-type",
            "expected `TaskGroup`, found `bool`",
        ),
        ("io-write-type", "expected `str`, found `bool`"),
        ("fs-exists-type", "expected `str`, found `bool`"),
        ("fs-read-to-string-type", "expected `str`, found `bool`"),
        ("fs-read-bytes-type", "expected `str`, found `bool`"),
        ("fs-write-string-path-type", "expected `str`, found `bool`"),
        ("fs-write-string-text-type", "expected `str`, found `bool`"),
        (
            "fs-write-bytes-path-type",
            "`fs.write_bytes(...)` expects `str`, found `bool`",
        ),
        ("fs-append-string-path-type", "expected `str`, found `bool`"),
        ("fs-append-string-text-type", "expected `str`, found `bool`"),
        (
            "fs-append-bytes-path-type",
            "`fs.append_bytes(...)` expects `str`, found `bool`",
        ),
        (
            "fs-append-bytes-bytes-type",
            "`fs.append_bytes(...)` expects `list[uint8]`, found `bool`",
        ),
        ("fs-create-dir-type", "expected `str`, found `bool`"),
        ("fs-read-dir-type", "expected `str`, found `bool`"),
        ("fs-remove-file-type", "expected `str`, found `bool`"),
        ("fs-open-type", "expected `str`, found `bool`"),
        ("fs-create-type", "expected `str`, found `bool`"),
        ("fs-append-type", "expected `str`, found `bool`"),
        ("file-read-all-type", "expected `fs.File`, found `bool`"),
        ("file-read-bytes-type", "expected `fs.File`, found `bool`"),
        ("file-write-all-text-type", "expected `str`, found `bool`"),
        (
            "file-write-all-file-type",
            "expected `fs.File`, found `bool`",
        ),
        (
            "file-write-bytes-file-type",
            "expected `fs.File`, found `bool`",
        ),
        ("file-flush-type", "expected `fs.File`, found `bool`"),
        ("file-close-type", "expected `fs.File`, found `bool`"),
        ("contains-arg", "`contains` requires a `str` argument"),
        ("contains-receiver", "expected `str`, found `bool`"),
        ("starts-with-arg", "`starts_with` requires a `str` argument"),
        ("starts-with-receiver", "expected `str`, found `bool`"),
        ("ends-with-arg", "`ends_with` requires a `str` argument"),
        ("ends-with-receiver", "expected `str`, found `bool`"),
        ("split-arg", "`split` requires a `str` argument"),
        ("split-receiver", "expected `str`, found `bool`"),
        ("replace-from", "`replace` requires `str` for `from`"),
        ("replace-to", "`replace` requires `str` for `to`"),
        ("replace-receiver", "expected `str`, found `bool`"),
        ("string-len-type", "expected `str`, found `bool`"),
        (
            "invalid-uint-literal",
            "invalid embedded uint literal `abc`",
        ),
        ("to-lower-receiver", "expected `str`, found `bool`"),
        ("to-upper-receiver", "expected `str`, found `bool`"),
        (
            "strip-prefix-arg",
            "`strip_prefix` requires a `str` argument",
        ),
        ("strip-prefix-receiver", "expected `str`, found `bool`"),
        (
            "strip-suffix-arg",
            "`strip_suffix` requires a `str` argument",
        ),
        ("strip-suffix-receiver", "expected `str`, found `bool`"),
        ("trim-receiver", "expected `str`, found `bool`"),
        ("join-part-element", "`join` requires `list[str]`"),
        ("join-parts", "`join` requires `list[str]`"),
        ("join-separator", "expected `str`, found `bool`"),
        ("abs-type", "`abs(...)` expects an integer or float value"),
        (
            "min-mismatch",
            "`min(...)` expects matching numeric arguments",
        ),
        (
            "max-mismatch",
            "`max(...)` expects matching numeric arguments",
        ),
        ("sqrt-type", "`sqrt(...)` expects `float32` or `float64`"),
        (
            "parse-int32-type",
            "`parse_int32(...)` expects `str`, found `bool`",
        ),
        (
            "parse-int64-type",
            "`parse_int64(...)` expects `str`, found `bool`",
        ),
        (
            "parse-float64-type",
            "`parse_float64(...)` expects `str`, found `bool`",
        ),
        ("map-index-missing", "dict key `missing` was not present"),
        (
            "map-index-missing-no-span",
            "dict key `missing` was not present",
        ),
        (
            "vec-extend-type",
            "`extend` requires another `list[T]` value",
        ),
        (
            "map-extend-type",
            "`update` requires another `dict[K, V]` value",
        ),
        ("variant-payload-none", "does not carry a payload"),
        (
            "variant-payload-type",
            "expected enum value, found `integer`",
        ),
        (
            "instance-get-missing",
            "class `Counter` has no field `value`",
        ),
        (
            "instance-get-type",
            "cannot access field `value` on non-instance `integer`",
        ),
        ("range-current-type", "expected `Range`, found `integer`"),
        (
            "range-current-overflow",
            "range start is outside host i64 bounds",
        ),
        ("range-end-type", "expected `Range`, found `integer`"),
        ("range-end-overflow", "range end is outside host i64 bounds"),
        ("range-advance-type", "expected `Range`, found `integer`"),
        ("vec-len-type", "expected `list`, found `integer`"),
        ("vec-push-type", "expected `list`, found `integer`"),
        ("map-len-type", "expected `dict`, found `integer`"),
        ("map-index-type", "expected `dict`, found `integer`"),
        ("map-set-type", "expected `dict`, found `integer`"),
        ("map-set-index-type", "expected `dict`, found `integer`"),
        ("map-clear-type", "expected `dict`, found `integer`"),
        ("map-keys-type", "expected `dict`, found `integer`"),
        ("map-values-type", "expected `dict`, found `integer`"),
        ("map-items-type", "expected `dict`, found `integer`"),
        ("map-extend-target-type", "expected `dict`, found `integer`"),
        ("set-len-type", "expected `set`, found `integer`"),
        ("set-is-empty-type", "expected `set`, found `integer`"),
        ("set-contains-type", "expected `set`, found `integer`"),
        ("set-insert-type", "expected `set`, found `integer`"),
        ("set-remove-type", "expected `set`, found `integer`"),
        ("set-index-type", "expected `set`, found `integer`"),
        (
            "tcp-read-all-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-read-line-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-read-bytes-count-type",
            "`read_bytes(...)` expects `int32`, found `bool`",
        ),
        (
            "tcp-read-bytes-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-read-bytes-negative-count",
            "`read_bytes(...)` requires a non-negative max_bytes",
        ),
        (
            "tcp-read-exact-count-type",
            "`read_exact(...)` expects `int32`, found `bool`",
        ),
        (
            "tcp-read-exact-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-read-exact-negative-count",
            "`read_exact(...)` requires a non-negative count",
        ),
        ("tcp-write-all-text-type", "expected `str`, found `bool`"),
        (
            "tcp-write-all-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-write-bytes-bytes-type",
            "`write_bytes(...)` expects `list[uint8]`, found `bool`",
        ),
        (
            "tcp-write-bytes-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-shutdown-read-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-shutdown-write-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-shutdown-both-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        ("tcp-flush-type", "expected `net.TcpStream`, found `bool`"),
        (
            "tcp-local-addr-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        (
            "tcp-peer-addr-type",
            "expected `net.TcpStream`, found `bool`",
        ),
        ("tcp-close-type", "expected `net.TcpStream`, found `bool`"),
        (
            "udp-send-text-address-type",
            "`send_text(...)` expects `str`, found `bool`",
        ),
        (
            "udp-send-text-text-type",
            "`send_text(...)` expects `str`, found `bool`",
        ),
        (
            "udp-send-text-type",
            "expected `net.UdpSocket`, found `bool`",
        ),
        (
            "udp-send-bytes-address-type",
            "`send_bytes(...)` expects `str`, found `bool`",
        ),
        (
            "udp-send-bytes-bytes-type",
            "`send_bytes(...)` expects `list[uint8]`, found `bool`",
        ),
        (
            "udp-send-bytes-type",
            "expected `net.UdpSocket`, found `bool`",
        ),
        (
            "udp-recv-count-type",
            "`recv(...)` expects `int32`, found `bool`",
        ),
        (
            "udp-recv-negative-count",
            "`recv(...)` requires a non-negative max_bytes",
        ),
        ("udp-recv-type", "expected `net.UdpSocket`, found `bool`"),
        (
            "udp-recv-from-count-type",
            "`recv_from(...)` expects `int32`, found `bool`",
        ),
        (
            "udp-recv-from-negative-count",
            "`recv_from(...)` requires a non-negative max_bytes",
        ),
        (
            "udp-recv-from-type",
            "expected `net.UdpSocket`, found `bool`",
        ),
        (
            "udp-local-addr-type",
            "expected `net.UdpSocket`, found `bool`",
        ),
        (
            "udp-peer-addr-type",
            "expected `net.UdpSocket`, found `bool`",
        ),
        ("udp-close-type", "expected `net.UdpSocket`, found `bool`"),
        (
            "udp-datagram-address-type",
            "expected `net.UdpDatagram`, found `bool`",
        ),
        (
            "udp-datagram-bytes-type",
            "expected `net.UdpDatagram`, found `bool`",
        ),
        (
            "udp-datagram-text-type",
            "expected `net.UdpDatagram`, found `bool`",
        ),
        (
            "process-supervisor-wait-type",
            "expected `process.Supervisor`, found `bool`",
        ),
        (
            "process-supervisor-wait-or-none-type",
            "expected `process.Supervisor`, found `bool`",
        ),
        (
            "process-supervisor-stop-type",
            "expected `process.Supervisor`, found `bool`",
        ),
        (
            "process-supervisor-is-empty-type",
            "expected `process.Supervisor`, found `bool`",
        ),
        (
            "process-supervisor-close-type",
            "expected `process.Supervisor`, found `bool`",
        ),
        (
            "process-child-stdin-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-stdout-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-stderr-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-wait-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-wait-or-none-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-wait-ok-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-kill-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-terminate-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-child-close-type",
            "expected `process.Child`, found `bool`",
        ),
        (
            "process-pipe-read-all-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-read-line-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-read-bytes-count-type",
            "`read_bytes(...)` expects `int32`, found `bool`",
        ),
        (
            "process-pipe-read-bytes-negative-count",
            "`read_bytes(...)` expects a non-negative `max_bytes`",
        ),
        (
            "process-pipe-read-bytes-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-write-all-text-type",
            "`write_all(...)` expects `str`, found `bool`",
        ),
        (
            "process-pipe-write-all-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-write-bytes-bytes-type",
            "`write_bytes(...)` expects `list[uint8]`, found `bool`",
        ),
        (
            "process-pipe-write-bytes-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-flush-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-pipe-close-type",
            "expected `process.Pipe`, found `bool`",
        ),
        (
            "process-completed-status-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-success-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-stdout-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-stderr-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-stdout-bytes-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-stderr-bytes-type",
            "expected `process.Completed`, found `bool`",
        ),
        (
            "process-completed-check-type",
            "expected `process.Completed`, found `bool`",
        ),
        ("net-connect-type", "expected `str`, found `bool`"),
        ("net-connect-timeout-type", "expected `str`, found `bool`"),
        ("net-listen-type", "expected `str`, found `bool`"),
        ("net-udp-bind-type", "expected `str`, found `bool`"),
        ("net-unix-listen-type", "expected `str`, found `bool`"),
        ("net-unix-connect-type", "expected `str`, found `bool`"),
        (
            "net-unix-connect-timeout-type",
            "expected `str`, found `bool`",
        ),
        (
            "net-tls-listen-address-type",
            "`net.tls_listen(...)` expects `str`, found `bool`",
        ),
        (
            "net-tls-connect-address-type",
            "`net.tls_connect(...)` expects `str`, found `bool`",
        ),
        ("net-http-listen-type", "expected `str`, found `bool`"),
        ("net-websocket-listen-type", "expected `str`, found `bool`"),
        ("net-websocket-connect-type", "expected `str`, found `bool`"),
        (
            "net-websocket-connect-timeout-type",
            "expected `str`, found `bool`",
        ),
        (
            "http-listener-accept-type",
            "expected `net.HttpListener`, found `bool`",
        ),
        (
            "http-listener-local-addr-type",
            "expected `net.HttpListener`, found `bool`",
        ),
        (
            "http-listener-close-type",
            "expected `net.HttpListener`, found `bool`",
        ),
        (
            "http-exchange-method-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-path-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-headers-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-body-text-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-body-bytes-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-respond-text-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-exchange-respond-bytes-type",
            "expected `net.HttpExchange`, found `bool`",
        ),
        (
            "http-response-status-type",
            "expected `net.HttpResponse`, found `bool`",
        ),
        (
            "http-response-reason-type",
            "expected `net.HttpResponse`, found `bool`",
        ),
        (
            "http-response-headers-type",
            "expected `net.HttpResponse`, found `bool`",
        ),
        (
            "http-response-text-type",
            "expected `net.HttpResponse`, found `bool`",
        ),
        (
            "http-response-bytes-type",
            "expected `net.HttpResponse`, found `bool`",
        ),
        (
            "websocket-listener-accept-type",
            "expected `net.WebSocketListener`, found `bool`",
        ),
        (
            "websocket-listener-local-addr-type",
            "expected `net.WebSocketListener`, found `bool`",
        ),
        (
            "websocket-send-text-type",
            "expected `net.WebSocket`, found `bool`",
        ),
        (
            "websocket-send-bytes-type",
            "expected `net.WebSocket`, found `bool`",
        ),
        (
            "websocket-recv-text-type",
            "expected `net.WebSocket`, found `bool`",
        ),
        (
            "websocket-recv-bytes-type",
            "expected `net.WebSocket`, found `bool`",
        ),
        (
            "websocket-close-type",
            "expected `net.WebSocket`, found `bool`",
        ),
        (
            "unix-listener-accept-type",
            "expected `net.UnixListener`, found `bool`",
        ),
        (
            "unix-listener-close-type",
            "expected `net.UnixListener`, found `bool`",
        ),
        (
            "unix-stream-read-line-type",
            "expected `net.UnixStream`, found `bool`",
        ),
        (
            "unix-stream-read-exact-count-type",
            "`read_exact(...)` expects `int32`, found `bool`",
        ),
        (
            "unix-stream-read-exact-negative-count",
            "`read_exact(...)` requires a non-negative count",
        ),
        (
            "unix-stream-read-exact-type",
            "expected `net.UnixStream`, found `bool`",
        ),
        (
            "unix-stream-write-all-text-type",
            "`write_all(...)` expects `str`, found `bool`",
        ),
        (
            "unix-stream-write-all-type",
            "expected `net.UnixStream`, found `bool`",
        ),
        (
            "unix-stream-close-type",
            "expected `net.UnixStream`, found `bool`",
        ),
        (
            "tls-listener-accept-type",
            "expected `net.TlsListener`, found `bool`",
        ),
        (
            "tls-listener-local-addr-type",
            "expected `net.TlsListener`, found `bool`",
        ),
        (
            "tls-listener-close-type",
            "expected `net.TlsListener`, found `bool`",
        ),
        (
            "tls-stream-read-line-type",
            "expected `net.TlsStream`, found `bool`",
        ),
        (
            "tls-stream-read-exact-count-type",
            "`read_exact(...)` expects `int32`, found `bool`",
        ),
        (
            "tls-stream-read-exact-negative-count",
            "`read_exact(...)` requires a non-negative count",
        ),
        (
            "tls-stream-read-exact-type",
            "expected `net.TlsStream`, found `bool`",
        ),
        (
            "tls-stream-write-all-text-type",
            "`write_all(...)` expects `str`, found `bool`",
        ),
        (
            "tls-stream-write-all-type",
            "expected `net.TlsStream`, found `bool`",
        ),
        (
            "tls-stream-close-type",
            "expected `net.TlsStream`, found `bool`",
        ),
        ("sleep-ms-negative", "sleep duration must be non-negative"),
        ("sleep-value-negative", "sleep(...) must be non-negative"),
        ("fail-division-no-span", "division by zero"),
        (
            "fail-int32-overflow-no-span",
            "integer value `123` does not fit in `int32`",
        ),
        (
            "vec-index-oob-no-span",
            "list index `5` is out of bounds for length `1`",
        ),
        (
            "vec-set-oob-no-span",
            "list index `5` is out of bounds for length `1`",
        ),
        (
            "vec-set-oob-span",
            "list index `5` is out of bounds for length `1`",
        ),
        (
            "vec-method-set-too-negative",
            "list set index `-5` is out of bounds for length `4`",
        ),
        (
            "vec-method-remove-too-negative",
            "list remove index `-5` is out of bounds for length `4`",
        ),
        (
            "vec-method-swap-too-negative",
            "list swap indices `-5` and `-1` are out of bounds for length `4`",
        ),
        (
            "vec-indexed-write-too-negative",
            "list index `-5` is out of bounds for length `4`",
        ),
        (
            "unbox-i64-overflow",
            "direct backend expected an integer that fits in host i64",
        ),
        (
            "unbox-i64-type",
            "direct backend expected `int32`, found `bool`",
        ),
        (
            "unbox-int64-overflow",
            "integer value `9223372036854775808` does not fit in `int64`",
        ),
        (
            "unbox-int64-type",
            "direct backend expected `int64`, found `bool`",
        ),
        (
            "unbox-u64-negative",
            "direct backend expected an integer that fits in host u64",
        ),
        (
            "unbox-u64-overflow",
            "direct backend expected an integer that fits in host u64",
        ),
        (
            "unbox-u64-type",
            "direct backend expected `uint64`, found `bool`",
        ),
        (
            "unbox-f64-type",
            "direct backend expected `float64`, found `integer`",
        ),
        (
            "unbox-bool-type",
            "direct backend expected `bool`, found `integer`",
        ),
        (
            "condition-type",
            "direct backend cannot use `str` as a branch condition",
        ),
        ("unary-invalid-op", "unknown unary opcode `99`"),
        (
            "unary-at-no-span",
            "unary `-` expects a numeric value, found `str`",
        ),
        (
            "unary-at-span",
            "unary `-` expects a numeric value, found `str`",
        ),
        ("binary-invalid-op", "unknown binary opcode `99`"),
        ("binary-floor-zero-no-span", "division by zero"),
        (
            "binary-at-no-span",
            "unsupported `+` operands `str` and `bool`",
        ),
        (
            "binary-at-span",
            "unsupported `+` operands `str` and `bool`",
        ),
        (
            "cast-no-span",
            "casts are only supported between numeric types, found `str` and `int32`",
        ),
        (
            "cast-at-span",
            "casts are only supported between numeric types, found `str` and `int32`",
        ),
    ] {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg("native_runtime::tests::direct_runtime_helper_errors_surface_expected_diagnostics")
            .arg("--nocapture")
            .env("AURA_DIRECT_RUNTIME_CASE", case)
            .output()
            .expect("child test process should run");

        assert!(!output.status.success(), "helper case `{case}` should fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "helper case `{case}` stderr should mention `{expected}`"
        );
        if matches!(
            case,
            "select-container-type"
                | "select-source-type"
                | "select-metadata-arity"
                | "select-metadata-arity-plural"
                | "select-metadata-kind"
                | "select-metadata-queue-shape"
                | "select-metadata-queue-name"
                | "select-metadata-queue-payload"
                | "select-metadata-task-shape"
                | "select-metadata-task-arity"
                | "select-metadata-task-name"
                | "select-metadata-task-result"
                | "select-metadata-duration-kind"
                | "queue-send-timeout-negative"
                | "queue-recv-timeout-negative"
                | "queue-recv-or-none-timeout-negative"
                | "queue-recv-or-value-timeout-negative"
                | "wait-any-timeout-negative"
                | "wait-all-timeout-negative"
                | "task-result-timeout-negative"
                | "task-result-or-none-timeout-negative"
                | "task-result-or-timeout-negative"
                | "sleep-ms-negative"
                | "sleep-value-negative"
        ) {
            assert!(
                stderr.contains("error[AU4001]"),
                "helper case `{case}` should preserve the AU4001 runtime trap code"
            );
        }
    }
}

#[test]
fn native_runtime_scalar_helpers_cover_comparisons_unary_ops_and_metadata() {
    assert_eq!(render_bool(0), "false");
    assert_eq!(render_bool(9), "true");
    assert_eq!(
        int32_overflow_message(12),
        "integer value `12` does not fit in `int32`"
    );
    assert_eq!(runtime_span(3, 4), Some(crate::diag::Span::new(3, 4)));
    assert_eq!(runtime_span(0, 4), None);
    assert_eq!(normalize_vec_index(3, 5), Some(3));
    assert_eq!(normalize_vec_index(-1, 5), Some(4));
    assert_eq!(normalize_vec_index(-6, 5), None);

    assert_eq!(value_type_name(&Value::Bool(true)), "bool");
    assert_eq!(value_type_name(&Value::Unit), "None");
    assert_eq!(
        value_type_name(&Value::ModuleNamespace(ModuleNamespaceValue {
            path: "pkg.tools".to_string(),
        })),
        "module pkg.tools"
    );
    assert_eq!(
        value_type_name(&Value::Instance(InstanceValue {
            class_name: "Point".to_string(),
            fields: Default::default(),
        })),
        "Point"
    );
    assert_eq!(
        value_type_name(&Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        })),
        "Status"
    );
    assert_eq!(
        value_type_name(&Value::Int(IntegerValue::from_signed(1))),
        "integer"
    );
    assert_eq!(value_type_name(&Value::Float(1.5)), "float64");
    assert_eq!(
        value_type_name(&Value::Vec(VecValue {
            element_type: crate::sema::Type::named("int32"),
            elements: Vec::new(),
        })),
        "list"
    );
    let array = Value::Array(
        ArrayValue::zeros(ArrayDType::Int64, vec![2].into_boxed_slice())
            .expect("metadata Array allocation should succeed"),
    );
    assert_eq!(value_type_name(&array), "Array[int64]");
    assert_eq!(
        inferred_collection_type(&array),
        Type::Named("Array".to_string(), vec![Type::named("int64")])
    );
    assert_eq!(
        value_type_name(&Value::Set(SetValue {
            element_type: crate::sema::Type::named("str"),
            elements: Vec::new(),
        })),
        "set"
    );
    assert_eq!(
        value_type_name(&Value::Map(MapValue {
            key_type: crate::sema::Type::named("str"),
            value_type: crate::sema::Type::named("int32"),
            entries: Vec::new(),
        })),
        "dict"
    );
    assert_eq!(value_type_name(&Value::Duration(5)), "Duration");
    assert_value_metadata(
        &Value::Rng(RngValue::from_seed(42)),
        "random.Rng",
        "random.Rng",
    );
    assert_eq!(
        value_type_name(&Value::Range(RangeValue { start: 1, end: 2 })),
        "Range"
    );
    assert_eq!(
        value_type_name(&Value::Channel(ChannelValue::new())),
        "Queue"
    );
    assert_eq!(
        value_type_name(&Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(
            Value::Unit
        ))))),
        "Task"
    );
    assert_eq!(
        value_type_name(&Value::TaskGroup(TaskGroupValue::new(
            &CancellationContext::default()
        ))),
        "TaskGroup"
    );
    assert_eq!(
        inferred_collection_type(&Value::Bool(true)),
        crate::sema::Type::named("bool")
    );
    assert_eq!(
        inferred_collection_type(&Value::Float(1.5)),
        crate::sema::Type::named("float64")
    );
    assert_eq!(
        inferred_collection_type(&Value::Int(IntegerValue::from_i32(7))),
        crate::sema::Type::named("int32")
    );
    assert_eq!(
        inferred_collection_type(&Value::Int(IntegerValue::from_signed(7))),
        crate::sema::Type::named("Unknown")
    );
    assert_eq!(
        inferred_collection_type(&Value::String("text".to_string())),
        crate::sema::Type::named("str")
    );
    assert_eq!(
        inferred_collection_type(&Value::Vec(VecValue {
            element_type: crate::sema::Type::named("int32"),
            elements: Vec::new(),
        })),
        crate::sema::Type::Named("list".to_string(), vec![crate::sema::Type::named("int32")])
    );
    assert_eq!(
        inferred_collection_type(&Value::Set(SetValue {
            element_type: crate::sema::Type::named("str"),
            elements: Vec::new(),
        })),
        crate::sema::Type::Named("set".to_string(), vec![crate::sema::Type::named("str")])
    );
    assert_eq!(
        inferred_collection_type(&Value::Map(MapValue {
            key_type: crate::sema::Type::named("str"),
            value_type: crate::sema::Type::named("int32"),
            entries: Vec::new(),
        })),
        crate::sema::Type::Named(
            "dict".to_string(),
            vec![
                crate::sema::Type::named("str"),
                crate::sema::Type::named("int32"),
            ],
        )
    );
    assert_eq!(
        inferred_collection_type(&Value::Duration(5)),
        crate::sema::Type::named("Duration")
    );
    assert_eq!(
        inferred_collection_type(&Value::Range(RangeValue { start: 1, end: 2 })),
        crate::sema::Type::named("Range")
    );
    assert_eq!(
        inferred_collection_type(&Value::Instance(InstanceValue {
            class_name: "Point".to_string(),
            fields: Default::default(),
        })),
        crate::sema::Type::named("Point")
    );
    assert_eq!(
        inferred_collection_type(&Value::Channel(ChannelValue::new())),
        crate::sema::Type::named("Queue")
    );
    assert_eq!(
        inferred_collection_type(&Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(
            Value::Unit
        ))))),
        crate::sema::Type::named("Task")
    );
    assert_eq!(
        inferred_collection_type(&Value::TaskGroup(TaskGroupValue::new(
            &CancellationContext::default()
        ))),
        crate::sema::Type::named("TaskGroup")
    );

    assert_eq!(
        compare_values(
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(2)),
            BinaryOp::Less,
        )
        .expect("int comparison should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(i128::MIN)),
            Value::Int(IntegerValue::from_literal(1)),
            BinaryOp::Sub,
        )
        .expect_err("subtraction beyond the signed integer range should fail")
        .message,
        "integer overflow"
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_literal(u128::MAX)),
            Value::Int(IntegerValue::from_literal(2)),
            BinaryOp::Mul,
        )
        .expect_err("unsigned multiplication beyond u128 should fail")
        .message,
        "integer overflow"
    );
    assert_eq!(
        eval_binary_value(Value::Float(1.0), Value::Float(0.0), BinaryOp::Mod)
            .expect_err("float modulo by zero should fail")
            .message,
        "division by zero"
    );
    assert_eq!(
        compare_values(
            Value::String("b".to_string()),
            Value::String("a".to_string()),
            BinaryOp::Greater,
        )
        .expect("string comparison should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(Value::Float(1.5), Value::Float(1.5), BinaryOp::LessEq)
            .expect("float comparison should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            BinaryOp::Less,
        )
        .expect("string less-than should work"),
        Value::Bool(true)
    );
    let compare_error = compare_values(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Less,
    )
    .expect_err("unsupported comparison should fail");
    assert!(compare_error.message.contains("unsupported comparison"));
    assert_eq!(
        compare_values(
            Value::Vec(VecValue {
                element_type: crate::sema::Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            }),
            Value::Vec(VecValue {
                element_type: crate::sema::Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            }),
            BinaryOp::Eq,
        )
        .expect("equality should work for runtime values"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(Value::Bool(true), Value::Bool(false), BinaryOp::NotEq,)
            .expect("inequality should work for runtime values"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::Int(IntegerValue::from_signed(2)),
            Value::Int(IntegerValue::from_signed(2)),
            BinaryOp::LessEq,
        )
        .expect("int less-equal should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::Int(IntegerValue::from_signed(3)),
            Value::Int(IntegerValue::from_signed(2)),
            BinaryOp::Greater,
        )
        .expect("int greater-than should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::Int(IntegerValue::from_signed(3)),
            Value::Int(IntegerValue::from_signed(3)),
            BinaryOp::GreaterEq,
        )
        .expect("int greater-equal should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(Value::Float(1.5), Value::Float(2.5), BinaryOp::Less,)
            .expect("float less-than should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(Value::Float(3.5), Value::Float(2.5), BinaryOp::Greater,)
            .expect("float greater-than should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::String("a".to_string()),
            Value::String("a".to_string()),
            BinaryOp::LessEq,
        )
        .expect("string less-equal should work"),
        Value::Bool(true)
    );
    assert_eq!(
        compare_values(
            Value::String("b".to_string()),
            Value::String("a".to_string()),
            BinaryOp::GreaterEq,
        )
        .expect("string greater-equal should work"),
        Value::Bool(true)
    );
    let int_operator_error = compare_values(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::from_signed(2)),
        BinaryOp::Add,
    )
    .expect_err("unsupported int comparison operators should fail");
    assert!(int_operator_error
        .message
        .contains("unsupported comparison operator"));
    let float_operator_error = compare_values(Value::Float(1.0), Value::Float(2.0), BinaryOp::Add)
        .expect_err("unsupported float comparison operators should fail");
    assert!(float_operator_error
        .message
        .contains("unsupported comparison operator"));
    let string_operator_error = compare_values(
        Value::String("a".to_string()),
        Value::String("b".to_string()),
        BinaryOp::Add,
    )
    .expect_err("unsupported string comparison operators should fail");
    assert!(string_operator_error
        .message
        .contains("unsupported comparison operator"));

    assert_eq!(
        eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::And)
            .expect("logical and should work"),
        Value::Bool(false)
    );
    assert_eq!(
        eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::Or)
            .expect("logical or should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_binary_value(Value::Bool(false), Value::Bool(false), BinaryOp::Or)
            .expect("logical or should preserve false"),
        Value::Bool(false)
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(4)),
            Value::Int(IntegerValue::from_signed(5)),
            BinaryOp::Add,
        )
        .expect("int addition should work"),
        Value::Int(IntegerValue::from_signed(9))
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(9)),
            Value::Int(IntegerValue::from_signed(4)),
            BinaryOp::Sub,
        )
        .expect("int subtraction should work"),
        Value::Int(IntegerValue::from_signed(5))
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(3)),
            Value::Int(IntegerValue::from_signed(4)),
            BinaryOp::Mul,
        )
        .expect("int multiplication should work"),
        Value::Int(IntegerValue::from_signed(12))
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(9)),
            Value::Int(IntegerValue::from_signed(3)),
            BinaryOp::Div,
        )
        .expect("int division should work"),
        Value::Int(IntegerValue::from_signed(3))
    );
    assert_eq!(
        eval_binary_value(
            Value::Int(IntegerValue::from_signed(9)),
            Value::Int(IntegerValue::from_signed(4)),
            BinaryOp::Mod,
        )
        .expect("int modulo should work"),
        Value::Int(IntegerValue::from_signed(1))
    );
    assert_eq!(
        eval_binary_value(Value::Float(9.0), Value::Float(2.0), BinaryOp::Div)
            .expect("float division should work"),
        Value::Float(4.5)
    );
    assert_eq!(
        eval_binary_value(Value::Float(9.0), Value::Float(2.0), BinaryOp::Add)
            .expect("float addition should work"),
        Value::Float(11.0)
    );
    assert_eq!(
        eval_binary_value(Value::Float(9.0), Value::Float(2.0), BinaryOp::Sub)
            .expect("float subtraction should work"),
        Value::Float(7.0)
    );
    assert_eq!(
        eval_binary_value(Value::Float(9.0), Value::Float(2.0), BinaryOp::Mul)
            .expect("float multiplication should work"),
        Value::Float(18.0)
    );
    assert_eq!(
        eval_binary_value(Value::Float(9.0), Value::Float(4.0), BinaryOp::Mod)
            .expect("float modulo should work"),
        Value::Float(1.0)
    );
    assert_eq!(
        eval_binary_value(
            Value::String("au".to_string()),
            Value::String("ra".to_string()),
            BinaryOp::Add,
        )
        .expect("string concatenation should work"),
        Value::String("aura".to_string())
    );
    let add_error = eval_binary_value(
        Value::String("a".to_string()),
        Value::Bool(true),
        BinaryOp::Add,
    )
    .expect_err("unsupported add should fail");
    assert!(add_error.message.contains("unsupported `+` operands"));
    let and_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::And,
    )
    .expect_err("logical and should reject non-bools");
    assert!(and_error
        .message
        .contains("logical `and` expects bool operands"));
    let or_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Or,
    )
    .expect_err("logical or should reject non-bools");
    assert!(or_error
        .message
        .contains("logical `or` expects bool operands"));
    let div_zero = eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::zero()),
        BinaryOp::Div,
    )
    .expect_err("division by zero should fail");
    assert_eq!(div_zero.message, "division by zero");
    let mod_zero = eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::zero()),
        BinaryOp::Mod,
    )
    .expect_err("modulo by zero should fail");
    assert_eq!(mod_zero.message, "division by zero");
    let sub_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Sub,
    )
    .expect_err("invalid subtraction should fail");
    assert!(sub_error.message.contains("unsupported `-` operands"));
    let mul_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Mul,
    )
    .expect_err("invalid multiplication should fail");
    assert!(mul_error.message.contains("unsupported `*` operands"));
    let div_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Div,
    )
    .expect_err("invalid division should fail");
    assert!(div_error.message.contains("unsupported `/` operands"));
    let mod_error = eval_binary_value(
        Value::Bool(true),
        Value::Int(IntegerValue::from_signed(1)),
        BinaryOp::Mod,
    )
    .expect_err("invalid modulo should fail");
    assert!(mod_error.message.contains("unsupported `%` operands"));

    assert_eq!(
        eval_unary_value(Value::Bool(false), UnaryOp::Not).expect("not should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_unary_value(Value::Int(IntegerValue::from_signed(2)), UnaryOp::Neg)
            .expect("integer negation should work"),
        Value::Int(IntegerValue::from_signed(-2))
    );
    assert_eq!(
        eval_unary_value(Value::Float(2.5), UnaryOp::Neg).expect("neg should work"),
        Value::Float(-2.5)
    );
    let not_error = eval_unary_value(Value::Int(IntegerValue::from_signed(1)), UnaryOp::Not)
        .expect_err("invalid unary not should fail");
    assert!(not_error.message.contains("expects `bool`"));
    let unary_error = eval_unary_value(Value::String("x".to_string()), UnaryOp::Neg)
        .expect_err("invalid unary neg should fail");
    assert!(unary_error.message.contains("expects a numeric value"));

    let module_value = super::boxed_value(Value::ModuleNamespace(ModuleNamespaceValue {
        path: "pkg.tools".to_string(),
    }));
    let instance_value = super::boxed_value(Value::Instance(InstanceValue {
        class_name: "Point".to_string(),
        fields: Default::default(),
    }));
    let enum_value = super::boxed_value(Value::EnumVariant(EnumVariantValue {
        enum_name: "Status".to_string(),
        variant_name: "Ready".to_string(),
        payloads: Vec::new(),
    }));
    let unit_value = super::boxed_value(Value::Unit);

    let int64_value = int_value(7);
    assert_eq!(
        super::aura_direct_value_type_matches(int64_value, b"int64".as_ptr(), "int64".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(int64_value, b"int32".as_ptr(), "int32".len(),),
        0
    );
    assert_eq!(
        super::aura_direct_value_type_matches(int64_value, b"uint64".as_ptr(), "uint64".len(),),
        0
    );
    let int32_value = super::aura_direct_box_i32(7);
    assert_eq!(
        super::aura_direct_value_type_matches(int32_value, b"int32".as_ptr(), "int32".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(int32_value, b"int64".as_ptr(), "int64".len(),),
        0
    );
    let uint64_value = super::aura_direct_box_u64(7);
    assert_eq!(
        super::aura_direct_value_type_matches(uint64_value, b"uint64".as_ptr(), "uint64".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(uint64_value, b"int64".as_ptr(), "int64".len(),),
        0
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            float_value(3.5),
            b"float32".as_ptr(),
            "float32".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(unit_value, b"None".as_ptr(), "None".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            module_value,
            b"module pkg.tools".as_ptr(),
            "module pkg.tools".len(),
        ),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(instance_value, b"Point".as_ptr(), "Point".len(),),
        1
    );
    assert_eq!(
        super::aura_direct_value_type_matches(enum_value, b"Status".as_ptr(), "Status".len(),),
        1
    );
}

#[test]
fn native_runtime_resource_metadata_reports_maintained_type_names() {
    let mut file_path = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    file_path.push(format!(
        "aura-native-resource-metadata-{}-{timestamp}.txt",
        std::process::id()
    ));
    let file = FileValue::create(file_path.to_str().expect("temp path should be valid UTF-8"))
        .expect("temp file should be created");
    assert_value_metadata(&Value::File(file.clone()), "fs.File", "fs.File");
    assert_direct_type_match(Value::File(file.clone()), "fs.File");
    close_via_direct(Value::File(file.clone()));
    let _ = std::fs::remove_file(&file_path);

    let tcp_listener =
        TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind locally");
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
    assert_value_metadata(
        &Value::TcpListener(tcp_listener.clone()),
        "net.TcpListener",
        "net.TcpListener",
    );
    assert_direct_type_match(Value::TcpListener(tcp_listener.clone()), "net.TcpListener");
    assert_value_metadata(
        &Value::TcpStream(tcp_stream.clone()),
        "net.TcpStream",
        "net.TcpStream",
    );
    assert_direct_type_match(Value::TcpStream(tcp_stream.clone()), "net.TcpStream");
    close_via_direct(Value::TcpStream(tcp_stream.clone()));
    close_via_direct(Value::TcpStream(accepted_stream.clone()));
    close_via_direct(Value::TcpListener(tcp_listener.clone()));

    let udp_socket = UdpSocketValue::bind("127.0.0.1:0").expect("udp socket should bind locally");
    assert_value_metadata(
        &Value::UdpSocket(udp_socket.clone()),
        "net.UdpSocket",
        "net.UdpSocket",
    );
    assert_direct_type_match(Value::UdpSocket(udp_socket.clone()), "net.UdpSocket");
    close_via_direct(Value::UdpSocket(udp_socket.clone()));
    let udp_datagram = UdpDatagramValue {
        address: "127.0.0.1:9".to_string(),
        data: vec![1, 2, 3],
    };
    assert_value_metadata(
        &Value::UdpDatagram(udp_datagram.clone()),
        "net.UdpDatagram",
        "net.UdpDatagram",
    );
    assert_direct_type_match(Value::UdpDatagram(udp_datagram), "net.UdpDatagram");

    #[cfg(unix)]
    {
        let mut socket_path = std::path::PathBuf::from("/tmp");
        socket_path.push(format!(
            "aura-nrm-{}-{}.sock",
            std::process::id(),
            timestamp % 1_000_000
        ));
        let _ = std::fs::remove_file(&socket_path);
        let unix_listener = UnixListenerValue::bind(
            socket_path
                .to_str()
                .expect("unix socket path should be valid UTF-8"),
        )
        .expect("unix listener should bind locally");
        assert_value_metadata(
            &Value::UnixListener(unix_listener.clone()),
            "net.UnixListener",
            "net.UnixListener",
        );
        assert_direct_type_match(
            Value::UnixListener(unix_listener.clone()),
            "net.UnixListener",
        );
        let accept_listener = unix_listener.clone();
        let unix_accept_thread = thread::spawn(move || {
            accept_listener
                .accept(Some(StdDuration::from_secs(1)), None)
                .expect("unix listener should accept local client")
        });
        let unix_stream = UnixStreamValue::connect(
            socket_path
                .to_str()
                .expect("unix socket path should be valid UTF-8"),
            Some(StdDuration::from_secs(1)),
            None,
        )
        .expect("unix stream should connect locally");
        let accepted_unix_stream = unix_accept_thread
            .join()
            .expect("unix accept worker should join successfully");
        assert_value_metadata(
            &Value::UnixStream(unix_stream.clone()),
            "net.UnixStream",
            "net.UnixStream",
        );
        assert_direct_type_match(Value::UnixStream(unix_stream.clone()), "net.UnixStream");
        assert_direct_type_match(
            Value::UnixStream(accepted_unix_stream.clone()),
            "net.UnixStream",
        );
        close_via_direct(Value::UnixStream(unix_stream.clone()));
        close_via_direct(Value::UnixStream(accepted_unix_stream.clone()));
        close_via_direct(Value::UnixListener(unix_listener.clone()));
        let _ = std::fs::remove_file(&socket_path);
    }

    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = std::env::temp_dir().join(format!(
        "aura-native-resource-metadata-{}-{timestamp}.cert.pem",
        std::process::id()
    ));
    let key_path = std::env::temp_dir().join(format!(
        "aura-native-resource-metadata-{}-{timestamp}.key.pem",
        std::process::id()
    ));
    std::fs::write(&cert_path, certificate.cert.pem().as_bytes()).expect("write cert pem");
    std::fs::write(&key_path, certificate.key_pair.serialize_pem().as_bytes())
        .expect("write key pem");
    let tls_listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("cert path should be valid UTF-8"),
        key_path.to_str().expect("key path should be valid UTF-8"),
    )
    .expect("tls listener should bind locally");
    let tls_address = tls_listener
        .local_addr()
        .expect("tls listener should expose a local address");
    assert_value_metadata(
        &Value::TlsListener(tls_listener.clone()),
        "net.TlsListener",
        "net.TlsListener",
    );
    assert_direct_type_match(Value::TlsListener(tls_listener.clone()), "net.TlsListener");
    let accept_listener = tls_listener.clone();
    let tls_accept_thread = thread::spawn(move || {
        accept_listener
            .accept(Some(StdDuration::from_secs(1)), None)
            .expect("tls listener should accept local client")
    });
    let tls_stream = TlsStreamValue::connect(
        &tls_address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be valid UTF-8")),
        Some(StdDuration::from_secs(1)),
        None,
    )
    .expect("tls stream should connect locally");
    let accepted_tls_stream = tls_accept_thread
        .join()
        .expect("tls accept worker should join successfully");
    assert_value_metadata(
        &Value::TlsStream(tls_stream.clone()),
        "net.TlsStream",
        "net.TlsStream",
    );
    assert_direct_type_match(Value::TlsStream(tls_stream.clone()), "net.TlsStream");
    assert_direct_type_match(
        Value::TlsStream(accepted_tls_stream.clone()),
        "net.TlsStream",
    );
    let tls_client_ptr = boxed_value(Value::TlsStream(tls_stream.clone()));
    let tls_server_ptr = boxed_value(Value::TlsStream(accepted_tls_stream.clone()));
    expect_result_ok_unit(super::aura_direct_tls_stream_write_all(
        tls_client_ptr,
        string_value("hello tls\n"),
        duration_value(5_000),
    ));
    let tls_line = expect_option_some_payload(expect_result_ok_payload(
        super::aura_direct_tls_stream_read_line(tls_server_ptr, duration_value(5_000)),
    ));
    assert_eq!(tls_line, Value::String("hello tls".to_string()));
    expect_unit(super::aura_direct_tls_stream_close(tls_client_ptr));
    expect_unit(super::aura_direct_tls_stream_close(tls_server_ptr));
    unsafe {
        release_value(tls_client_ptr);
        release_value(tls_server_ptr);
    }
    close_via_direct(Value::TlsListener(tls_listener.clone()));
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_file(&key_path);

    let http_listener =
        HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind locally");
    let http_listener_address = http_listener
        .local_addr()
        .expect("http listener should expose a local address");
    assert_value_metadata(
        &Value::HttpListener(http_listener.clone()),
        "net.HttpListener",
        "net.HttpListener",
    );
    assert_direct_type_match(
        Value::HttpListener(http_listener.clone()),
        "net.HttpListener",
    );
    let accept_listener = http_listener.clone();
    let http_exchange_thread = thread::spawn(move || {
        let exchange = accept_listener
            .accept(Some(StdDuration::from_secs(1)), None)
            .expect("http listener should accept local client");
        assert_value_metadata(
            &Value::HttpExchange(exchange.clone()),
            "net.HttpExchange",
            "net.HttpExchange",
        );
        assert_direct_type_match(Value::HttpExchange(exchange.clone()), "net.HttpExchange");
        exchange
            .respond_text(204, "", Vec::new())
            .expect("http exchange should respond");
    });
    let mut http_client = std::net::TcpStream::connect(&http_listener_address)
        .expect("http metadata client should connect");
    http_client
        .write_all(b"GET /metadata HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
        .expect("http metadata client should write request");
    let mut http_response_bytes = Vec::new();
    http_client
        .read_to_end(&mut http_response_bytes)
        .expect("http metadata client should read response");
    assert!(
        http_response_bytes.starts_with(b"HTTP/1.1 204"),
        "http metadata response should report 204"
    );
    http_exchange_thread
        .join()
        .expect("http exchange worker should join successfully");
    close_via_direct(Value::HttpListener(http_listener.clone()));

    let http_server =
        std::net::TcpListener::bind("127.0.0.1:0").expect("http fixture should bind locally");
    let http_address = http_server
        .local_addr()
        .expect("http fixture should expose a local address");
    let http_thread = thread::spawn(move || {
        let (mut stream, _) = http_server
            .accept()
            .expect("http fixture should accept one request");
        let mut request = [0_u8; 512];
        let _ = stream
            .read(&mut request)
            .expect("http fixture should read request bytes");
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .expect("http fixture should write response");
    });
    let http_response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{http_address}/"),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(1)),
        None,
    )
    .expect("http response should be read from local fixture");
    http_thread
        .join()
        .expect("http fixture worker should join successfully");
    assert_value_metadata(
        &Value::HttpResponse(http_response.clone()),
        "net.HttpResponse",
        "net.HttpResponse",
    );
    assert_direct_type_match(Value::HttpResponse(http_response), "net.HttpResponse");

    let ws_listener =
        WebSocketListenerValue::bind("127.0.0.1:0").expect("websocket listener should bind");
    let ws_address = ws_listener
        .local_addr()
        .expect("websocket listener should expose a local address");
    assert_value_metadata(
        &Value::WebSocketListener(ws_listener.clone()),
        "net.WebSocketListener",
        "net.WebSocketListener",
    );
    assert_direct_type_match(
        Value::WebSocketListener(ws_listener.clone()),
        "net.WebSocketListener",
    );
    let accept_listener = ws_listener.clone();
    let ws_accept_thread = thread::spawn(move || {
        accept_listener
            .accept(Some(StdDuration::from_secs(1)))
            .expect("websocket listener should accept local client")
    });
    let ws_client = WebSocketValue::connect(
        &format!("ws://{ws_address}"),
        Some(StdDuration::from_secs(1)),
    )
    .expect("websocket client should connect locally");
    let ws_server = ws_accept_thread
        .join()
        .expect("websocket accept worker should join successfully");
    assert_value_metadata(
        &Value::WebSocket(ws_client.clone()),
        "net.WebSocket",
        "net.WebSocket",
    );
    assert_direct_type_match(Value::WebSocket(ws_client.clone()), "net.WebSocket");
    assert_direct_type_match(Value::WebSocket(ws_server.clone()), "net.WebSocket");
    close_via_direct(Value::WebSocket(ws_client.clone()));
    close_via_direct(Value::WebSocket(ws_server.clone()));

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
    assert_value_metadata(
        &Value::ProcessChild(child.clone()),
        "process.Child",
        "process.Child",
    );
    assert_direct_type_match(Value::ProcessChild(child.clone()), "process.Child");
    assert_value_metadata(
        &Value::ProcessPipe(stdout_pipe.clone()),
        "process.Pipe",
        "process.Pipe",
    );
    assert_direct_type_match(Value::ProcessPipe(stdout_pipe.clone()), "process.Pipe");
    close_via_direct(Value::ProcessPipe(stdout_pipe));
    let _ = child.wait(Some(StdDuration::from_secs(1)), None);
    close_via_direct(Value::ProcessChild(child.clone()));

    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        Vec::new(),
        Vec::new(),
    );
    assert_value_metadata(
        &Value::ProcessCompleted(completed.clone()),
        "process.Completed",
        "process.Completed",
    );
    assert_direct_type_match(Value::ProcessCompleted(completed), "process.Completed");
    let supervisor = ProcessSupervisorValue::new();
    assert_value_metadata(
        &Value::ProcessSupervisor(supervisor.clone()),
        "process.Supervisor",
        "process.Supervisor",
    );
    assert_direct_type_match(
        Value::ProcessSupervisor(supervisor.clone()),
        "process.Supervisor",
    );
    close_via_direct(Value::ProcessSupervisor(supervisor));
}

#[test]
fn native_runtime_thread_local_and_pointer_helpers_cover_remaining_paths() {
    assert!(!current_cancellation().is_cancelled());
    let group = TaskGroupValue::new(&crate::runtime_value::CancellationContext::default());
    let child = group.child_cancellation();
    group.cancel();
    let scoped = with_cancellation_scope(child, || current_cancellation().is_cancelled());
    assert!(scoped);
    assert!(!current_cancellation().is_cancelled());

    assert!(super::take_direct_cleanup_registration(42).is_none());
    super::with_direct_task_runtime_state(|state| {
        state.cleanup_draining = true;
    });
    super::drain_direct_cleanup_stack();
    assert!(super::direct_cleanup_is_draining());
    super::with_direct_task_runtime_state(|state| state.cleanup_draining = false);

    super::with_direct_task_runtime_state(|state| state.next_cleanup_id = i64::MAX);
    let max_registration_id = super::push_direct_cleanup_registration(11, std::ptr::null_mut(), 0);
    assert_eq!(max_registration_id, i64::MAX);
    assert!(super::take_direct_cleanup_registration(max_registration_id).is_some());
    super::with_direct_task_runtime_state(|state| {
        assert_eq!(state.next_cleanup_id, 1);
        state.next_cleanup_id = -1;
    });
    let negative_registration_id =
        super::push_direct_cleanup_registration(12, std::ptr::null_mut(), 0);
    assert_eq!(negative_registration_id, -1);
    assert!(super::take_direct_cleanup_registration(negative_registration_id).is_some());
    super::with_direct_task_runtime_state(|state| {
        assert_eq!(state.next_cleanup_id, 1);
        state.next_cleanup_id = 1;
    });
    let cleanup_id = super::aura_direct_register_cleanup(1, std::ptr::null_mut(), 0);
    assert!(cleanup_id > 0);
    super::aura_direct_unregister_cleanup(cleanup_id);
    super::aura_direct_unregister_cleanup(cleanup_id);

    let inactive_cleanup_id = super::aura_direct_register_cleanup(1, std::ptr::null_mut(), 0);
    assert_eq!(
        super::aura_direct_refresh_cleanup(0, inactive_cleanup_id, 1, std::ptr::null_mut(), 0),
        0
    );

    let replaced_cleanup_id = super::aura_direct_register_cleanup(1, std::ptr::null_mut(), 0);
    let refreshed_cleanup_id =
        super::aura_direct_refresh_cleanup(1, replaced_cleanup_id, 1, std::ptr::null_mut(), 0);
    assert!(refreshed_cleanup_id > 0);
    super::aura_direct_unregister_cleanup(refreshed_cleanup_id);

    let new_cleanup_id = super::aura_direct_refresh_cleanup(1, 0, 1, std::ptr::null_mut(), 0);
    assert!(new_cleanup_id > 0);
    super::aura_direct_unregister_cleanup(new_cleanup_id);

    let primary = super::DirectPrimaryDiagnosticGuard::install(Diagnostic::new("primary"));
    let nested = super::DirectPrimaryDiagnosticGuard::install(Diagnostic::new("nested"));
    assert!(primary.installed);
    assert!(!nested.installed);
    assert_eq!(
        super::direct_primary_runtime_diagnostic()
            .expect("primary diagnostic should be installed")
            .message,
        "primary"
    );
    drop(nested);
    assert!(super::direct_primary_runtime_diagnostic().is_some());
    drop(primary);
    assert!(super::direct_primary_runtime_diagnostic().is_none());

    assert_eq!(
        extract_duration_nanoseconds(&Value::Int(IntegerValue::from_signed(7))),
        7
    );
    assert_eq!(extract_duration_nanoseconds(&Value::Duration(9)), 9);
    assert_eq!(decode_bytes(b"aura".as_ptr(), "aura".len()), "aura");

    unsafe {
        super::aura_direct_enter_call(0, 0, b"covered".as_ptr(), b"covered".len());
        super::aura_direct_enter_call(0, 0, b"covered".as_ptr(), b"covered".len());
        super::aura_direct_exit_call();
        super::aura_direct_exit_call();
        super::aura_direct_exit_call();
    }

    let boxed = boxed_value(Value::Int(IntegerValue::from_signed(5)));
    assert_eq!(
        unsafe { value_ref(boxed) },
        Value::Int(IntegerValue::from_signed(5))
    );
    unsafe {
        value_mut(boxed, |value| match value {
            Value::Int(inner) => *inner = IntegerValue::from_signed(8),
            other => panic!("expected int box, found {:?}", other),
        })
    };
    assert_eq!(expect_int(boxed), 8);

    let vec = super::aura_direct_vec_empty();
    expect_unit(super::aura_direct_vec_push_in_place(vec, string_value("x")));
    assert_eq!(super::with_vector(vec, |vector| vector.elements.len()), 1);
    super::with_vector_mut(vec, |vector| {
        vector.elements.push(Value::String("y".to_string()));
    });
    assert_eq!(
        expect_vec_strings(super::aura_direct_clone_value(vec)),
        vec!["x".to_string(), "y".to_string()]
    );

    let map = super::aura_direct_map_empty();
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("name"),
        int_value(1),
    ));
    assert_eq!(super::with_map(map, |map| map.entries.len()), 1);
    super::with_map_mut(map, |map| {
        map.entries.push((
            Value::String("other".to_string()),
            Value::Int(IntegerValue::from_signed(2)),
        ));
    });
    assert_eq!(
        expect_vec_ints(super::aura_direct_map_values(map)),
        vec![1, 2]
    );

    let set = super::aura_direct_set_empty();
    assert_eq!(
        super::aura_direct_set_insert_in_place(set, string_value("ready")),
        1
    );
    assert_eq!(super::with_set(set, |set| set.elements.len()), 1);
    super::with_set_mut(set, |set| {
        set.elements.push(Value::String("go".to_string()));
    });
    assert_eq!(super::aura_direct_set_len(set), 2);

    assert_eq!(runtime_span(0, 1), None);
    assert_eq!(runtime_span(1, 0), None);
    assert_eq!(runtime_span(2, 3), Some(crate::diag::Span::new(2, 3)));

    assert_eq!(value_type_name(&Value::Unit), "None");
    assert_eq!(
        value_type_name(&Value::ModuleNamespace(ModuleNamespaceValue {
            path: "pkg.tools".to_string(),
        })),
        "module pkg.tools"
    );
    assert_eq!(
        value_type_name(&Value::Instance(InstanceValue {
            class_name: "Counter".to_string(),
            fields: BTreeMap::new(),
        })),
        "Counter"
    );
    assert_eq!(
        value_type_name(&Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        })),
        "Status"
    );
    assert_eq!(
        value_type_name(&Value::Channel(ChannelValue::new())),
        "Queue"
    );
    assert_eq!(
        value_type_name(&Value::Task(TaskValue::from_handle(thread::spawn(|| Ok(
            Value::Unit
        ))))),
        "Task"
    );
    assert_eq!(
        value_type_name(&Value::TaskGroup(TaskGroupValue::new(
            &CancellationContext::default()
        ))),
        "TaskGroup"
    );

    let plain = render_runtime_diagnostic(Diagnostic::new("plain failure"));
    assert!(plain.contains("error[AU2999]: plain failure"));

    let rendered = render_runtime_diagnostic(Diagnostic::at(Span::new(1, 1), "annotated"));
    assert!(rendered.contains("error[AU2999]: annotated"));
}

#[test]
fn native_runtime_task_boundary_maps_task_signals_and_resumes_unrelated_panics() {
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let plain_panic = panic::catch_unwind(|| {
        super::task_runtime_boundary(|| {
            panic!("plain boundary panic");
        });
    });
    panic::set_hook(previous_hook);
    assert!(plain_panic.is_err());

    let result = run_lightweight_root_task(|| {
        let cancelled_task = spawn_lightweight_task(|| {
            super::task_runtime_boundary(|| -> std::result::Result<Value, Diagnostic> {
                std::panic::panic_any(TaskCancelledSignal);
            })
        })?;
        match cancelled_task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Cancelled => {}
            other => panic!("expected cancelled direct-runtime task, got {other:?}"),
        }

        let failed_task = spawn_lightweight_task(|| {
            super::task_runtime_boundary(|| -> std::result::Result<Value, Diagnostic> {
                std::panic::panic_any(LightweightTaskFailureSignal(Diagnostic::new(
                    "boundary failure",
                )));
            })
        })?;
        match failed_task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Err(error)) => assert_eq!(error.message, "boundary failure"),
            other => panic!("expected failed direct-runtime task, got {other:?}"),
        }

        Ok(Value::Unit)
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
}

#[test]
fn native_runtime_direct_call_depth_is_isolated_across_suspended_tasks() {
    const TASK_COUNT: usize = 1_000;

    let result = run_lightweight_root_task(|| {
        let ready = ChannelValue::new();
        let release = ChannelValue::new();
        let mut tasks = Vec::with_capacity(TASK_COUNT);

        for index in 0..TASK_COUNT {
            let task_ready = ready.clone();
            let task_release = release.clone();
            tasks.push(spawn_lightweight_task(move || {
                super::with_direct_task_runtime_scope(|| {
                    Ok(super::with_task_runtime_error_capture(|| {
                        task_ready
                            .send(Value::Unit)
                            .expect("ready channel should remain open");
                        let function = format!("suspended_{index}");
                        let path = format!("/workspace/task_{index}.au");
                        unsafe {
                            super::aura_direct_enter_call_with_frame(
                                index as i64 + 1,
                                1,
                                path.as_ptr(),
                                path.len(),
                                function.as_ptr(),
                                function.len(),
                            );
                        }
                        let _ = task_release.recv_with_cancellation(None, None);
                        assert_eq!(
                            super::direct_runtime_call_frames(),
                            vec![RuntimeCallFrame {
                                function,
                                span: runtime_source_span(&path, index + 1, 1),
                            }],
                            "a pinned worker must restore frame state by task identity after suspension"
                        );
                        unsafe {
                            super::aura_direct_exit_call();
                        }
                        Value::Unit
                    }))
                })
            })?);
        }

        for _ in 0..TASK_COUNT {
            ready
                .recv_with_cancellation(Some(StdDuration::from_secs(10)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
                .ok_or_else(|| Diagnostic::new("timed out waiting for suspended direct tasks"))?;
        }
        release.close();

        for (index, task) in tasks.iter().enumerate() {
            match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(10)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Ready(Ok(Value::Unit)) => {}
                other => {
                    return Err(Diagnostic::new(format!(
                        "suspended direct task {index} did not finish cleanly: {other:?}"
                    )))
                }
            }
        }

        Ok(Value::Unit)
    });

    assert_eq!(
        result.expect("1,000 suspended direct tasks should have independent call depth"),
        Value::Unit
    );
}

fn runtime_source_span(path: &str, line: usize, column: usize) -> RuntimeSourceSpan {
    RuntimeSourceSpan {
        path: Some(path.to_string()),
        start: Span::new(line, column),
        end: Span::new(line, column.saturating_add(1)),
    }
}

#[test]
fn native_runtime_call_frames_push_pop_and_snapshot_once_before_cleanup() {
    super::reset_direct_runtime_frame_materialization_count();
    super::with_direct_task_runtime_scope(|| {
        unsafe {
            super::aura_direct_enter_call_with_frame(
                2,
                3,
                b"/workspace/main.au".as_ptr(),
                b"/workspace/main.au".len(),
                b"main".as_ptr(),
                b"main".len(),
            );
            super::aura_direct_enter_call_with_frame(
                8,
                5,
                b"/workspace/lib.au".as_ptr(),
                b"/workspace/lib.au".len(),
                b"pkg.lib.work".as_ptr(),
                b"pkg.lib.work".len(),
            );
        }

        let first = capture_runtime_diagnostic(|| {
            super::runtime_error_at(Span::new(9, 7), "body failed");
        });
        assert_eq!(
            first.call_frames,
            vec![
                RuntimeCallFrame {
                    function: "pkg.lib.work".to_string(),
                    span: runtime_source_span("/workspace/lib.au", 8, 5),
                },
                RuntimeCallFrame {
                    function: "main".to_string(),
                    span: runtime_source_span("/workspace/main.au", 2, 3),
                },
            ]
        );
        assert_eq!(
            super::direct_runtime_frame_materialization_count(),
            2,
            "the first trap should materialize each active call frame exactly once"
        );

        // Propagation through an observer with a different active frame must
        // preserve the first completed snapshot byte-for-byte.
        unsafe {
            super::aura_direct_exit_call();
            super::aura_direct_enter_call_with_frame(
                20,
                1,
                b"/workspace/observer.au".as_ptr(),
                b"/workspace/observer.au".len(),
                b"observe".as_ptr(),
                b"observe".len(),
            );
        }
        let propagated = capture_runtime_diagnostic(|| {
            super::runtime_diagnostic_error(first.clone());
        });
        assert_eq!(propagated.call_frames, first.call_frames);
        assert_eq!(propagated.task_ancestry, first.task_ancestry);
        assert_eq!(
            super::direct_runtime_frame_materialization_count(),
            2,
            "re-propagating an already captured diagnostic must not rematerialize frames"
        );

        unsafe {
            super::aura_direct_exit_call();
            super::aura_direct_exit_call();
        }
        assert!(super::direct_runtime_call_frames().is_empty());
        Ok::<_, Diagnostic>(Value::Unit)
    })
    .expect("call-frame probe should complete");
}

#[test]
fn native_runtime_common_frame_storage_stays_inline_until_a_trap_materializes_it() {
    let ancestry = vec![RuntimeTaskFrame {
        task_function: "child".to_string(),
        task_entry_span: runtime_source_span("/workspace/child.au", 2, 1),
        parent_function: "main".to_string(),
        spawn_span: runtime_source_span("/workspace/main.au", 8, 5),
    }];

    super::reset_direct_runtime_frame_materialization_count();
    super::with_direct_task_runtime_scope_with_ancestry(ancestry, || {
        unsafe {
            super::aura_direct_enter_call_with_frame(
                2,
                1,
                b"/workspace/child.au".as_ptr(),
                b"/workspace/child.au".len(),
                b"child".as_ptr(),
                b"child".len(),
            );
        }

        super::with_direct_task_runtime_state(|state| {
            assert_eq!(state.call_frames.len(), 1);
            assert!(
                !state.call_frames.has_heap_spill(),
                "the common depth-one call chain must remain inline"
            );
            assert_eq!(state.task_ancestry.len(), 1);
        });
        assert_eq!(
            super::direct_runtime_frame_materialization_count(),
            0,
            "successful task and call entry must retain compact metadata instead of owned diagnostics"
        );

        unsafe {
            super::aura_direct_exit_call();
        }
        assert_eq!(super::direct_runtime_frame_materialization_count(), 0);
        Ok::<_, Diagnostic>(Value::Unit)
    })
    .expect("compact frame storage probe should complete");
}

#[test]
fn native_runtime_task_state_map_retains_only_pointer_sized_slots() {
    let state = super::boxed_direct_task_runtime_state(true, super::DirectTaskAncestry::default());
    let allocation = state.as_ref() as *const super::DirectTaskRuntimeState;
    super::with_direct_task_runtime_scope_with_state(state, || {
        let (stored_size, stored_allocation) = super::DIRECT_TASK_RUNTIME_STATES.with(|states| {
            let states = states.borrow();
            let state = states
                .values()
                .next()
                .expect("the active direct task must have runtime state");
            (
                std::mem::size_of_val(state),
                state.as_ref() as *const super::DirectTaskRuntimeState,
            )
        });
        assert_eq!(
            stored_size,
            std::mem::size_of::<Box<super::DirectTaskRuntimeState>>(),
            "the scheduler map must move only a boxed state pointer through coroutine scope setup"
        );
        assert_eq!(
            stored_allocation, allocation,
            "scope installation must preserve the prebuilt state allocation"
        );
        Ok::<_, Diagnostic>(Value::Unit)
    })
    .expect("boxed direct task-state layout probe should complete");
}

#[test]
fn nested_native_runtime_scope_restores_outer_task_ancestry() {
    let outer = RuntimeTaskFrame {
        task_function: "outer_task".to_string(),
        task_entry_span: runtime_source_span("/workspace/outer.au", 3, 1),
        parent_function: "main".to_string(),
        spawn_span: runtime_source_span("/workspace/main.au", 8, 5),
    };
    let inner = RuntimeTaskFrame {
        task_function: "inner_task".to_string(),
        task_entry_span: runtime_source_span("/workspace/inner.au", 4, 1),
        parent_function: "outer_task".to_string(),
        spawn_span: runtime_source_span("/workspace/outer.au", 12, 7),
    };

    super::with_direct_task_runtime_scope_with_ancestry(vec![outer.clone()], || {
        assert_eq!(super::direct_runtime_task_ancestry(), vec![outer.clone()]);

        super::with_direct_task_runtime_scope_with_ancestry(vec![inner.clone()], || {
            assert_eq!(super::direct_runtime_task_ancestry(), vec![inner]);
        });

        assert_eq!(
            super::direct_runtime_task_ancestry(),
            vec![outer],
            "leaving a nested native scope must restore the outer diagnostic ancestry"
        );
    });
}

#[test]
fn native_runtime_deep_persistent_task_ancestry_drops_iteratively() {
    const DEPTH: usize = 100_000;

    thread::Builder::new()
        .name("aura-deep-ancestry-drop".to_string())
        .stack_size(64 * 1024)
        .spawn(|| {
            let frame = super::DirectRuntimeTaskFrame::from_runtime(RuntimeTaskFrame {
                task_function: "child".to_string(),
                task_entry_span: runtime_source_span("/workspace/child.au", 2, 1),
                parent_function: "parent".to_string(),
                spawn_span: runtime_source_span("/workspace/parent.au", 8, 5),
            });
            let mut ancestry = super::DirectTaskAncestry::default();
            for _ in 0..DEPTH {
                ancestry = ancestry.prepend(frame.clone());
            }
            assert_eq!(ancestry.len(), DEPTH);
            drop(ancestry);
        })
        .expect("deep ancestry drop probe should spawn")
        .join()
        .expect("deep ancestry must drop without recursively overflowing the stack");
}

#[test]
fn native_runtime_frame_metadata_rejects_invalid_utf8_before_mutating_call_state() {
    let diagnostic = run_lightweight_root_task(|| {
        let invalid = [0xff_u8];
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                unsafe {
                    super::aura_direct_enter_call_with_frame(
                        2,
                        1,
                        b"/workspace/main.au".as_ptr(),
                        b"/workspace/main.au".len(),
                        invalid.as_ptr(),
                        invalid.len(),
                    );
                }
                #[allow(unreachable_code)]
                Value::Unit
            }))
        })
    })
    .expect_err("invalid frame metadata should fail at the scheduler boundary");
    assert!(diagnostic.message.contains("invalid UTF-8"));
    assert!(
        diagnostic.call_frames.is_empty(),
        "invalid metadata must be rejected before the attempted frame becomes active"
    );
}

#[test]
fn native_runtime_frame_metadata_rejects_a_null_required_name_before_call_activation() {
    let diagnostic = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                unsafe {
                    super::aura_direct_enter_call_with_frame(
                        2,
                        1,
                        b"/workspace/main.au".as_ptr(),
                        b"/workspace/main.au".len(),
                        std::ptr::null(),
                        0,
                    );
                }
                #[allow(unreachable_code)]
                Value::Unit
            }))
        })
    })
    .expect_err("a missing required function name should fail at the scheduler boundary");
    assert_eq!(
        diagnostic.message,
        "aura direct runtime received invalid UTF-8 bytes"
    );
    assert!(
        diagnostic.call_frames.is_empty(),
        "missing required metadata must be rejected before the attempted frame becomes active"
    );
}

#[test]
fn native_runtime_rejected_call_depth_does_not_push_the_attempted_frame() {
    let diagnostic = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                for index in 0..super::DIRECT_MAX_CALL_DEPTH {
                    let function: &'static str =
                        Box::leak(format!("accepted_{index}").into_boxed_str());
                    unsafe {
                        super::aura_direct_enter_call_with_frame(
                            index as i64 + 1,
                            1,
                            b"/workspace/depth.au".as_ptr(),
                            b"/workspace/depth.au".len(),
                            function.as_ptr(),
                            function.len(),
                        );
                    }
                }
                let attempted = b"rejected";
                unsafe {
                    super::aura_direct_enter_call_with_frame(
                        400,
                        9,
                        b"/workspace/depth.au".as_ptr(),
                        b"/workspace/depth.au".len(),
                        attempted.as_ptr(),
                        attempted.len(),
                    );
                }
                #[allow(unreachable_code)]
                Value::Unit
            }))
        })
    })
    .expect_err("the rejected call should fail at the scheduler boundary");
    assert_eq!(diagnostic.call_frames.len(), super::DIRECT_MAX_CALL_DEPTH);
    assert_eq!(
        diagnostic
            .call_frames
            .first()
            .map(|frame| frame.function.as_str()),
        Some("accepted_255")
    );
    assert!(
        diagnostic
            .call_frames
            .iter()
            .all(|frame| frame.function != "rejected"),
        "the rejected attempted callee was never active"
    );
}

#[test]
fn native_runtime_child_task_inherits_youngest_first_ancestry_and_starts_a_new_call_chain() {
    let parent_ancestry = vec![RuntimeTaskFrame {
        task_function: "parent".to_string(),
        task_entry_span: runtime_source_span("/workspace/parent.au", 3, 1),
        parent_function: "main".to_string(),
        spawn_span: runtime_source_span("/workspace/main.au", 10, 7),
    }];
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope_with_ancestry(parent_ancestry, || {
            unsafe {
                super::aura_direct_enter_call_with_frame(
                    5,
                    1,
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    b"parent".as_ptr(),
                    b"parent".len(),
                );
            }
            let args = super::aura_direct_arg_buffer_new(0);
            let group = super::aura_direct_task_group_new();
            let task_ptr = unsafe {
                super::aura_direct_start_task_call_with_frames(
                    direct_task_frame_trap as *const () as usize as i64,
                    args,
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"child".as_ptr(),
                    b"child".len(),
                    b"/workspace/child.au".as_ptr(),
                    b"/workspace/child.au".len(),
                    2,
                    1,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    12,
                    9,
                )
            };
            let task = unsafe {
                match value_ref(task_ptr) {
                    Value::Task(task) => task.clone(),
                    other => panic!("expected Task handle, found {other:?}"),
                }
            };
            let diagnostic = match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(5)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Ready(Err(error)) => error,
                other => panic!("expected failing direct child, got {other:?}"),
            };
            assert_eq!(
                diagnostic.call_frames,
                vec![RuntimeCallFrame {
                    function: "child".to_string(),
                    span: runtime_source_span("/workspace/child.au", 2, 1),
                }]
            );
            assert_eq!(
                diagnostic.task_ancestry,
                vec![
                    RuntimeTaskFrame {
                        task_function: "child".to_string(),
                        task_entry_span: runtime_source_span("/workspace/child.au", 2, 1),
                        parent_function: "parent".to_string(),
                        spawn_span: runtime_source_span("/workspace/parent.au", 12, 9),
                    },
                    RuntimeTaskFrame {
                        task_function: "parent".to_string(),
                        task_entry_span: runtime_source_span("/workspace/parent.au", 3, 1),
                        parent_function: "main".to_string(),
                        spawn_span: runtime_source_span("/workspace/main.au", 10, 7),
                    },
                ]
            );
            unsafe {
                release_value(task_ptr);
                release_value(group);
                super::aura_direct_exit_call();
            }
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("direct child ancestry probe should complete"),
        Value::Unit
    );
}

#[test]
fn native_runtime_function_value_task_handoff_uses_selected_callable_metadata() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let parent_ancestry = vec![RuntimeTaskFrame {
        task_function: "outer".to_string(),
        task_entry_span: runtime_source_span("/workspace/outer.au", 3, 1),
        parent_function: "main".to_string(),
        spawn_span: runtime_source_span("/workspace/main.au", 10, 7),
    }];
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope_with_ancestry(parent_ancestry, || {
            unsafe {
                super::aura_direct_enter_call_with_frame(
                    5,
                    1,
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    b"parent".as_ptr(),
                    b"parent".len(),
                );
            }
            let function = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "runtime_selected_child".to_string(),
                signature: Type::Function {
                    params: Vec::new(),
                    return_type: Box::new(Type::Unit),
                },
                source_path: Some("/workspace/selected_child.au".to_string()),
                entry_span: Span::new(2, 1),
                direct_thunk: Some(direct_task_frame_trap as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: None,
            })));
            let args = super::aura_direct_arg_buffer_new(0);
            let group = super::aura_direct_task_group_new();
            let task_ptr = unsafe {
                super::aura_direct_start_task_function_with_frames(
                    function,
                    args,
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    12,
                    9,
                )
            };
            let task = unsafe {
                match value_ref(task_ptr) {
                    Value::Task(task) => task.clone(),
                    other => panic!("expected Task handle, found {other:?}"),
                }
            };
            let diagnostic = match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(5)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Ready(Err(error)) => error,
                other => panic!("expected failing selected child, got {other:?}"),
            };
            assert_eq!(
                diagnostic.call_frames,
                vec![RuntimeCallFrame {
                    function: "child".to_string(),
                    span: runtime_source_span("/workspace/child.au", 2, 1),
                }]
            );
            assert_eq!(
                diagnostic.task_ancestry,
                vec![
                    RuntimeTaskFrame {
                        task_function: "runtime_selected_child".to_string(),
                        task_entry_span: runtime_source_span("/workspace/selected_child.au", 2, 1,),
                        parent_function: "parent".to_string(),
                        spawn_span: runtime_source_span("/workspace/parent.au", 12, 9),
                    },
                    RuntimeTaskFrame {
                        task_function: "outer".to_string(),
                        task_entry_span: runtime_source_span("/workspace/outer.au", 3, 1),
                        parent_function: "main".to_string(),
                        spawn_span: runtime_source_span("/workspace/main.au", 10, 7),
                    },
                ]
            );
            unsafe {
                super::aura_direct_release_value(task_ptr);
                super::aura_direct_release_value(group);
                super::aura_direct_release_value(function);
                super::aura_direct_exit_call();
            }
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("selected function-value child should complete"),
        Value::Unit
    );
}

#[test]
fn native_runtime_closure_task_handoff_transfers_capture_ownership_to_child() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope(|| {
            let capture_type = Type::named("int64");
            let function = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "capturing_child".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(capture_type.clone()),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "captured".to_string(),
                        ty: capture_type.clone(),
                        mode: crate::sema::ClosureCaptureMode::Copy,
                        span: Span::new(2, 1),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: Some("/workspace/capturing_child.au".to_string()),
                entry_span: Span::new(2, 1),
                direct_thunk: Some(test_native_thunk as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(
                    vec![ClosureCaptureValue {
                        name: "captured".to_string(),
                        ty: capture_type,
                        value: Value::Int(crate::integer::IntegerValue::from_signed(9)),
                        source_place: None,
                        mutable: false,
                    }],
                    false,
                ))),
            })));
            let args = super::aura_direct_arg_buffer_new(0);
            let group = super::aura_direct_task_group_new();
            let task = unsafe {
                super::aura_direct_start_task_function_with_frames(
                    function,
                    args,
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    12,
                    9,
                )
            };
            let joined = super::aura_direct_task_join(task);
            assert_eq!(expect_task_result_ready_int(joined), 9);
            unsafe {
                release_value(joined);
            }
            let closed = super::aura_direct_task_group_close(group, 0);
            expect_unit(closed);
            unsafe {
                release_value(closed);
                release_value(task);
                release_value(group);
                release_value(function);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "a completed closure task must leave no capture in the parent ownership ledger"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("capturing direct task should finish without poisoning teardown state"),
        Value::Unit
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "closure task handoff must release its externally owned claim flag"
    );
}

#[test]
fn native_runtime_closure_task_handoff_preserves_repeatable_and_one_shot_semantics() {
    fn closure_task_value(name: &str, captured: i64, consuming: bool) -> *mut OpaqueValue {
        boxed_value(Value::Function(Box::new(FunctionValue {
            name: name.to_string(),
            signature: Type::Closure {
                params: Box::new(Vec::new()),
                return_type: Box::new(Type::named("int64")),
                captures: Box::new(vec![crate::sema::ClosureCapture {
                    name: "captured".to_string(),
                    ty: Type::named("int64"),
                    mode: if consuming {
                        crate::sema::ClosureCaptureMode::Move
                    } else {
                        crate::sema::ClosureCaptureMode::Copy
                    },
                    span: Span::new(2, 1),
                }]),
                call_kind: if consuming {
                    crate::sema::ClosureCallKind::Consuming
                } else {
                    crate::sema::ClosureCallKind::Repeatable
                },
            },
            source_path: Some("/workspace/task_closure.au".to_string()),
            entry_span: Span::new(2, 1),
            direct_thunk: Some(test_native_thunk as *const () as usize as i64),
            direct_default_binder: Some(1),
            closure_environment: Some(Arc::new(ClosureEnvironment::new(
                vec![ClosureCaptureValue {
                    name: "captured".to_string(),
                    ty: Type::named("int64"),
                    value: Value::Int(IntegerValue::from_signed(i128::from(captured))),
                    source_place: None,
                    mutable: false,
                }],
                consuming,
            ))),
        })))
    }

    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope(|| {
            let repeatable = closure_task_value("repeatable_child", 11, false);
            let consuming = closure_task_value("consuming_child", 23, true);
            let group = super::aura_direct_task_group_new();

            for expected in [11, 11] {
                let task = unsafe {
                    super::aura_direct_start_task_function_with_frames(
                        repeatable,
                        super::aura_direct_arg_buffer_new(0),
                        0,
                        1,
                        group,
                        1,
                        0,
                        0,
                        b"parent".as_ptr(),
                        b"parent".len(),
                        b"/workspace/parent.au".as_ptr(),
                        b"/workspace/parent.au".len(),
                        8,
                        5,
                    )
                };
                let joined = super::aura_direct_task_join(task);
                assert_eq!(
                    expect_task_result_ready_int(joined),
                    expected,
                    "repeatable closure task starts must clone the capture each time"
                );
                unsafe {
                    release_value(joined);
                    release_value(task);
                }
            }

            let consuming_task = unsafe {
                super::aura_direct_start_task_function_with_frames(
                    consuming,
                    super::aura_direct_arg_buffer_new(0),
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    b"/workspace/parent.au".as_ptr(),
                    b"/workspace/parent.au".len(),
                    9,
                    5,
                )
            };
            let joined = super::aura_direct_task_join(consuming_task);
            assert_eq!(expect_task_result_ready_int(joined), 23);
            unsafe {
                release_value(joined);
                release_value(consuming_task);
            }

            let rejected_args = super::aura_direct_arg_buffer_new(0);
            let consuming_address = consuming as usize;
            let group_address = group as usize;
            let rejected_args_address = rejected_args as usize;
            assert_eq!(
                capture_runtime_error_message(move || unsafe {
                    super::aura_direct_start_task_function_with_frames(
                        consuming_address as *mut OpaqueValue,
                        rejected_args_address as *mut i64,
                        0,
                        1,
                        group_address as *mut OpaqueValue,
                        1,
                        0,
                        0,
                        b"parent".as_ptr(),
                        b"parent".len(),
                        std::ptr::null(),
                        0,
                        10,
                        5,
                    );
                }),
                "closure `consuming_child` has already consumed its captured environment"
            );
            unsafe {
                free_arg_buffer(rejected_args, 0);
            }

            let closed = super::aura_direct_task_group_close(group, 0);
            expect_unit(closed);
            unsafe {
                release_value(closed);
                release_value(group);
                release_value(consuming);
                release_value(repeatable);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "closure task transfer and retry rejection must balance all owned handles"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("closure task call-kind probes should complete"),
        Value::Unit
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "repeatable and one-shot task starts must release every claim flag"
    );
}

#[test]
fn native_runtime_closure_task_rejects_negative_public_arity_then_allows_valid_retry() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope(|| {
            let closure = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "retryable_child".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "captured".to_string(),
                        ty: Type::named("int64"),
                        mode: crate::sema::ClosureCaptureMode::Copy,
                        span: Span::new(2, 1),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: Some("/workspace/retryable_child.au".to_string()),
                entry_span: Span::new(2, 1),
                direct_thunk: Some(test_native_thunk as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(
                    vec![ClosureCaptureValue {
                        name: "captured".to_string(),
                        ty: Type::named("int64"),
                        value: Value::Int(IntegerValue::from_signed(29)),
                        source_place: None,
                        mutable: false,
                    }],
                    false,
                ))),
            })));
            let group = super::aura_direct_task_group_new();
            let closure_address = closure as usize;
            let group_address = group as usize;
            assert_eq!(
                capture_runtime_error_message(move || unsafe {
                    super::aura_direct_start_task_function_with_frames(
                        closure_address as *mut OpaqueValue,
                        std::ptr::null(),
                        -1,
                        1,
                        group_address as *mut OpaqueValue,
                        1,
                        0,
                        0,
                        b"parent".as_ptr(),
                        b"parent".len(),
                        std::ptr::null(),
                        0,
                        4,
                        3,
                    );
                }),
                "invalid task-start arg count"
            );

            let task = unsafe {
                super::aura_direct_start_task_function_with_frames(
                    closure,
                    super::aura_direct_arg_buffer_new(0),
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    std::ptr::null(),
                    0,
                    5,
                    3,
                )
            };
            let joined = super::aura_direct_task_join(task);
            assert_eq!(
                expect_task_result_ready_int(joined),
                29,
                "a repeatable closure must remain callable after rejected ABI metadata"
            );
            unsafe {
                release_value(joined);
                release_value(task);
            }
            let closed = super::aura_direct_task_group_close(group, 0);
            expect_unit(closed);
            unsafe {
                release_value(closed);
                release_value(group);
                release_value(closure);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "rejected and successful closure task starts must balance ownership"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("closure task retry probe should complete"),
        Value::Unit
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "the valid retry must release its external claim flag"
    );
}

#[test]
fn native_runtime_detached_closure_task_surfaces_unobserved_trap_and_cleans_capture() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let diagnostic =
        crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
            super::with_direct_task_runtime_scope(|| {
                Ok(super::with_task_runtime_error_capture(|| {
                    let closure = boxed_value(Value::Function(Box::new(FunctionValue {
                        name: "detached_closure".to_string(),
                        signature: Type::Closure {
                            params: Box::new(Vec::new()),
                            return_type: Box::new(Type::Unit),
                            captures: Box::new(vec![crate::sema::ClosureCapture {
                                name: "payload".to_string(),
                                ty: Type::named("str"),
                                mode: crate::sema::ClosureCaptureMode::Move,
                                span: Span::new(2, 1),
                            }]),
                            call_kind: crate::sema::ClosureCallKind::Consuming,
                        },
                        source_path: Some("/workspace/detached.au".to_string()),
                        entry_span: Span::new(2, 1),
                        direct_thunk: Some(direct_task_frame_trap as *const () as usize as i64),
                        direct_default_binder: Some(1),
                        closure_environment: Some(Arc::new(ClosureEnvironment::new(
                            vec![ClosureCaptureValue {
                                name: "payload".to_string(),
                                ty: Type::named("str"),
                                value: Value::String("owned by detached task".to_string()),
                                source_place: None,
                                mutable: false,
                            }],
                            true,
                        ))),
                    })));
                    let group = super::aura_direct_task_group_new();
                    let detached = unsafe {
                        super::aura_direct_start_task_function_with_frames(
                            closure,
                            super::aura_direct_arg_buffer_new(0),
                            0,
                            0,
                            group,
                            1,
                            0,
                            0,
                            b"parent".as_ptr(),
                            b"parent".len(),
                            b"/workspace/parent.au".as_ptr(),
                            b"/workspace/parent.au".len(),
                            12,
                            9,
                        )
                    };
                    expect_unit(detached);
                    unsafe {
                        release_value(detached);
                    }
                    let closed = super::aura_direct_task_group_close(group, 0);
                    #[allow(unreachable_code)]
                    {
                        unsafe {
                            release_value(closed);
                            release_value(group);
                            release_value(closure);
                        }
                        Value::Unit
                    }
                }))
            })
        })
        .expect_err("closing the group must surface the detached task's unobserved failure");
    assert_eq!(diagnostic.message, "child frame trap");
    assert_eq!(
        diagnostic.task_ancestry,
        vec![RuntimeTaskFrame {
            task_function: "detached_closure".to_string(),
            task_entry_span: runtime_source_span("/workspace/detached.au", 2, 1),
            parent_function: "parent".to_string(),
            spawn_span: runtime_source_span("/workspace/parent.au", 12, 9),
        }]
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "detached trap cleanup must release the task's capture claim flag"
    );
}

#[test]
fn native_runtime_function_value_task_handoff_preserves_absent_source_paths() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope(|| {
            let signature = Type::Function {
                params: Vec::new(),
                return_type: Box::new(Type::Unit),
            };
            let signature =
                serde_json::to_vec(&signature).expect("function signature should serialize");
            let function = super::aura_direct_function_value(
                direct_task_frame_trap as *const () as usize as i64,
                1,
                b"source_only_child".as_ptr(),
                b"source_only_child".len(),
                signature.as_ptr(),
                signature.len(),
                std::ptr::null(),
                0,
                6,
                4,
            );
            let args = super::aura_direct_arg_buffer_new(0);
            let group = super::aura_direct_task_group_new();
            let task_ptr = unsafe {
                super::aura_direct_start_task_function_with_frames(
                    function,
                    args,
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    std::ptr::null(),
                    0,
                    13,
                    5,
                )
            };
            let task = unsafe {
                match value_ref(task_ptr) {
                    Value::Task(task) => task.clone(),
                    other => panic!("expected Task handle, found {other:?}"),
                }
            };
            let diagnostic = match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(5)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Ready(Err(error)) => error,
                other => panic!("expected failing source-only child, got {other:?}"),
            };
            assert_eq!(
                diagnostic.task_ancestry,
                vec![RuntimeTaskFrame {
                    task_function: "source_only_child".to_string(),
                    task_entry_span: RuntimeSourceSpan::point(None, Span::new(6, 4)),
                    parent_function: "parent".to_string(),
                    spawn_span: RuntimeSourceSpan::point(None, Span::new(13, 5)),
                }]
            );
            unsafe {
                release_value(task_ptr);
                release_value(group);
                release_value(function);
            }
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("source-only function-value child should complete"),
        Value::Unit
    );
}

#[test]
fn native_runtime_static_task_frames_preserve_absent_paths_and_reject_invalid_utf8() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let result = crate::runtime_value::run_lightweight_root_task_with_worker_count(2, move || {
        super::with_direct_task_runtime_scope(|| {
            let group = super::aura_direct_task_group_new();
            let task_ptr = unsafe {
                super::aura_direct_start_task_call_with_frames(
                    direct_task_frame_trap as *const () as usize as i64,
                    super::aura_direct_arg_buffer_new(0),
                    0,
                    1,
                    group,
                    1,
                    0,
                    0,
                    b"static_child".as_ptr(),
                    b"static_child".len(),
                    std::ptr::null(),
                    0,
                    2,
                    1,
                    b"parent".as_ptr(),
                    b"parent".len(),
                    std::ptr::null(),
                    0,
                    7,
                    5,
                )
            };
            let task = unsafe {
                match value_ref(task_ptr) {
                    Value::Task(task) => task,
                    other => panic!("expected Task handle, found {other:?}"),
                }
            };
            let diagnostic = match task
                .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(5)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
            {
                TaskWaitStatus::Ready(Err(error)) => error,
                other => panic!("expected failing static child, found {other:?}"),
            };
            assert_eq!(
                diagnostic.task_ancestry,
                vec![RuntimeTaskFrame {
                    task_function: "static_child".to_string(),
                    task_entry_span: RuntimeSourceSpan::point(None, Span::new(2, 1)),
                    parent_function: "parent".to_string(),
                    spawn_span: RuntimeSourceSpan::point(None, Span::new(7, 5)),
                }]
            );
            unsafe {
                release_value(task_ptr);
                release_value(group);
            }
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("static task with absent paths should finish"),
        Value::Unit
    );
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "static task metadata probes must release the external claim flag"
    );

    fn invalid_frame_metadata(case: usize) -> String {
        capture_direct_boundary_error_message(move || {
            let invalid = [0xff_u8];
            let valid_task = b"child".as_slice();
            let valid_parent = b"parent".as_slice();
            let valid_path = b"/workspace/task.au".as_slice();
            let task_function = if case == 0 {
                invalid.as_slice()
            } else {
                valid_task
            };
            let parent_function = if case == 1 {
                invalid.as_slice()
            } else {
                valid_parent
            };
            let task_path = if case == 2 {
                invalid.as_slice()
            } else {
                valid_path
            };
            let spawn_path = if case == 3 {
                invalid.as_slice()
            } else {
                valid_path
            };
            unsafe {
                super::aura_direct_start_task_call_with_frames(
                    1,
                    std::ptr::null(),
                    0,
                    1,
                    std::ptr::null_mut(),
                    1,
                    0,
                    0,
                    task_function.as_ptr(),
                    task_function.len(),
                    task_path.as_ptr(),
                    task_path.len(),
                    1,
                    1,
                    parent_function.as_ptr(),
                    parent_function.len(),
                    spawn_path.as_ptr(),
                    spawn_path.len(),
                    1,
                    1,
                );
            }
        })
    }

    for case in 0..4 {
        assert_eq!(
            invalid_frame_metadata(case),
            "aura direct runtime received invalid UTF-8 bytes",
            "metadata case {case} must fail before task ownership handoff"
        );
    }
}

#[test]
fn native_runtime_function_value_task_handoff_rejects_invalid_spawn_path_utf8() {
    let signature = Type::Function {
        params: Vec::new(),
        return_type: Box::new(Type::Unit),
    };
    let signature = serde_json::to_vec(&signature).expect("function signature should serialize");
    let function = super::aura_direct_function_value(
        direct_task_frame_trap as *const () as usize as i64,
        1,
        b"child".as_ptr(),
        b"child".len(),
        signature.as_ptr(),
        signature.len(),
        std::ptr::null(),
        0,
        1,
        1,
    );
    let args = super::aura_direct_arg_buffer_new(0);
    let group = super::aura_direct_task_group_new();
    let function_address = function as usize;
    let args_address = args as usize;
    let group_address = group as usize;
    let message = capture_direct_boundary_error_message(move || {
        let invalid_path = [0xff_u8];
        unsafe {
            super::aura_direct_start_task_function_with_frames(
                function_address as *mut OpaqueValue,
                args_address as *mut i64,
                0,
                1,
                group_address as *mut OpaqueValue,
                1,
                0,
                0,
                b"parent".as_ptr(),
                b"parent".len(),
                invalid_path.as_ptr(),
                invalid_path.len(),
                1,
                1,
            );
        }
    });
    assert_eq!(message, "aura direct runtime received invalid UTF-8 bytes");

    unsafe {
        free_arg_buffer(args, 0);
        release_value(group);
        release_value(function);
    }
}

#[test]
fn native_runtime_function_value_task_handoff_rejects_noncallables_and_missing_thunks() {
    let non_callable = int_value(7);
    let missing_thunk = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "declaration-only".to_string(),
        signature: Type::Function {
            params: Vec::new(),
            return_type: Box::new(Type::Unit),
        },
        source_path: Some("/workspace/declaration.au".to_string()),
        entry_span: Span::new(1, 1),
        direct_thunk: None,
        direct_default_binder: Some(1),
        closure_environment: None,
    })));
    let group = super::aura_direct_task_group_new();

    for (function, expected) in [
        (
            non_callable,
            "task starting expected a function value, found `integer`",
        ),
        (missing_thunk, "direct function value has no native thunk"),
    ] {
        let args = super::aura_direct_arg_buffer_new(0);
        let function_address = function as usize;
        let args_address = args as usize;
        let group_address = group as usize;
        let message = capture_direct_boundary_error_message(move || unsafe {
            super::aura_direct_start_task_function_with_frames(
                function_address as *mut OpaqueValue,
                args_address as *mut i64,
                0,
                1,
                group_address as *mut OpaqueValue,
                1,
                0,
                0,
                b"parent".as_ptr(),
                b"parent".len(),
                std::ptr::null(),
                0,
                1,
                1,
            );
        });
        assert_eq!(message, expected);
        unsafe {
            free_arg_buffer(args, 0);
        }
    }

    unsafe {
        release_value(non_callable);
        release_value(missing_thunk);
        release_value(group);
    }
}

#[test]
fn native_runtime_closure_calls_preserve_results_writebacks_and_call_kind() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let repeatable = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_repeatable".to_string(),
                signature: Type::Closure {
                    params: Box::new(vec![FunctionParamContract {
                        name: "value".to_string(),
                        ty: Type::named("int64"),
                        passing: ReceiverKind::BorrowMut,
                        has_default: false,
                        default_erased: false,
                    }]),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "offset".to_string(),
                        ty: Type::named("int64"),
                        mode: crate::sema::ClosureCaptureMode::Copy,
                        span: Span::new(2, 17),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: Some("/workspace/main.au".to_string()),
                entry_span: Span::new(2, 17),
                direct_thunk: Some(
                    direct_closure_add_and_increment_mut_arg as *const () as usize as i64,
                ),
                direct_default_binder: Some(1),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(
                    vec![ClosureCaptureValue {
                        name: "offset".to_string(),
                        ty: Type::named("int64"),
                        value: Value::Int(IntegerValue::from_signed(7)),
                        source_place: None,
                        mutable: false,
                    }],
                    false,
                ))),
            })));

            let mut first_args = [int_value(5) as i64];
            let first_result =
                super::aura_direct_function_call(repeatable, first_args.as_mut_ptr(), 1);
            assert_eq!(expect_int(first_result), 12);
            assert_eq!(
                expect_int(first_args[0] as *mut OpaqueValue),
                6,
                "the closure thunk's mutable writeback must reach its caller"
            );
            unsafe {
                release_value(first_result);
                release_value(first_args[0] as *mut OpaqueValue);
            }

            let mut second_args = [int_value(8) as i64];
            let second_result =
                super::aura_direct_function_call(repeatable, second_args.as_mut_ptr(), 1);
            assert_eq!(
                expect_int(second_result),
                15,
                "repeatable closure calls must receive a fresh clone of the capture"
            );
            assert_eq!(expect_int(second_args[0] as *mut OpaqueValue), 9);
            unsafe {
                release_value(second_result);
                release_value(second_args[0] as *mut OpaqueValue);
            }

            unsafe {
                release_value(repeatable);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "successful and rejected closure calls must balance every opaque handle"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("native closure calls should complete"),
        Value::Unit
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let consuming = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_consuming".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "payload".to_string(),
                        ty: Type::named("int64"),
                        mode: crate::sema::ClosureCaptureMode::Move,
                        span: Span::new(4, 17),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Consuming,
                },
                source_path: Some("/workspace/main.au".to_string()),
                entry_span: Span::new(4, 17),
                direct_thunk: Some(direct_closure_returns_capture as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(
                    vec![ClosureCaptureValue {
                        name: "payload".to_string(),
                        ty: Type::named("int64"),
                        value: Value::Int(IntegerValue::from_signed(19)),
                        source_place: None,
                        mutable: false,
                    }],
                    true,
                ))),
            })));
            let first = super::aura_direct_function_call(consuming, std::ptr::null_mut(), 0);
            assert_eq!(expect_int(first), 19);
            unsafe {
                release_value(first);
            }
            super::aura_direct_function_call(consuming, std::ptr::null_mut(), 0);
        }),
        "closure `main::__lambda_consuming` has already consumed its captured environment"
    );
}

#[test]
fn native_runtime_closure_call_moves_owned_args_and_copies_only_mutable_writebacks() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let closure = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_mixed_args".to_string(),
                signature: Type::Closure {
                    params: Box::new(vec![
                        FunctionParamContract {
                            name: "owned".to_string(),
                            ty: Type::named("int64"),
                            passing: ReceiverKind::Value,
                            has_default: false,
                            default_erased: false,
                        },
                        FunctionParamContract {
                            name: "mutable".to_string(),
                            ty: Type::named("int64"),
                            passing: ReceiverKind::BorrowMut,
                            has_default: false,
                            default_erased: false,
                        },
                    ]),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "offset".to_string(),
                        ty: Type::named("int64"),
                        mode: crate::sema::ClosureCaptureMode::Copy,
                        span: Span::new(3, 17),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: Some("/workspace/main.au".to_string()),
                entry_span: Span::new(3, 17),
                direct_thunk: Some(
                    direct_closure_consumes_owned_and_writes_mut as *const () as usize as i64,
                ),
                direct_default_binder: Some(1),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(
                    vec![ClosureCaptureValue {
                        name: "offset".to_string(),
                        ty: Type::named("int64"),
                        value: Value::Int(IntegerValue::from_signed(2)),
                        source_place: None,
                        mutable: false,
                    }],
                    false,
                ))),
            })));
            let mut public_args = [int_value(3) as i64, int_value(4) as i64];
            let called = super::aura_direct_function_call(closure, public_args.as_mut_ptr(), 2);

            assert_eq!(expect_int(called), 9);
            assert_eq!(
                public_args[0], 0,
                "an owned public argument must remain consumed after the closure returns"
            );
            assert_eq!(
                expect_int(public_args[1] as *mut OpaqueValue),
                9,
                "only the mutable argument's replacement must be copied to the caller"
            );

            unsafe {
                release_value(public_args[1] as *mut OpaqueValue);
                release_value(called);
                release_value(closure);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "mixed closure argument movement must balance captures, inputs, and writebacks"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("mixed closure call should complete"),
        Value::Unit
    );
}

#[test]
fn native_runtime_closure_construction_handles_zero_captures_and_reports_invalid_inputs() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let zero_capture_base = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_zero".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::named("int64")),
                    captures: Box::new(Vec::new()),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: Some("/workspace/main.au".to_string()),
                entry_span: Span::new(2, 17),
                direct_thunk: Some(direct_zero_capture_closure as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: None,
            })));
            let zero_capture = super::aura_direct_closure_value(
                zero_capture_base,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                0,
            );
            let called = super::aura_direct_function_call(zero_capture, std::ptr::null_mut(), 0);
            assert_eq!(expect_int(called), 42);
            unsafe {
                release_value(called);
                release_value(zero_capture);
            }

            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "invalid closure construction must consume or release every transferred value"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("closure construction probes should complete"),
        Value::Unit
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let invalid_count_base = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_invalid".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::Unit),
                    captures: Box::new(Vec::new()),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: None,
                entry_span: Span::new(1, 1),
                direct_thunk: Some(direct_zero_capture_closure as *const () as usize as i64),
                direct_default_binder: Some(1),
                closure_environment: None,
            })));
            super::aura_direct_closure_value(
                invalid_count_base,
                std::ptr::null_mut(),
                -1,
                std::ptr::null(),
                0,
            );
        }),
        "invalid closure capture count"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_closure_value(
                int_value(7),
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                0,
            );
        }),
        "direct closure construction expected a function value"
    );
}

#[test]
fn native_runtime_selected_default_callbacks_bind_functions_but_not_closures() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let ordinary = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "selected".to_string(),
                signature: Type::Function {
                    params: vec![FunctionParamContract {
                        name: "value".to_string(),
                        ty: Type::named("int64"),
                        passing: ReceiverKind::Value,
                        has_default: true,
                        default_erased: false,
                    }],
                    return_type: Box::new(Type::named("int64")),
                },
                source_path: None,
                entry_span: Span::new(1, 1),
                direct_thunk: Some(test_native_thunk as *const () as usize as i64),
                direct_default_binder: Some(
                    direct_test_default_binder as *const () as usize as i64,
                ),
                closure_environment: None,
            })));
            let ordinary_args = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_function_bind_defaults(ordinary, ordinary_args, 1, 0);
            assert_eq!(
                unsafe { value_ref(*ordinary_args as *mut OpaqueValue) },
                Value::Int(IntegerValue::from_signed(41)),
                "the selected declaration's native default binder must fill its missing slot"
            );
            let called = super::aura_direct_function_call(ordinary, ordinary_args, 1);
            assert_eq!(expect_int(called), 41);
            unsafe {
                release_value(called);
            }
            unsafe {
                free_arg_buffer(ordinary_args, 1);
            }

            let closure = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_no_defaults".to_string(),
                signature: Type::Closure {
                    params: Box::new(vec![FunctionParamContract {
                        name: "value".to_string(),
                        ty: Type::named("int64"),
                        passing: ReceiverKind::Value,
                        has_default: false,
                        default_erased: false,
                    }]),
                    return_type: Box::new(Type::Unit),
                    captures: Box::new(Vec::new()),
                    call_kind: crate::sema::ClosureCallKind::Repeatable,
                },
                source_path: None,
                entry_span: Span::new(1, 1),
                direct_thunk: Some(test_native_thunk as *const () as usize as i64),
                direct_default_binder: Some(
                    direct_test_default_binder as *const () as usize as i64,
                ),
                closure_environment: Some(Arc::new(ClosureEnvironment::new(Vec::new(), false))),
            })));
            let closure_args = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_function_bind_defaults(closure, closure_args, 1, 0);
            assert_eq!(
                unsafe { *closure_args },
                0,
                "lambda parameters have no declaration defaults and must bypass the binder callback"
            );
            unsafe {
                free_arg_buffer(closure_args, 1);
                release_value(closure);
                release_value(ordinary);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(state.owned_value_refs.is_empty());
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("selected default callback probes should complete"),
        Value::Unit
    );
}

#[test]
fn native_runtime_indirect_call_and_default_callback_traps_identify_invalid_selection() {
    fn selected_function(thunk: Option<i64>, default_binder: Option<i64>) -> *mut OpaqueValue {
        boxed_value(Value::Function(Box::new(FunctionValue {
            name: "selected".to_string(),
            signature: Type::Function {
                params: Vec::new(),
                return_type: Box::new(Type::named("int64")),
            },
            source_path: None,
            entry_span: Span::new(1, 1),
            direct_thunk: thunk,
            direct_default_binder: default_binder,
            closure_environment: None,
        })))
    }

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_bind_defaults(int_value(7), std::ptr::null_mut(), 0, 0);
        }),
        "indirect call expected a function value, found `integer`"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_bind_defaults(
                selected_function(Some(1), None),
                std::ptr::null_mut(),
                0,
                0,
            );
        }),
        "direct function value has no native default binder"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_bind_defaults(
                selected_function(
                    Some(1),
                    Some(direct_test_default_binder as *const () as usize as i64),
                ),
                std::ptr::null_mut(),
                -1,
                0,
            );
        }),
        "invalid indirect-call arg count"
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_call(int_value(7), std::ptr::null_mut(), 0);
        }),
        "indirect call expected a function value, found `integer`"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_call(
                selected_function(None, Some(1)),
                std::ptr::null_mut(),
                0,
            );
        }),
        "direct function value has no native thunk"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_function_call(
                selected_function(
                    Some(test_native_thunk as *const () as usize as i64),
                    Some(1),
                ),
                std::ptr::null_mut(),
                -1,
            );
        }),
        "invalid indirect-call arg count"
    );
}

#[test]
fn native_runtime_trapping_closure_call_releases_combined_buffer_without_mut_writeback() {
    unsafe extern "C-unwind" fn trapping_closure_thunk(
        _args: *const i64,
        _count: usize,
    ) -> *mut OpaqueValue {
        super::runtime_error("closure body trapped")
    }

    let external = string_value("mutable caller value");
    let external_address = external as usize;
    let signature = Type::Closure {
        params: Box::new(vec![FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("str"),
            passing: ReceiverKind::BorrowMut,
            has_default: false,
            default_erased: false,
        }]),
        return_type: Box::new(Type::Unit),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "captured".to_string(),
            ty: Type::named("str"),
            mode: crate::sema::ClosureCaptureMode::Move,
            span: Span::new(3, 13),
        }]),
        call_kind: crate::sema::ClosureCallKind::Consuming,
    };
    let function = boxed_value(Value::Function(Box::new(FunctionValue {
        name: "main::__lambda_trap".to_string(),
        signature,
        source_path: Some("/workspace/main.au".to_string()),
        entry_span: Span::new(3, 13),
        direct_thunk: Some(trapping_closure_thunk as *const () as usize as i64),
        direct_default_binder: Some(1),
        closure_environment: Some(Arc::new(ClosureEnvironment::new(
            vec![ClosureCaptureValue {
                name: "captured".to_string(),
                ty: Type::named("str"),
                value: Value::String("owned capture".to_string()),
                source_place: None,
                mutable: false,
            }],
            false,
        ))),
    })));
    let function_address = function as usize;

    let error = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let external = external_address as *mut OpaqueValue;
            let retained = unsafe { retain_value(external) };
            let mut args = [retained as i64];
            let function = function_address as *mut OpaqueValue;
            let failure = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                super::with_task_runtime_error_capture(|| {
                    super::aura_direct_function_call(function, args.as_mut_ptr(), 1);
                    Ok::<Value, Diagnostic>(Value::Unit)
                })
            }));
            assert!(failure.is_err(), "the generated closure thunk must trap");
            assert_eq!(
                args[0], 0,
                "a trapping closure call must not install a mutable writeback"
            );
            assert_eq!(
                unsafe { &*external }.ref_count.load(Ordering::Acquire),
                1,
                "the unwind guard must release the retained public argument"
            );
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "closure capture and public-argument handles must leave no owned-ledger entries"
                );
            });
            Ok(Value::Unit)
        })
    });
    let error = error.expect_err("the direct-runtime trap should fail the root task");
    assert_eq!(error.message, "closure body trapped");
    unsafe {
        release_value(function);
        release_value(external);
    }
}

#[test]
fn native_runtime_uncalled_closure_releases_owned_capture_environment() {
    let result = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let base = boxed_value(Value::Function(Box::new(FunctionValue {
                name: "main::__lambda_uncalled".to_string(),
                signature: Type::Closure {
                    params: Box::new(Vec::new()),
                    return_type: Box::new(Type::Unit),
                    captures: Box::new(vec![crate::sema::ClosureCapture {
                        name: "payload".to_string(),
                        ty: Type::named("str"),
                        mode: crate::sema::ClosureCaptureMode::Move,
                        span: Span::new(2, 17),
                    }]),
                    call_kind: crate::sema::ClosureCallKind::Consuming,
                },
                source_path: Some("/workspace/main.au".to_string()),
                entry_span: Span::new(2, 17),
                direct_thunk: Some(1),
                direct_default_binder: Some(1),
                closure_environment: None,
            })));
            let capture = string_value("never called");
            let captures = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store_owned(captures, 0, capture as i64);
            let closure = super::aura_direct_closure_value(base, captures, 1, std::ptr::null(), 1);
            unsafe {
                release_value(closure);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "dropping an uncalled closure must release its environment and opaque handle"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("uncalled closure cleanup should complete"),
        Value::Unit
    );
}

unsafe extern "C-unwind" fn direct_task_frame_trap(
    _args: *const i64,
    _arg_count: usize,
) -> *mut OpaqueValue {
    unsafe {
        super::aura_direct_enter_call_with_frame(
            2,
            1,
            b"/workspace/child.au".as_ptr(),
            b"/workspace/child.au".len(),
            b"child".as_ptr(),
            b"child".len(),
        );
    }
    super::runtime_error_at(Span::new(4, 11), "child frame trap")
}

#[cfg(unix)]
#[test]
fn native_runtime_internal_diagnostic_channels_are_hidden_cloexec_and_one_shot() {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;

    const HELPER_ENV: &str = "AURA_INTERNAL_DIAGNOSTIC_CHANNEL_TEST_HELPER";
    if std::env::var(HELPER_ENV).as_deref() == Ok("1") {
        let data_fd = std::env::var(crate::INTERNAL_DIAGNOSTIC_FD_ENV)
            .expect("helper data descriptor should be present")
            .parse::<i32>()
            .expect("helper data descriptor should be numeric");
        let signal_fd = std::env::var(crate::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV)
            .expect("helper signal descriptor should be present")
            .parse::<i32>()
            .expect("helper signal descriptor should be numeric");
        super::initialize_internal_diagnostic_channels();
        assert!(
            std::env::var_os(crate::INTERNAL_DIAGNOSTIC_FD_ENV).is_none(),
            "the private data descriptor must be hidden from Aura env access and child processes"
        );
        assert!(
            std::env::var_os(crate::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV).is_none(),
            "the private signal descriptor must be hidden from Aura env access and child processes"
        );
        for fd in [data_fd, signal_fd] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert_ne!(flags, -1);
            assert_ne!(
                flags & libc::FD_CLOEXEC,
                0,
                "captured private descriptors must not leak through user process spawns"
            );
        }

        let mut diagnostic =
            Diagnostic::coded_at("AU4003", Span::new(4, 11), "structured channel failure")
                .with_assertion_operand("left", "str", "x".repeat(6_000))
                .with_assertion_operand("right", "str", "expected");
        diagnostic.capture_runtime_frames_once(
            vec![RuntimeCallFrame {
                function: "child".to_string(),
                span: runtime_source_span("/workspace/child.au", 2, 1),
            }],
            vec![RuntimeTaskFrame {
                task_function: "child".to_string(),
                task_entry_span: runtime_source_span("/workspace/child.au", 2, 1),
                parent_function: "main".to_string(),
                spawn_span: runtime_source_span("/workspace/main.au", 8, 7),
            }],
        );
        assert_eq!(
            super::try_emit_internal_structured_diagnostic(&diagnostic),
            super::InternalDiagnosticEmission::Emitted,
            "the signal marker and structured record should both be written",
        );
        assert_eq!(
            super::try_emit_internal_structured_diagnostic(&diagnostic),
            super::InternalDiagnosticEmission::NoChannel,
            "the inherited channel is a consumed one-shot",
        );
        return;
    }

    let mut diagnostic_descriptors = [0; 2];
    let mut signal_descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe(diagnostic_descriptors.as_mut_ptr()) },
        0
    );
    assert_eq!(unsafe { libc::pipe(signal_descriptors.as_mut_ptr()) }, 0);
    let diagnostic_reader = unsafe { std::fs::File::from_raw_fd(diagnostic_descriptors[0]) };
    let diagnostic_writer = unsafe { std::fs::File::from_raw_fd(diagnostic_descriptors[1]) };
    let signal_reader = unsafe { std::fs::File::from_raw_fd(signal_descriptors[0]) };
    let signal_writer = unsafe { std::fs::File::from_raw_fd(signal_descriptors[1]) };
    for fd in [
        diagnostic_reader.as_raw_fd(),
        diagnostic_writer.as_raw_fd(),
        signal_reader.as_raw_fd(),
        signal_writer.as_raw_fd(),
    ] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) },
            0
        );
    }
    let inherited_data_fd = diagnostic_writer.as_raw_fd();
    let inherited_signal_fd = signal_writer.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().expect("test binary should exist"));
    command
        .arg("--exact")
        .arg(
            "native_runtime::tests::native_runtime_internal_diagnostic_channels_are_hidden_cloexec_and_one_shot",
        )
        .arg("--nocapture")
        .env(HELPER_ENV, "1")
        .env(
            crate::INTERNAL_DIAGNOSTIC_FD_ENV,
            inherited_data_fd.to_string(),
        )
        .env(
            crate::INTERNAL_DIAGNOSTIC_SIGNAL_FD_ENV,
            inherited_signal_fd.to_string(),
        );
    unsafe {
        command.pre_exec(move || {
            for fd in [inherited_data_fd, inherited_signal_fd] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .expect("private-channel helper process should start");
    drop(diagnostic_writer);
    drop(signal_writer);

    let mut signal = Vec::new();
    signal_reader
        .take(crate::MAX_INTERNAL_DIAGNOSTIC_BYTES as u64 + 1)
        .read_to_end(&mut signal)
        .expect("signal reader should observe EOF after the marker");
    let mut bytes = Vec::new();
    diagnostic_reader
        .take(crate::MAX_INTERNAL_DIAGNOSTIC_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .expect("reader should observe EOF after the one record");
    let status = child
        .wait()
        .expect("private-channel helper process should exit");
    assert!(status.success(), "private-channel helper should pass");

    assert_eq!(
        signal,
        [crate::INTERNAL_DIAGNOSTIC_SIGNAL_MARKER],
        "a trapped native program must emit exactly one intent marker before its record"
    );
    let structured: StructuredDiagnostic =
        serde_json::from_slice(&bytes).expect("channel should carry one structured diagnostic");
    assert_eq!(structured.message, "structured channel failure");
    assert_eq!(structured.assertion_operands.len(), 2);
    assert!(structured.assertion_operands[0].truncated);
    assert!(structured.assertion_operands[0].value.len() <= 4_096);
    assert_eq!(structured.assertion_operands[1].value, "expected");
    assert_eq!(structured.call_frames.len(), 1);
    assert_eq!(structured.call_frames[0].span.path, "/workspace/child.au");
    assert_eq!(structured.task_ancestry.len(), 1);
    assert_eq!(
        structured.task_ancestry[0].spawn_span.path,
        "/workspace/main.au"
    );
    assert!(
        bytes.len() <= crate::MAX_INTERNAL_DIAGNOSTIC_BYTES,
        "writer must enforce the same bounded record contract as its reader"
    );
}

#[cfg(unix)]
#[test]
fn native_runtime_internal_diagnostic_signal_precedes_failed_record_encoding() {
    use std::os::fd::FromRawFd;

    let mut diagnostic_descriptors = [0; 2];
    let mut signal_descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe(diagnostic_descriptors.as_mut_ptr()) },
        0
    );
    assert_eq!(unsafe { libc::pipe(signal_descriptors.as_mut_ptr()) }, 0);
    super::install_internal_diagnostic_channels(diagnostic_descriptors[1], signal_descriptors[1]);

    let oversized = Diagnostic::new("x".repeat(crate::MAX_INTERNAL_DIAGNOSTIC_BYTES + 1));
    assert_eq!(
        super::try_emit_internal_structured_diagnostic(&oversized),
        super::InternalDiagnosticEmission::SignaledWithoutRecord,
        "an oversized record must preserve the parent-owned JSON failure path",
    );
    let mut signal = Vec::new();
    unsafe { std::fs::File::from_raw_fd(signal_descriptors[0]) }
        .read_to_end(&mut signal)
        .expect("signal reader should observe EOF");
    assert_eq!(
        signal,
        [crate::INTERNAL_DIAGNOSTIC_SIGNAL_MARKER],
        "trap intent must be observable even when the data record cannot be encoded"
    );
    let mut bytes = Vec::new();
    unsafe { std::fs::File::from_raw_fd(diagnostic_descriptors[0]) }
        .read_to_end(&mut bytes)
        .expect("data reader should observe EOF");
    assert!(
        bytes.is_empty(),
        "failed record encoding must not emit a partial JSON object"
    );
}

#[test]
fn native_runtime_error_capture_is_isolated_across_suspended_tasks() {
    let result = run_lightweight_root_task(|| {
        let ready = ChannelValue::new();
        let release_first = ChannelValue::new();
        let release_second = ChannelValue::new();

        let first_ready = ready.clone();
        let first_release = release_first.clone();
        let first = spawn_lightweight_task(move || {
            super::with_direct_task_runtime_scope(|| {
                Ok(super::with_task_runtime_error_capture(|| {
                    first_ready
                        .send(Value::Unit)
                        .expect("ready channel should remain open");
                    let _ = first_release.recv_with_cancellation(None, None);
                    Value::Unit
                }))
            })
        })?;

        let second_ready = ready.clone();
        let second_release = release_second.clone();
        let second = spawn_lightweight_task(move || {
            super::with_direct_task_runtime_scope(|| {
                let capture_was_preserved = super::with_task_runtime_error_capture(|| {
                    second_ready
                        .send(Value::Unit)
                        .expect("ready channel should remain open");
                    let _ = second_release.recv_with_cancellation(None, None);
                    super::direct_runtime_error_capture_enabled()
                });
                if capture_was_preserved {
                    Ok(Value::Unit)
                } else {
                    Err(Diagnostic::new(
                        "another suspended task cleared direct runtime error capture",
                    ))
                }
            })
        })?;

        for _ in 0..2 {
            ready
                .recv_with_cancellation(Some(StdDuration::from_secs(1)), None)
                .map_err(|error| Diagnostic::new(error.to_string()))?
                .ok_or_else(|| Diagnostic::new("timed out waiting for capture test tasks"))?;
        }

        release_first.close();
        match first
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Ok(Value::Unit)) => {}
            other => {
                return Err(Diagnostic::new(format!(
                    "first capture test task did not finish cleanly: {other:?}"
                )))
            }
        }

        release_second.close();
        match second
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Ok(Value::Unit)) => Ok(Value::Unit),
            other => Err(Diagnostic::new(format!(
                "second capture test task did not preserve its state: {other:?}"
            ))),
        }
    });

    assert_eq!(
        result.expect("direct runtime error capture should be task-local"),
        Value::Unit
    );
}

#[test]
fn native_runtime_cleanup_diagnostic_state_is_isolated_across_tasks() {
    let result = run_lightweight_root_task(|| {
        let ready = ChannelValue::new();
        let release = ChannelValue::new();

        let first_ready = ready.clone();
        let first_release = release.clone();
        let first = spawn_lightweight_task(move || {
            super::with_direct_task_runtime_scope(|| {
                let _primary = super::DirectPrimaryDiagnosticGuard::install(Diagnostic::new(
                    "first task primary",
                ));
                let key = super::direct_task_runtime_key();
                super::with_direct_task_runtime_state(|state| state.cleanup_draining = true);
                let _draining = super::DirectCleanupDrainGuard { key };
                first_ready
                    .send(Value::Unit)
                    .expect("ready channel should remain open");
                let _ = first_release.recv_with_cancellation(None, None);
                Ok(Value::Unit)
            })
        })?;

        ready
            .recv_with_cancellation(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
            .ok_or_else(|| Diagnostic::new("timed out waiting for cleanup-state task"))?;

        let second = spawn_lightweight_task(|| {
            super::with_direct_task_runtime_scope(|| {
                let draining = super::direct_cleanup_is_draining();
                let primary = super::direct_primary_runtime_diagnostic();
                if draining || primary.is_some() {
                    return Err(Diagnostic::new(
                        "another suspended task leaked direct cleanup diagnostic state",
                    ));
                }
                Ok(Value::Unit)
            })
        })?;

        let second_status = second
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        release.close();
        let first_status = first
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?;

        match (first_status, second_status) {
            (TaskWaitStatus::Ready(Ok(Value::Unit)), TaskWaitStatus::Ready(Ok(Value::Unit))) => {
                Ok(Value::Unit)
            }
            other => Err(Diagnostic::new(format!(
                "cleanup diagnostic isolation failed: {other:?}"
            ))),
        }
    });

    assert_eq!(
        result.expect("direct cleanup diagnostic state should be task-local"),
        Value::Unit
    );
}

#[test]
fn native_runtime_task_exit_unwinds_live_drop_values() {
    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = dropped.clone();
    let result = run_lightweight_root_task(move || {
        let task = spawn_lightweight_task(move || {
            let _probe = DropProbe(task_dropped);
            super::task_runtime_boundary(|| {
                std::panic::panic_any(TaskCancelledSignal);
            });
            #[allow(unreachable_code)]
            Ok(Value::Unit)
        })?;

        match task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Cancelled => Ok(Value::Unit),
            other => Err(Diagnostic::new(format!(
                "expected cancelled task, got {other:?}"
            ))),
        }
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert!(
        dropped.load(Ordering::SeqCst),
        "task exit must unwind live Rust values before reclaiming its coroutine stack"
    );
}

#[test]
fn native_runtime_direct_forced_exit_runs_external_cleanup() {
    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let cleaned = Arc::new(AtomicBool::new(false));
    let cleanup_probe = DropProbe(cleaned.clone());
    let result = run_lightweight_root_task(move || {
        let task = unsafe {
            crate::runtime_value::spawn_lightweight_task_with_cancellation_and_forced_exit_cleanup(
                CancellationContext::default(),
                || {
                    super::with_direct_task_runtime_scope_with_ancestry(
                        vec![RuntimeTaskFrame {
                            task_function: "failing_child".to_string(),
                            task_entry_span: runtime_source_span(
                                "/workspace/failing_child.au",
                                2,
                                1,
                            ),
                            parent_function: "main".to_string(),
                            spawn_span: runtime_source_span("/workspace/main.au", 9, 5),
                        }],
                        || {
                            super::aura_direct_enter_call_with_frame(
                                2,
                                1,
                                b"/workspace/failing_child.au".as_ptr(),
                                b"/workspace/failing_child.au".len(),
                                b"failing_child".as_ptr(),
                                b"failing_child".len(),
                            );
                            super::with_task_runtime_error_capture(|| {
                                super::task_runtime_boundary(|| {
                                    std::panic::panic_any(LightweightTaskFailureSignal(
                                        Diagnostic::new("direct task failure"),
                                    ));
                                });
                                #[allow(unreachable_code)]
                                Ok(Value::Unit)
                            })
                        },
                    )
                },
                move || {
                    super::discard_current_direct_task_runtime_state();
                    drop(cleanup_probe);
                },
            )?
        };

        let status = task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?;
        let stale_child_state =
            super::DIRECT_TASK_RUNTIME_STATES.with(|states| states.borrow().contains_key(&2));
        if stale_child_state {
            return Err(Diagnostic::new(
                "direct forced exit left task-local runtime state behind",
            ));
        }
        match status {
            TaskWaitStatus::Ready(Err(error))
                if error.message == "direct task failure"
                    && error.call_frames
                        == vec![RuntimeCallFrame {
                            function: "failing_child".to_string(),
                            span: runtime_source_span("/workspace/failing_child.au", 2, 1),
                        }]
                    && error.task_ancestry
                        == vec![RuntimeTaskFrame {
                            task_function: "failing_child".to_string(),
                            task_entry_span: runtime_source_span(
                                "/workspace/failing_child.au",
                                2,
                                1,
                            ),
                            parent_function: "main".to_string(),
                            spawn_span: runtime_source_span("/workspace/main.au", 9, 5),
                        }] =>
            {
                Ok(Value::Unit)
            }
            other => Err(Diagnostic::new(format!(
                "expected failed direct task, got {other:?}"
            ))),
        }
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert!(
        cleaned.load(Ordering::SeqCst),
        "direct forced exit must run its scheduler-owned external cleanup"
    );
}

unsafe extern "C-unwind" fn direct_task_trap_while_holding_argument(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert_eq!(arg_count, 1);
    let _held_argument = unsafe { *args };
    super::aura_direct_fail_division_by_zero(0, 0)
}

unsafe extern "C-unwind" fn direct_task_cancel_while_holding_argument(
    args: *const i64,
    arg_count: usize,
) -> *mut OpaqueValue {
    assert_eq!(arg_count, 1);
    let _held_argument = unsafe { *args };
    assert_eq!(super::aura_direct_cancelled(), 1);
    super::task_runtime_boundary(|| std::panic::panic_any(TaskCancelledSignal));
    unreachable!("the cancellation boundary must exit the current lightweight task")
}

#[test]
fn native_runtime_direct_forced_exit_releases_frame_owned_argument_references() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("owned by the direct task frame");
    let argument_address = argument as usize;

    let result = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let argument = argument_address as *mut OpaqueValue;
            let args = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store(args, 0, argument as i64);
            let group = super::aura_direct_task_group_new();
            let task = unsafe {
                super::aura_direct_start_task_call(
                    direct_task_trap_while_holding_argument as *const () as usize as i64,
                    args,
                    1,
                    1,
                    group,
                    1,
                    0,
                    0,
                )
            };
            let joined = super::aura_direct_task_join(task);
            assert_eq!(expect_task_result_error_message(joined), "division by zero");
            unsafe {
                release_value(joined);
                release_value(task);
                release_value(group);
            }
            assert_eq!(
                unsafe { &*argument }.ref_count.load(Ordering::Acquire),
                1,
                "forced exit must release the argument reference claimed by the direct task frame"
            );
            Ok(Value::Unit)
        })
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "forced task exit must free its externally owned claim flag"
    );
    unsafe {
        release_value(argument);
    }
}

#[test]
fn native_runtime_direct_cancellation_releases_frame_owned_argument_references() {
    let _claim_flag_guard = super::direct_task_claim_flag_test_guard();
    let claim_flag_baseline = super::direct_task_claim_flag_live_count();
    let argument = string_value("owned by the cancelled direct task frame");
    let argument_address = argument as usize;

    let result = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let argument = argument_address as *mut OpaqueValue;
            let args = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store(args, 0, argument as i64);
            let group = super::aura_direct_task_group_new();
            let task = unsafe {
                super::aura_direct_start_task_call(
                    direct_task_cancel_while_holding_argument as *const () as usize as i64,
                    args,
                    1,
                    1,
                    group,
                    1,
                    0,
                    0,
                )
            };
            let cancelled = super::aura_direct_task_group_cancel(group);
            expect_unit(cancelled);
            unsafe {
                release_value(cancelled);
            }
            let joined = super::aura_direct_task_join(task);
            match unsafe { take_value(joined) } {
                Value::EnumVariant(variant)
                    if variant.enum_name == "TaskResult" && variant.variant_name == "Cancelled" => {
                }
                other => panic!("expected TaskResult.Cancelled, found {other:?}"),
            }
            unsafe {
                release_value(joined);
                release_value(task);
                release_value(group);
            }
            assert_eq!(
                unsafe { &*argument }.ref_count.load(Ordering::Acquire),
                1,
                "cancellation must release the argument reference claimed by the direct task frame"
            );
            Ok(Value::Unit)
        })
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        super::direct_task_claim_flag_live_count(),
        claim_flag_baseline,
        "task cancellation must free its externally owned claim flag"
    );
    unsafe {
        release_value(argument);
    }
}

#[test]
fn native_runtime_direct_owned_ledger_balances_normal_and_buffer_transfers() {
    let external = string_value("external owner");
    let external_address = external as usize;

    let result = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            let external = external_address as *mut OpaqueValue;
            let tracked = unsafe { retain_value(external) };
            let borrowed_buffer = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store(borrowed_buffer, 0, tracked as i64);
            unsafe {
                release_value(tracked);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "the raw buffer reference must not be mistaken for a task-frame owner"
                );
            });
            assert_eq!(
                unsafe { &*external }.ref_count.load(Ordering::Acquire),
                2,
                "the external and raw-buffer references must both remain live"
            );
            super::aura_direct_arg_buffer_store(borrowed_buffer, 0, 0);
            unsafe {
                free_arg_buffer(borrowed_buffer, 1);
            }
            assert_eq!(
                unsafe { &*external }.ref_count.load(Ordering::Acquire),
                1,
                "releasing the raw-buffer reference must not release the external owner"
            );

            let transferred = string_value("transferred to an owned buffer");
            let owned_buffer = super::aura_direct_arg_buffer_new(1);
            super::aura_direct_arg_buffer_store_owned(owned_buffer, 0, transferred as i64);
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "an owned buffer transfer must detach its frame-ledger entry"
                );
            });
            super::aura_direct_arg_buffer_store(owned_buffer, 0, 0);
            unsafe {
                free_arg_buffer(owned_buffer, 1);
            }

            let local = string_value("balanced local");
            let retained = unsafe { retain_value(local) };
            unsafe {
                release_value(retained);
                release_value(local);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "normal retain/release pairs must leave the task ledger empty"
                );
            });
            Ok(Value::Unit)
        })
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    unsafe {
        release_value(external);
    }
}

#[test]
fn native_runtime_releases_cleanup_arguments_when_cleanup_traps() {
    unsafe extern "C-unwind" fn successful_cleanup(
        _args: *const i64,
        _arg_count: usize,
    ) -> *mut OpaqueValue {
        boxed_value(Value::Unit)
    }

    unsafe extern "C-unwind" fn failing_cleanup(
        _args: *const i64,
        _arg_count: usize,
    ) -> *mut OpaqueValue {
        super::runtime_error("cleanup failed")
    }

    let outer_retained = string_value("outer retained cleanup argument");
    let failing_retained = string_value("failing retained cleanup argument");
    let outer_retained_address = outer_retained as usize;
    let failing_retained_address = failing_retained as usize;
    let result = run_lightweight_root_task(move || {
        let task = spawn_lightweight_task(move || {
            super::with_direct_task_runtime_scope(|| {
                super::with_task_runtime_error_capture(|| {
                    let outer_retained = outer_retained_address as *mut OpaqueValue;
                    let outer_args = super::aura_direct_arg_buffer_new(1);
                    super::aura_direct_arg_buffer_store(outer_args, 0, outer_retained as i64);
                    super::aura_direct_register_cleanup(
                        successful_cleanup as *const () as usize as i64,
                        outer_args,
                        1,
                    );

                    let failing_retained = failing_retained_address as *mut OpaqueValue;
                    let failing_args = super::aura_direct_arg_buffer_new(1);
                    super::aura_direct_arg_buffer_store(failing_args, 0, failing_retained as i64);
                    super::aura_direct_register_cleanup(
                        failing_cleanup as *const () as usize as i64,
                        failing_args,
                        1,
                    );
                    super::runtime_error("body failed")
                })
            })
        })?;

        match task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .map_err(|error| Diagnostic::new(error.to_string()))?
        {
            TaskWaitStatus::Ready(Err(error)) if error.message == "body failed" => Ok(Value::Unit),
            other => Err(Diagnostic::new(format!(
                "expected primary body failure, got {other:?}"
            ))),
        }
    });

    assert_eq!(result.expect("root task should complete"), Value::Unit);
    assert_eq!(
        unsafe { &*outer_retained }.ref_count.load(Ordering::SeqCst),
        1,
        "forced exit must release an outer cleanup snapshot left after a cleanup trap"
    );
    assert_eq!(
        unsafe { &*failing_retained }
            .ref_count
            .load(Ordering::SeqCst),
        1,
        "forced exit must release the trapping cleanup's retained snapshot"
    );
    unsafe {
        release_value(outer_retained);
        release_value(failing_retained);
    }
}

#[test]
fn native_runtime_max_depth_skips_saturated_cleanup_and_releases_its_snapshot() {
    unsafe extern "C-unwind" fn mark_cleanup_invoked(
        args: *const i64,
        arg_count: usize,
    ) -> *mut OpaqueValue {
        assert_eq!(arg_count, 1);
        let value = unsafe { *args as *mut OpaqueValue };
        unsafe {
            value_mut(value, |value| match value {
                Value::String(text) => *text = "cleanup invoked".to_string(),
                other => panic!("expected str cleanup witness, found {other:?}"),
            });
        }
        boxed_value(Value::Unit)
    }

    let witness = string_value("cleanup skipped");
    let witness_address = witness as usize;
    let diagnostic = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                for _ in 0..super::DIRECT_MAX_CALL_DEPTH {
                    unsafe {
                        super::aura_direct_enter_call(1, 1, b"recurse".as_ptr(), b"recurse".len());
                    }
                }
                let cleanup_args = super::aura_direct_arg_buffer_new(1);
                super::aura_direct_arg_buffer_store(
                    cleanup_args,
                    0,
                    witness_address as *mut OpaqueValue as i64,
                );
                super::aura_direct_register_cleanup(
                    mark_cleanup_invoked as *const () as usize as i64,
                    cleanup_args,
                    1,
                );
                unsafe {
                    super::aura_direct_enter_call(2, 1, b"rejected".as_ptr(), b"rejected".len());
                }
                #[allow(unreachable_code)]
                Value::Unit
            }))
        })
    })
    .expect_err("the saturated call chain should fail at the task boundary");
    assert_eq!(
        diagnostic.message,
        "maximum call depth of 256 exceeded while calling `rejected`"
    );
    assert_eq!(
        unsafe { value_ref(witness) },
        Value::String("cleanup skipped".to_string()),
        "a cleanup registered at the saturated depth must not enter its callback"
    );
    assert_eq!(
        unsafe { &*witness }.ref_count.load(Ordering::SeqCst),
        1,
        "skipping the callback must still release its retained argument snapshot"
    );
    unsafe {
        release_value(witness);
    }
}

#[test]
fn native_runtime_retain_and_release_keep_values_alive_until_last_handle() {
    let boxed = string_value("aura");
    let retained = unsafe { retain_value(boxed) };

    unsafe { release_value(boxed) };
    assert_eq!(
        unsafe { value_ref(retained) },
        Value::String("aura".to_string())
    );

    unsafe { release_value(retained) };
}

#[test]
fn native_runtime_arg_buffer_store_retains_opaque_values() {
    let buffer = super::aura_direct_arg_buffer_new(1);
    let value = string_value("buffered");
    super::aura_direct_arg_buffer_store(buffer, 0, value as i64);

    unsafe {
        release_value(value);
        let stored = *buffer as *mut OpaqueValue;
        assert_eq!(value_ref(stored), Value::String("buffered".to_string()));
        release_value(stored);
        free_arg_buffer(buffer, 1);
    }
}

#[test]
fn native_runtime_owned_arg_buffer_replacement_releases_previous_value_and_validates_index() {
    let result = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            let previous = string_value("previous");
            let replacement = string_value("replacement");
            let replacement_alias = unsafe { retain_value(replacement) };
            let buffer = super::aura_direct_arg_buffer_new(1);

            super::aura_direct_arg_buffer_store(buffer, 0, previous as i64);
            assert_eq!(
                unsafe { &*previous }.ref_count.load(Ordering::SeqCst),
                2,
                "the ordinary store must retain its input for the buffer"
            );
            super::aura_direct_arg_buffer_store_owned(buffer, 0, replacement as i64);
            assert_eq!(
                unsafe { &*previous }.ref_count.load(Ordering::SeqCst),
                1,
                "an owned replacement must release the prior buffered reference"
            );
            assert_eq!(
                unsafe { &*replacement }.ref_count.load(Ordering::SeqCst),
                2,
                "the replacement buffer and retained alias must own one reference each"
            );

            super::aura_direct_arg_buffer_store_owned(buffer, 0, 0);
            assert_eq!(
                unsafe { &*replacement }.ref_count.load(Ordering::SeqCst),
                1,
                "clearing an owned slot must release the transferred replacement"
            );
            unsafe {
                free_arg_buffer(buffer, 1);
                release_value(replacement_alias);
                release_value(previous);
            }
            super::with_direct_task_runtime_state(|state| {
                assert!(
                    state.owned_value_refs.is_empty(),
                    "owned replacement and clearing must leave no stale ledger references"
                );
            });
            Ok(Value::Unit)
        })
    });
    assert_eq!(
        result.expect("owned buffer replacement should complete"),
        Value::Unit
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_arg_buffer_store_owned(
                std::ptr::null_mut(),
                -1,
                string_value("still caller owned") as i64,
            );
        }),
        "invalid owned arg index"
    );
}

#[test]
fn native_runtime_task_arg_buffer_guard_releases_pre_handoff_values() {
    let value = string_value("captured before a trapping default");
    super::with_direct_task_runtime_scope(|| {
        let buffer = super::aura_direct_arg_buffer_new(2);
        super::aura_direct_arg_buffer_store(buffer, 0, value as i64);
        let guard = super::aura_direct_task_arg_buffer_guard(buffer, 2);
        assert!(guard > 0);
        assert_eq!(
            unsafe { &*value }.ref_count.load(Ordering::SeqCst),
            2,
            "the guarded buffer owns the supplied opaque argument"
        );

        // This is the same drain path used when a later selected default traps.
        super::drain_direct_cleanup_stack();
        assert_eq!(
            unsafe { &*value }.ref_count.load(Ordering::SeqCst),
            1,
            "pre-handoff cleanup must release the retained argument and buffer"
        );
        super::with_direct_task_runtime_state(|state| {
            assert!(state.cleanup_stack.is_empty());
        });
    });
    unsafe {
        release_value(value);
    }
}

#[test]
fn native_runtime_task_arg_buffer_disarm_transfers_without_releasing() {
    let value = string_value("captured for scheduler handoff");
    super::with_direct_task_runtime_scope(|| {
        let buffer = super::aura_direct_arg_buffer_new(1);
        super::aura_direct_arg_buffer_store(buffer, 0, value as i64);
        let guard = super::aura_direct_task_arg_buffer_guard(buffer, 1);
        super::aura_direct_task_arg_buffer_disarm(guard);
        assert_eq!(
            unsafe { &*value }.ref_count.load(Ordering::SeqCst),
            2,
            "disarming transfers the raw buffer without releasing its argument"
        );
        super::with_direct_task_runtime_state(|state| {
            assert!(state.cleanup_stack.is_empty());
        });
        unsafe {
            let stored = *buffer as *mut OpaqueValue;
            release_value(stored);
            free_arg_buffer(buffer, 1);
        }
    });
    unsafe {
        release_value(value);
    }
}

#[test]
fn native_runtime_task_arg_buffer_guard_reports_invalid_and_mismatched_ids() {
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_task_arg_buffer_guard(std::ptr::null_mut(), -1);
        }),
        "invalid guarded task arg buffer size"
    );
    assert_eq!(
        capture_direct_boundary_error_message(|| {
            super::aura_direct_task_arg_buffer_disarm(i64::MAX);
        }),
        "unknown guarded task arg buffer"
    );

    assert_eq!(
        capture_direct_boundary_error_message(|| {
            let ordinary_cleanup =
                super::push_direct_cleanup_registration(1, std::ptr::null_mut(), 0);
            super::aura_direct_task_arg_buffer_disarm(ordinary_cleanup);
        }),
        "task arg buffer guard id referred to an ordinary cleanup"
    );
}

#[test]
fn native_runtime_boxing_range_and_condition_helpers_cover_remaining_valid_paths() {
    assert_eq!(expect_float(super::aura_direct_box_f64(2.5)), 2.5);
    assert!(!expect_bool_boxed(super::aura_direct_box_bool(0)));
    expect_unit(super::aura_direct_box_unit());
    assert_eq!(
        expect_int(super::aura_direct_box_uint_literal(b"42".as_ptr(), 2)),
        42
    );
    assert_eq!(
        expect_string(super::aura_direct_string_literal(b"aura".as_ptr(), 4)),
        "aura"
    );
    assert_eq!(
        expect_string(super::aura_direct_stringify_value(super::boxed_value(
            Value::Range(RangeValue { start: 2, end: 4 },)
        ))),
        "range(2, 4)"
    );

    let range = super::aura_direct_range_new(2, 5);
    assert_eq!(super::aura_direct_range_current(range), 2);
    assert_eq!(super::aura_direct_range_end(range), 5);
    match unsafe { take_value(super::aura_direct_range_advance(range)) } {
        Value::Range(advanced) => {
            assert_eq!(advanced.start, 3);
            assert_eq!(advanced.end, 5);
        }
        other => panic!("expected advanced range, found {:?}", other),
    }

    assert_eq!(super::aura_direct_unbox_i64(int_value(9)), 9);
    assert_eq!(super::aura_direct_unbox_f64(float_value(1.5)), 1.5);
    assert_eq!(super::aura_direct_unbox_bool(bool_value(true)), 1);
    assert_eq!(super::aura_direct_value_as_condition(bool_value(false)), 0);
    assert_eq!(super::aura_direct_value_as_condition(int_value(0)), 0);
    assert_eq!(super::aura_direct_value_as_condition(int_value(3)), 1);
    assert_eq!(
        super::aura_direct_value_as_condition(super::aura_direct_box_unit()),
        0
    );

    let vec = super::aura_direct_vec_empty();
    assert_eq!(super::aura_direct_vec_is_empty(vec), 1);
    expect_unit(super::aura_direct_vec_push_in_place(vec, int_value(1)));
    assert_eq!(super::aura_direct_vec_len(vec), 1);
    assert_eq!(super::aura_direct_vec_is_empty(vec), 0);

    let map = super::aura_direct_map_empty();
    assert_eq!(super::aura_direct_map_is_empty(map), 1);
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("answer"),
        int_value(42),
    ));
    assert_eq!(super::aura_direct_map_is_empty(map), 0);

    let set = super::aura_direct_set_empty();
    assert_eq!(super::aura_direct_set_is_empty(set), 1);
    assert_eq!(
        super::aura_direct_set_insert_in_place(set, string_value("ready")),
        1
    );
    assert_eq!(super::aura_direct_set_is_empty(set), 0);
    expect_option_none(super::aura_direct_set_index_option(set, 5));
    expect_option_none(super::aura_direct_set_index_option(set, i64::MAX));
    expect_option_none(super::aura_direct_set_take_index_in_place(set, i64::MAX));
    assert_eq!(
        super::aura_direct_set_len(set),
        1,
        "the maximum Aura int64 position must be an ordinary missing index"
    );
}

#[test]
fn native_runtime_collection_helpers_cover_remaining_success_paths() {
    let vec = int_vec(&[1, 2, 3]);
    assert_eq!(
        expect_option_some_int(super::aura_direct_vec_index_option(vec, 1)),
        2
    );
    assert_eq!(expect_int(super::aura_direct_vec_index(vec, 2, 0, 0)), 3);
    expect_unit(super::aura_direct_vec_set_index_in_place(
        vec,
        0,
        int_value(9),
        0,
        0,
    ));
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(vec)),
        vec![9, 2, 3]
    );

    let map = super::aura_direct_map_empty();
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("a"),
        int_value(1),
    ));
    expect_option_none(super::aura_direct_map_set_in_place(
        map,
        string_value("b"),
        int_value(2),
    ));
    assert_eq!(
        expect_option_some_int(super::aura_direct_map_get(map, string_value("a"))),
        1
    );
    assert_eq!(
        super::aura_direct_map_contains_key(map, string_value("b")),
        1
    );
    assert_eq!(
        expect_vec_strings(super::aura_direct_map_keys(map)),
        vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_map_values(map)),
        vec![1, 2]
    );
    let entries = unsafe { take_value(super::aura_direct_map_items(map)) };
    match entries {
        Value::Vec(entries) => {
            assert_eq!(entries.elements.len(), 2);
            assert!(matches!(&entries.elements[0], Value::Tuple(_)));
        }
        other => panic!("expected map entries vec, found {:?}", other),
    }
    assert_eq!(
        expect_int(super::aura_direct_map_index(map, string_value("b"), 0, 0)),
        2
    );
    expect_unit(super::aura_direct_map_set_index_in_place(
        map,
        string_value("b"),
        int_value(7),
        0,
        0,
    ));
    assert_eq!(
        expect_option_some_int(super::aura_direct_map_remove_in_place(
            map,
            string_value("a"),
        )),
        1
    );
    expect_unit(super::aura_direct_map_clear_in_place(map));
    assert_eq!(super::aura_direct_map_is_empty(map), 1);

    let set = super::aura_direct_set_empty();
    assert_eq!(
        super::aura_direct_set_contains(set, string_value("ready")),
        0
    );
    assert_eq!(
        super::aura_direct_set_insert_in_place(set, string_value("ready")),
        1
    );
    assert_eq!(
        super::aura_direct_set_contains(set, string_value("ready")),
        1
    );
    assert_eq!(
        super::aura_direct_set_remove_in_place(set, string_value("ready")),
        1
    );
    assert_eq!(
        super::aura_direct_set_contains(set, string_value("ready")),
        0
    );

    let other = super::aura_direct_map_empty();
    expect_option_none(super::aura_direct_map_set_in_place(
        other,
        string_value("b"),
        int_value(9),
    ));
    expect_option_none(super::aura_direct_map_set_in_place(
        other,
        string_value("c"),
        int_value(3),
    ));
    expect_unit(super::aura_direct_map_extend_in_place(map, other));
    assert_eq!(
        expect_vec_strings(super::aura_direct_map_keys(map)),
        vec!["b".to_string(), "c".to_string()]
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_map_values(map)),
        vec![9, 3]
    );
}

#[test]
fn duration_integer_outside_signed_range_helper_exits_with_error() {
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() == Ok("duration-int-out-of-range") {
        extract_duration_nanoseconds(&Value::Int(IntegerValue::from_literal(u128::MAX)));
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg("native_runtime::tests::duration_integer_outside_signed_range_helper_exits_with_error")
        .arg("--nocapture")
        .env("AURA_DIRECT_RUNTIME_HELPER", "duration-int-out-of-range")
        .output()
        .expect("child test process should run");

    assert!(
        !output.status.success(),
        "duration helper should exit with failure for out-of-range integers"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("expected `Duration`, found an integer outside signed timer range"),
        "duration helper stderr should mention out-of-range integer values"
    );
}

#[test]
fn duration_type_mismatch_helper_exits_with_error() {
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() == Ok("duration-type") {
        extract_duration_nanoseconds(&Value::String("oops".to_string()));
    }

    let output = Command::new(std::env::current_exe().expect("test binary should exist"))
        .arg("--exact")
        .arg("native_runtime::tests::duration_type_mismatch_helper_exits_with_error")
        .arg("--nocapture")
        .env("AURA_DIRECT_RUNTIME_HELPER", "duration-type")
        .output()
        .expect("child test process should run");

    assert!(
        !output.status.success(),
        "duration helper should exit with failure for wrong value types"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected `Duration`, found `str`"),
        "duration helper stderr should mention the wrong runtime type"
    );
}

#[test]
fn native_runtime_operator_and_io_helpers_cover_additional_paths() {
    assert_eq!(render_bool(0), "false");
    assert_eq!(render_bool(7), "true");
    assert_eq!(
        int32_overflow_message(9),
        "integer value `9` does not fit in `int32`"
    );
    assert_eq!(render_float(4.0), "4.0");

    assert_eq!(
        compare_values(
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            BinaryOp::Less
        )
        .expect("string ordering should work"),
        Value::Bool(true)
    );
    let string_compare_error = compare_values(
        Value::String("a".to_string()),
        Value::String("b".to_string()),
        BinaryOp::Add,
    )
    .expect_err("unsupported string comparison operators should fail");
    assert!(string_compare_error
        .message
        .contains("unsupported comparison operator"));

    let int_compare_error = compare_values(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Int(IntegerValue::from_signed(2)),
        BinaryOp::Add,
    )
    .expect_err("unsupported int comparison operators should fail");
    assert!(int_compare_error
        .message
        .contains("unsupported comparison operator"));

    let float_compare_error = compare_values(Value::Float(1.0), Value::Float(2.0), BinaryOp::Add)
        .expect_err("unsupported float comparison operators should fail");
    assert!(float_compare_error
        .message
        .contains("unsupported comparison operator"));

    let mismatch_compare_error = compare_values(
        Value::Bool(true),
        Value::String("no".to_string()),
        BinaryOp::Less,
    )
    .expect_err("mismatched comparisons should fail");
    assert!(mismatch_compare_error
        .message
        .contains("unsupported comparison between"));

    assert_eq!(
        eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::And)
            .expect("bool and should work"),
        Value::Bool(false)
    );
    let and_error = eval_binary_value(
        Value::Int(IntegerValue::from_signed(1)),
        Value::Bool(false),
        BinaryOp::And,
    )
    .expect_err("logical and should require bools");
    assert!(and_error
        .message
        .contains("logical `and` expects bool operands"));

    let or_error = eval_binary_value(
        Value::Bool(true),
        Value::String("x".to_string()),
        BinaryOp::Or,
    )
    .expect_err("logical or should require bools");
    assert!(or_error
        .message
        .contains("logical `or` expects bool operands"));

    let add_error = eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::Add)
        .expect_err("add should reject non-addable types");
    assert!(add_error.message.contains("unsupported `+` operands"));

    let sub_error = eval_binary_value(
        Value::String("a".to_string()),
        Value::String("b".to_string()),
        BinaryOp::Sub,
    )
    .expect_err("sub should reject strings");
    assert!(sub_error.message.contains("unsupported `-` operands"));

    let mul_error = eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::Mul)
        .expect_err("mul should reject bools");
    assert!(mul_error.message.contains("unsupported `*` operands"));

    let div_error = eval_binary_value(Value::Bool(true), Value::Bool(false), BinaryOp::Div)
        .expect_err("div should reject bools");
    assert!(div_error.message.contains("unsupported `/` operands"));

    let mod_error = eval_binary_value(
        Value::String("a".to_string()),
        Value::String("b".to_string()),
        BinaryOp::Mod,
    )
    .expect_err("mod should reject strings");
    assert!(mod_error.message.contains("unsupported `%` operands"));

    let not_error = eval_unary_value(Value::Int(IntegerValue::from_signed(1)), UnaryOp::Not)
        .expect_err("not should reject non-bools");
    assert!(not_error.message.contains("`not` expects `bool`"));

    let neg_error = eval_unary_value(Value::Bool(true), UnaryOp::Neg)
        .expect_err("neg should reject non-numeric values");
    assert!(neg_error
        .message
        .contains("unary `-` expects a numeric value"));

    assert_eq!(super::aura_direct_value_as_condition(bool_value(true)), 1);
    assert_eq!(super::aura_direct_value_as_condition(int_value(0)), 0);
    assert_eq!(
        super::aura_direct_value_as_condition(boxed_value(Value::Unit)),
        0
    );

    assert_eq!(
        expect_int(super::aura_direct_unary_value(0, int_value(9))),
        -9
    );
    assert_eq!(
        expect_bool_boxed(super::aura_direct_unary_value_at(
            1,
            bool_value(false),
            3,
            4
        )),
        true
    );

    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            0,
            int_value(2),
            int_value(3)
        )),
        5
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            1,
            int_value(7),
            int_value(4)
        )),
        3
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            2,
            int_value(6),
            int_value(5)
        )),
        30
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            3,
            int_value(9),
            int_value(2)
        )),
        4
    );
    assert_eq!(
        expect_int(super::aura_direct_binary_value(
            4,
            int_value(9),
            int_value(2)
        )),
        1
    );
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        5,
        int_value(4),
        int_value(4)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        6,
        int_value(4),
        int_value(5)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        7,
        int_value(4),
        int_value(5)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        8,
        int_value(5),
        int_value(5)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        9,
        int_value(6),
        int_value(5)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        10,
        int_value(6),
        int_value(6)
    )));
    assert!(!expect_bool_boxed(super::aura_direct_binary_value(
        11,
        bool_value(true),
        bool_value(false)
    )));
    assert!(expect_bool_boxed(super::aura_direct_binary_value(
        12,
        bool_value(false),
        bool_value(true)
    )));
    assert_eq!(
        expect_int(super::aura_direct_binary_value_at(
            0,
            int_value(10),
            int_value(1),
            0,
            5,
            6,
        )),
        11
    );

    let target = "float64";
    assert_eq!(
        expect_float(super::aura_direct_cast_value(
            int_value(7),
            target.as_ptr(),
            target.len(),
        )),
        7.0
    );
    let target = "int32";
    assert_eq!(
        expect_int(super::aura_direct_cast_value_at(
            int_value(7),
            target.as_ptr(),
            target.len(),
            7,
            8,
        )),
        7
    );
}

#[test]
fn native_assert_failure_preserves_default_custom_empty_whitespace_and_span() {
    let assert_diagnostic = |message: i64, line, column| {
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                super::aura_direct_assert_fail(message, line, column);
            })
        })
        .expect_err("assert failure should fail the active lightweight task")
    };

    let default = assert_diagnostic(0, 4, 7);
    assert_eq!(default.code, "AU4001");
    assert_eq!(default.message, "assertion failed");
    assert_eq!(default.span, Some(Span::new(4, 7)));

    for expected in ["custom", "", " \t "] {
        let message = string_value(expected);
        let diagnostic = assert_diagnostic(message as i64, 9, 3);
        assert_eq!(diagnostic.code, "AU4001");
        assert_eq!(diagnostic.message, expected);
        assert_eq!(diagnostic.span, Some(Span::new(9, 3)));
        unsafe {
            release_value(message);
        }
    }
}

#[test]
fn native_detailed_assert_failure_attaches_typed_bounded_operands_without_consuming_inputs() {
    let message = string_value("values differ");
    let left_label = string_value("left");
    let left_type = string_value("int64");
    let left_value = string_value("41");
    let right_label = string_value("right");
    let right_type = string_value("str");
    let long_right = "é".repeat(3_000);
    let right_value = string_value(&long_right);
    let addresses = [
        message,
        left_label,
        left_type,
        left_value,
        right_label,
        right_type,
        right_value,
    ]
    .map(|value| value as usize);

    let diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_assert_fail_detailed(
                addresses[0] as i64,
                14,
                6,
                addresses[1] as i64,
                addresses[2] as i64,
                addresses[3] as i64,
                addresses[4] as i64,
                addresses[5] as i64,
                addresses[6] as i64,
            );
        })
    })
    .expect_err("a detailed assertion failure should fail the active task");

    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "values differ");
    assert_eq!(diagnostic.span, Some(Span::new(14, 6)));
    assert_eq!(diagnostic.assertion_operands.len(), 2);
    assert_eq!(diagnostic.assertion_operands[0].label, "left");
    assert_eq!(diagnostic.assertion_operands[0].r#type, "int64");
    assert_eq!(diagnostic.assertion_operands[0].value, "41");
    assert!(!diagnostic.assertion_operands[0].truncated);
    assert_eq!(diagnostic.assertion_operands[1].label, "right");
    assert_eq!(diagnostic.assertion_operands[1].r#type, "str");
    assert!(diagnostic.assertion_operands[1].truncated);
    assert!(diagnostic.assertion_operands[1]
        .value
        .ends_with("... (truncated)"));
    assert!(diagnostic.assertion_operands[1].value.len() <= 4_096);

    for address in addresses {
        let value = address as *mut OpaqueValue;
        assert_eq!(
            unsafe { &*value }.ref_count.load(Ordering::Acquire),
            1,
            "the detailed assertion helper must borrow every ABI string"
        );
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn native_detailed_assert_failure_rejects_malformed_capture_strings() {
    let left_label = string_value("left");
    let malformed_type = int_value(17);
    let left_value = string_value("41");
    let right_label = string_value("right");
    let right_type = string_value("int64");
    let right_value = string_value("42");
    let addresses = [
        left_label,
        malformed_type,
        left_value,
        right_label,
        right_type,
        right_value,
    ]
    .map(|value| value as usize);

    let diagnostic = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_assert_fail_detailed(
                0,
                4,
                2,
                addresses[0] as i64,
                addresses[1] as i64,
                addresses[2] as i64,
                addresses[3] as i64,
                addresses[4] as i64,
                addresses[5] as i64,
            );
        })
    })
    .expect_err("malformed private assertion captures must fail deterministically");

    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "direct assertion left type must be `str`, found `integer`"
    );
    assert_eq!(diagnostic.span, None);

    for address in addresses {
        let value = address as *mut OpaqueValue;
        assert_eq!(unsafe { &*value }.ref_count.load(Ordering::Acquire), 1);
        unsafe {
            release_value(value);
        }
    }
}

#[test]
fn native_assert_failure_rejects_non_string_messages_without_consuming_them() {
    let message = int_value(17);
    let message_address = message as usize;
    let diagnostic = run_lightweight_root_task(move || {
        let message = message_address as *mut super::OpaqueValue;
        super::with_task_runtime_error_capture(|| {
            super::aura_direct_assert_fail(message as i64, 6, 4);
        })
    })
    .expect_err("an invalid assertion message should fail the active lightweight task");

    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "direct assertion message must be `str`, found `integer`"
    );
    assert_eq!(diagnostic.span, None);
    assert_eq!(
        unsafe { &*message }.ref_count.load(Ordering::Acquire),
        1,
        "the exported assertion helper must borrow rather than consume its message argument"
    );
    unsafe {
        release_value(message);
    }
}

#[test]
fn native_assert_failure_omits_spans_for_absent_or_invalid_coordinates() {
    let assert_diagnostic = |message: i64, line, column| {
        run_lightweight_root_task(move || {
            super::with_task_runtime_error_capture(|| {
                super::aura_direct_assert_fail(message, line, column);
            })
        })
        .expect_err("assert failure should fail the active lightweight task")
    };

    let default = assert_diagnostic(0, 0, 8);
    assert_eq!(default.code, "AU4001");
    assert_eq!(default.message, "assertion failed");
    assert_eq!(default.span, None);

    let message = string_value("coordinate-free");
    let custom = assert_diagnostic(message as i64, 9, -1);
    assert_eq!(custom.code, "AU4001");
    assert_eq!(custom.message, "coordinate-free");
    assert_eq!(custom.span, None);
    assert_eq!(
        unsafe { &*message }.ref_count.load(Ordering::Acquire),
        1,
        "building an unspanned diagnostic must not consume the borrowed custom message"
    );
    unsafe {
        release_value(message);
    }
}

#[test]
fn native_assert_failure_remains_primary_when_cleanup_traps() {
    unsafe extern "C-unwind" fn failing_cleanup(
        _args: *const i64,
        _arg_count: usize,
    ) -> *mut OpaqueValue {
        unsafe {
            super::aura_direct_enter_call_with_frame(
                20,
                1,
                b"/workspace/cleanup.au".as_ptr(),
                b"/workspace/cleanup.au".len(),
                b"Resource.close".as_ptr(),
                b"Resource.close".len(),
            );
        }
        super::runtime_error("cleanup failed")
    }

    let diagnostic = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                unsafe {
                    super::aura_direct_enter_call_with_frame(
                        1,
                        1,
                        b"/workspace/main.au".as_ptr(),
                        b"/workspace/main.au".len(),
                        b"main".as_ptr(),
                        b"main".len(),
                    );
                }
                let args = super::aura_direct_arg_buffer_new(0);
                super::aura_direct_register_cleanup(
                    failing_cleanup as *const () as usize as i64,
                    args,
                    0,
                );
                super::aura_direct_assert_fail(0, 12, 5);
            }))
        })
    })
    .expect_err("assert failure should fail the active lightweight task");
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "assertion failed");
    assert_eq!(diagnostic.span, Some(Span::new(12, 5)));
    assert_eq!(
        diagnostic.call_frames,
        vec![RuntimeCallFrame {
            function: "main".to_string(),
            span: runtime_source_span("/workspace/main.au", 1, 1),
        }],
        "cleanup frames must not replace the body-primary snapshot"
    );
}

#[test]
fn native_cleanup_primary_trap_captures_the_cleanup_call_chain() {
    unsafe extern "C-unwind" fn failing_cleanup(
        _args: *const i64,
        _arg_count: usize,
    ) -> *mut OpaqueValue {
        unsafe {
            super::aura_direct_enter_call_with_frame(
                20,
                1,
                b"/workspace/resource.au".as_ptr(),
                b"/workspace/resource.au".len(),
                b"Resource.close".as_ptr(),
                b"Resource.close".len(),
            );
        }
        super::runtime_error_at(Span::new(21, 9), "cleanup primary")
    }

    let diagnostic = run_lightweight_root_task(|| {
        super::with_direct_task_runtime_scope(|| {
            Ok(super::with_task_runtime_error_capture(|| {
                unsafe {
                    super::aura_direct_enter_call_with_frame(
                        1,
                        1,
                        b"/workspace/main.au".as_ptr(),
                        b"/workspace/main.au".len(),
                        b"main".as_ptr(),
                        b"main".len(),
                    );
                }
                let args = super::aura_direct_arg_buffer_new(0);
                super::aura_direct_register_cleanup(
                    failing_cleanup as *const () as usize as i64,
                    args,
                    0,
                );
                super::drain_direct_cleanup_stack();
                Value::Unit
            }))
        })
    })
    .expect_err("a cleanup-primary trap should fail the active lightweight task");
    assert_eq!(diagnostic.message, "cleanup primary");
    assert_eq!(diagnostic.span, Some(Span::new(21, 9)));
    assert_eq!(
        diagnostic.call_frames,
        vec![
            RuntimeCallFrame {
                function: "Resource.close".to_string(),
                span: runtime_source_span("/workspace/resource.au", 20, 1),
            },
            RuntimeCallFrame {
                function: "main".to_string(),
                span: runtime_source_span("/workspace/main.au", 1, 1),
            },
        ],
        "when cleanup is the primary trap its own active frame must be the innermost frame"
    );
}

#[test]
fn native_runtime_discarded_frame_state_is_absent_when_the_same_task_key_is_reused() {
    let inherited = RuntimeTaskFrame {
        task_function: "old_child".to_string(),
        task_entry_span: runtime_source_span("/workspace/old_child.au", 3, 1),
        parent_function: "old_parent".to_string(),
        spawn_span: runtime_source_span("/workspace/old_parent.au", 7, 5),
    };

    super::with_direct_task_runtime_scope_with_ancestry(vec![inherited], || {
        unsafe {
            super::aura_direct_enter_call_with_frame(
                3,
                1,
                b"/workspace/old_child.au".as_ptr(),
                b"/workspace/old_child.au".len(),
                b"old_child".as_ptr(),
                b"old_child".len(),
            );
        }
        assert_eq!(super::direct_runtime_call_frames().len(), 1);
        assert_eq!(super::direct_runtime_task_ancestry().len(), 1);

        super::discard_current_direct_task_runtime_state();
        super::with_direct_task_runtime_scope(|| {
            assert!(
                super::direct_runtime_call_frames().is_empty(),
                "a reused task identity must start with no retired call frames"
            );
            assert!(
                super::direct_runtime_task_ancestry().is_empty(),
                "a reused task identity must start with no retired ancestry"
            );
        });
    });
}

#[test]
fn direct_tuple_abi_constructs_projects_matches_and_compares_opaque_values() {
    fn tuple(elements: Vec<*mut OpaqueValue>) -> *mut OpaqueValue {
        let count = elements.len();
        let buffer = super::aura_direct_arg_buffer_new(count as i64);
        for (index, element) in elements.into_iter().enumerate() {
            super::aura_direct_arg_buffer_store_owned(buffer, index as i64, element as i64);
        }
        super::aura_direct_tuple_new(buffer, count as i64)
    }

    let left = tuple(vec![int_value(7), string_value("seven")]);
    let right = tuple(vec![int_value(7), string_value("seven")]);
    assert_eq!(
        super::aura_direct_value_type_matches(left, b"(int64, str)".as_ptr(), "(int64, str)".len(),),
        1,
        "an untagged tuple should infer its structural element types"
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            left,
            b"(?Number, ?Text)".as_ptr(),
            "(?Number, ?Text)".len(),
        ),
        1,
        "an untagged tuple must match wildcard patterns from its structural element metadata"
    );
    let equality = super::aura_direct_binary_value(5, left, right);
    assert!(expect_bool_boxed(equality));

    let non_tuple = bool_value(true);
    assert_eq!(
        super::aura_direct_value_type_matches(
            non_tuple,
            b"(?Element,)".as_ptr(),
            "(?Element,)".len(),
        ),
        0,
        "an untagged non-tuple must not satisfy a tuple type pattern"
    );
    unsafe {
        release_value(non_tuple);
    }

    let projected = super::aura_direct_tuple_element(left, 1);
    unsafe {
        release_value(equality);
        release_value(left);
    }
    assert_eq!(
        expect_string(projected),
        "seven",
        "projection must own an independent clone after the tuple is dropped"
    );
    unsafe {
        release_value(projected);
        release_value(right);
    }

    let owned_text = "destructured without cloning".repeat(8);
    let owned_text_allocation = owned_text.as_ptr();
    let captured = tuple(vec![boxed_value(Value::String(owned_text))]);
    let taken = super::aura_direct_tuple_take_element(captured, 0);
    unsafe {
        super::with_value(taken, |value| match value {
            Value::String(text) => assert_eq!(
                text.as_ptr(),
                owned_text_allocation,
                "destructive extraction must transfer the original allocation"
            ),
            other => panic!("expected destructured str, found {other:?}"),
        });
        super::with_value(captured, |value| match value {
            Value::Tuple(tuple) => assert_eq!(
                tuple.elements,
                vec![Value::Unit],
                "the private captured source slot must be consumed"
            ),
            other => panic!("expected captured tuple, found {other:?}"),
        });
    }
    let captured_address = captured as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_element(captured_address as *mut OpaqueValue, 0);
            Ok(Value::Unit)
        })
    })
    .expect_err("a destructively extracted slot must not remain publicly readable");
    assert_eq!(
        error.message,
        "tuple element at index 0 has already been moved"
    );
    unsafe {
        release_value(captured);
    }
    assert_eq!(
        expect_string(taken),
        "destructured without cloning".repeat(8)
    );
    unsafe {
        release_value(taken);
    }

    let inner = tuple(vec![string_value("nested")]);
    let nested = tuple(vec![int_value(9), inner]);
    let nested_type = "(int64, (str,))";
    super::aura_direct_tag_value_type(nested, nested_type.as_ptr(), nested_type.len());
    assert_eq!(
        super::aura_direct_value_type_matches(
            nested,
            b"(?Number, (?Text,))".as_ptr(),
            "(?Number, (?Text,))".len(),
        ),
        1,
        "runtime tuple type patterns must parse and match recursively"
    );
    assert_eq!(
        super::aura_direct_value_type_matches(
            nested,
            b"(?Same, (?Same,))".as_ptr(),
            "(?Same, (?Same,))".len(),
        ),
        0,
        "repeated type variables must retain their structural substitution"
    );
    unsafe {
        release_value(nested);
    }

    let not_a_tuple = int_value(1);
    let not_a_tuple_address = not_a_tuple as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_element(not_a_tuple_address as *mut OpaqueValue, 0);
            Ok(Value::Unit)
        })
    })
    .expect_err("projecting from a non-tuple should fail the active task");
    assert_eq!(error.message, "expected tuple value, found `integer`");
    unsafe {
        release_value(not_a_tuple);
    }

    let singleton = tuple(vec![int_value(1)]);
    assert_eq!(
        super::aura_direct_value_type_matches(singleton, b"(int64,)".as_ptr(), "(int64,)".len(),),
        1,
        "singleton tuple runtime types must preserve the comma that distinguishes their arity"
    );
    let singleton_address = singleton as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_element(singleton_address as *mut OpaqueValue, 1);
            Ok(Value::Unit)
        })
    })
    .expect_err("out-of-bounds tuple projection should fail the active task");
    assert_eq!(error.message, "tuple of length 1 has no element at index 1");
    let singleton_address = singleton as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_element(singleton_address as *mut OpaqueValue, -1);
            Ok(Value::Unit)
        })
    })
    .expect_err("negative tuple projection should fail the active task");
    assert_eq!(error.message, "invalid tuple element index");
    unsafe {
        release_value(singleton);
    }

    let error = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_new(std::ptr::null_mut(), -1);
            Ok(Value::Unit)
        })
    })
    .expect_err("negative tuple arity should fail the active task");
    assert_eq!(error.message, "invalid tuple element count");

    let error = run_lightweight_root_task(|| {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_new(std::ptr::null_mut(), 1);
            Ok(Value::Unit)
        })
    })
    .expect_err("a missing non-empty tuple buffer should fail the active task");
    assert_eq!(
        error.message,
        "direct runtime received a null tuple element buffer"
    );

    let buffer = super::aura_direct_arg_buffer_new(1);
    super::aura_direct_arg_buffer_store_owned(buffer, 0, 0);
    let buffer_address = buffer as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_new(buffer_address as *mut i64, 1);
            Ok(Value::Unit)
        })
    })
    .expect_err("a null owned tuple element should fail the active task");
    assert_eq!(
        error.message,
        "direct runtime received a null owned tuple element handle"
    );

    let not_a_tuple = int_value(2);
    let not_a_tuple_address = not_a_tuple as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ =
                super::aura_direct_tuple_take_element(not_a_tuple_address as *mut OpaqueValue, 0);
            Ok(Value::Unit)
        })
    })
    .expect_err("destructive extraction from a non-tuple should fail the active task");
    assert_eq!(error.message, "expected tuple value, found `integer`");
    unsafe {
        release_value(not_a_tuple);
    }

    let captured_negative = tuple(vec![int_value(3)]);
    let captured_address = captured_negative as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_take_element(captured_address as *mut OpaqueValue, -1);
            Ok(Value::Unit)
        })
    })
    .expect_err("negative destructive tuple extraction should fail the active task");
    assert_eq!(error.message, "invalid tuple element index");
    unsafe {
        release_value(captured_negative);
    }

    let captured_bounds = tuple(vec![int_value(3)]);
    let captured_address = captured_bounds as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_take_element(captured_address as *mut OpaqueValue, 1);
            Ok(Value::Unit)
        })
    })
    .expect_err("out-of-bounds destructive tuple extraction should fail the active task");
    assert_eq!(error.message, "tuple of length 1 has no element at index 1");
    unsafe {
        release_value(captured_bounds);
    }

    let captured_repeat = tuple(vec![int_value(3)]);
    let first = super::aura_direct_tuple_take_element(captured_repeat, 0);
    unsafe {
        release_value(first);
    }
    let captured_address = captured_repeat as usize;
    let error = run_lightweight_root_task(move || {
        super::with_task_runtime_error_capture(|| {
            let _ = super::aura_direct_tuple_take_element(captured_address as *mut OpaqueValue, 0);
            Ok(Value::Unit)
        })
    })
    .expect_err("a tuple element cannot be destructively extracted twice");
    assert_eq!(
        error.message,
        "tuple element at index 0 has already been moved"
    );
    unsafe {
        release_value(captured_repeat);
    }
}

#[test]
fn canonical_collection_abi_pins_mutation_search_capacity_and_set_discard() {
    let byte = |value: u8| {
        boxed_value(Value::Int(
            IntegerValue::from_typed_unsigned(value as u128, IntegerKind::Uint8)
                .expect("every test byte fits uint8"),
        ))
    };

    let values = int_vec(&[1, 2, 1]);
    assert_eq!(
        expect_int(super::aura_direct_collection_operation(
            values,
            std::ptr::null_mut(),
            -1,
            0,
        )),
        1,
        "pop(-1) removes and returns the last list value"
    );
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(values)),
        vec![1, 2]
    );

    let one = byte(1);
    assert_eq!(
        expect_int(super::aura_direct_collection_operation(values, one, 0, 2)),
        0,
        "index returns the first matching position"
    );
    assert_eq!(
        expect_int(super::aura_direct_collection_operation(values, one, 0, 3)),
        1,
        "count reports the number of equal values"
    );
    expect_unit(super::aura_direct_collection_operation(values, one, 0, 1));
    assert_eq!(
        expect_vec_ints(super::aura_direct_clone_value(values)),
        vec![2],
        "remove deletes the first equal value"
    );
    expect_unit(super::aura_direct_collection_operation(
        values,
        std::ptr::null_mut(),
        16,
        4,
    ));

    let map = super::aura_direct_map_empty();
    expect_unit(super::aura_direct_collection_operation(
        map,
        std::ptr::null_mut(),
        8,
        4,
    ));

    let set = super::aura_direct_set_empty();
    assert_eq!(super::aura_direct_set_insert_in_place(set, byte(3)), 1);
    assert_eq!(super::aura_direct_set_insert_in_place(set, byte(4)), 1);
    expect_unit(super::aura_direct_collection_operation(set, one, 0, 6));
    assert_eq!(
        super::aura_direct_set_len(set),
        2,
        "discard of an absent value is a no-op"
    );
    let three = byte(3);
    expect_unit(super::aura_direct_collection_operation(set, three, 0, 5));
    assert_eq!(super::aura_direct_set_len(set), 1);
    expect_unit(super::aura_direct_collection_operation(
        set,
        std::ptr::null_mut(),
        0,
        7,
    ));
    assert_eq!(
        super::aura_direct_set_len(set),
        0,
        "clear removes every set value"
    );
    expect_unit(super::aura_direct_collection_operation(
        set,
        std::ptr::null_mut(),
        4,
        4,
    ));

    for value in [values, one, map, set, three] {
        unsafe { release_value(value) };
    }
}

#[test]
fn canonical_collection_abi_preserves_absence_bounds_and_capacity_diagnostics() {
    let capture =
        |collection: *mut OpaqueValue, argument: *mut OpaqueValue, scalar: i64, opcode: i64| {
            let collection = collection as usize;
            let argument = argument as usize;
            capture_direct_boundary_diagnostic(move || {
                super::aura_direct_collection_operation(
                    collection as *mut OpaqueValue,
                    argument as *mut OpaqueValue,
                    scalar,
                    opcode,
                );
            })
        };

    let byte_nine = || {
        boxed_value(Value::Int(
            IntegerValue::from_typed_unsigned(9, IntegerKind::Uint8).unwrap(),
        ))
    };

    let pop_values = int_vec(&[1, 2]);
    let out_of_bounds = capture(pop_values, std::ptr::null_mut(), 4, 0);
    assert_eq!(out_of_bounds.code, "AU4003");
    assert_eq!(
        out_of_bounds.message,
        "list pop index `4` is out of bounds for length `2`"
    );

    let remove_values = int_vec(&[1, 2]);
    let remove_needle = byte_nine();
    let remove_missing = capture(remove_values, remove_needle, 0, 1);
    assert_eq!(remove_missing.code, "AU4008");
    assert_eq!(remove_missing.message, "collection value was not found");
    assert_eq!(
        remove_missing.help,
        vec!["check `value in values` before removing when absence is expected".to_string()]
    );

    let index_values = int_vec(&[1, 2]);
    let index_needle = byte_nine();
    let index_missing = capture(index_values, index_needle, 0, 2);
    assert_eq!(index_missing.code, "AU4008");
    assert_eq!(
        index_missing.help,
        vec!["check `value in values` before searching when absence is expected".to_string()]
    );

    let negative_values = int_vec(&[1, 2]);
    let negative_capacity = capture(negative_values, std::ptr::null_mut(), -1, 4);
    assert_eq!(negative_capacity.code, "AU4003");
    assert_eq!(
        negative_capacity.message,
        "collection capacity cannot be negative"
    );

    let allocation_values = int_vec(&[1, 2]);
    let allocation = capture(allocation_values, std::ptr::null_mut(), i64::MAX, 4);
    assert_eq!(allocation.code, "AU4005");
    assert_eq!(allocation.message, "collection capacity allocation failed");

    let set = super::aura_direct_set_empty();
    let set_needle = byte_nine();
    let set_missing = capture(set, set_needle, 0, 5);
    assert_eq!(set_missing.code, "AU4008");
    assert_eq!(set_missing.message, "collection value was not found");

    for value in [
        pop_values,
        remove_values,
        remove_needle,
        index_values,
        index_needle,
        negative_values,
        allocation_values,
        set,
        set_needle,
    ] {
        unsafe { release_value(value) };
    }
}

#[test]
fn canonical_list_pop_trap_releases_the_write_lock_for_later_tasks() {
    let values = int_vec(&[1, 2]);
    let values_address = values as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_collection_operation(
            values_address as *mut OpaqueValue,
            std::ptr::null_mut(),
            9,
            0,
        );
    });
    assert_eq!(diagnostic.code, "AU4003");
    assert_eq!(
        diagnostic.message,
        "list pop index `9` is out of bounds for length `2`"
    );

    let values_address = values as usize;
    let later_use = run_lightweight_root_task(move || {
        super::with_direct_task_runtime_scope(|| {
            super::with_task_runtime_error_capture(|| {
                assert_eq!(
                    super::aura_direct_vec_len(values_address as *mut OpaqueValue),
                    2,
                    "the failed task must leave the shared list readable"
                );
                let popped = super::aura_direct_collection_operation(
                    values_address as *mut OpaqueValue,
                    std::ptr::null_mut(),
                    -1,
                    0,
                );
                Ok(unsafe { super::consume_value(popped) })
            })
        })
    });
    assert_eq!(
        later_use.expect("the list must remain usable after another task traps"),
        Value::Int(
            IntegerValue::from_typed_unsigned(2, IntegerKind::Uint8)
                .expect("the popped value fits uint8")
        )
    );
    assert_eq!(super::aura_direct_vec_len(values), 1);
    unsafe { release_value(values) };
}

#[test]
fn direct_list_index_mutator_traps_release_the_write_lock_for_later_tasks() {
    let later_len = |values: *mut OpaqueValue| {
        let values_address = values as usize;
        run_lightweight_root_task(move || {
            super::with_direct_task_runtime_scope(|| {
                super::with_task_runtime_error_capture(|| {
                    Ok(Value::Int(IntegerValue::from_i64(
                        super::aura_direct_vec_len(values_address as *mut OpaqueValue),
                    )))
                })
            })
        })
        .expect("a rejected list mutation must not poison the list")
    };

    let set_values = int_vec(&[1, 2]);
    let set_values_address = set_values as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_vec_set_in_place(
            set_values_address as *mut OpaqueValue,
            7,
            int_value(9),
        );
    });
    assert_eq!(diagnostic.code, "AU4003");
    assert_eq!(
        diagnostic.message,
        "list set index `7` is out of bounds for length `2`"
    );
    assert_eq!(later_len(set_values), Value::Int(IntegerValue::from_i64(2)));

    let remove_values = int_vec(&[1, 2]);
    let remove_values_address = remove_values as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_vec_remove_in_place(remove_values_address as *mut OpaqueValue, -3);
    });
    assert_eq!(diagnostic.code, "AU4003");
    assert_eq!(
        diagnostic.message,
        "list remove index `-3` is out of bounds for length `2`"
    );
    assert_eq!(
        later_len(remove_values),
        Value::Int(IntegerValue::from_i64(2))
    );

    let swap_values = int_vec(&[1, 2]);
    let swap_values_address = swap_values as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_vec_swap_in_place(swap_values_address as *mut OpaqueValue, 0, 8);
    });
    assert_eq!(diagnostic.code, "AU4003");
    assert_eq!(
        diagnostic.message,
        "list swap indices `0` and `8` are out of bounds for length `2`"
    );
    assert_eq!(
        later_len(swap_values),
        Value::Int(IntegerValue::from_i64(2))
    );

    for values in [set_values, remove_values, swap_values] {
        unsafe { release_value(values) };
    }
}

#[test]
fn direct_collection_receiver_traps_release_value_locks_for_later_tasks() {
    let later_integer = |value: *mut OpaqueValue| {
        let value_address = value as usize;
        run_lightweight_root_task(move || {
            super::with_direct_task_runtime_scope(|| {
                super::with_task_runtime_error_capture(|| {
                    Ok(Value::Int(IntegerValue::from_i64(
                        super::aura_direct_unbox_i64(value_address as *mut OpaqueValue),
                    )))
                })
            })
        })
        .expect("a rejected collection receiver must remain readable")
    };

    let not_a_map = int_value(11);
    let not_a_map_address = not_a_map as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_map_clear_in_place(not_a_map_address as *mut OpaqueValue);
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "expected `dict`, found `integer`");
    assert_eq!(
        later_integer(not_a_map),
        Value::Int(IntegerValue::from_i64(11))
    );

    let not_a_set = int_value(13);
    let not_a_set_address = not_a_set as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_collection_operation(
            not_a_set_address as *mut OpaqueValue,
            std::ptr::null_mut(),
            0,
            7,
        );
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "expected `set`, found `integer`");
    assert_eq!(
        later_integer(not_a_set),
        Value::Int(IntegerValue::from_i64(13))
    );

    let not_a_collection = int_value(17);
    let not_a_collection_address = not_a_collection as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_collection_operation(
            not_a_collection_address as *mut OpaqueValue,
            std::ptr::null_mut(),
            1,
            4,
        );
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "reserve requires a collection");
    assert_eq!(
        later_integer(not_a_collection),
        Value::Int(IntegerValue::from_i64(17))
    );

    for value in [not_a_map, not_a_set, not_a_collection] {
        unsafe { release_value(value) };
    }
}

#[test]
fn direct_fixed_width_and_general_operator_abis_cover_every_new_numeric_opcode() {
    let int8 = |value: i128| {
        boxed_value(Value::Int(
            IntegerValue::from_typed_signed(value, IntegerKind::Int8)
                .expect("test value fits int8"),
        ))
    };
    let width_case = |left: i128, right: i128, operation, mode, expected: i128| {
        let left = int8(left);
        let right = int8(right);
        let result = super::aura_direct_integer_width_binary(left, right, operation, mode, 5, 7);
        assert_eq!(
            unsafe { take_value(result) },
            Value::Int(
                IntegerValue::from_typed_signed(expected, IntegerKind::Int8)
                    .expect("expected value fits int8")
            ),
            "mode {mode}, operation {operation}"
        );
        unsafe {
            release_value(left);
            release_value(right);
        }
    };

    for (left, right, operation, expected) in [
        (127, 1, 0, -128),
        (-128, 1, 1, 127),
        (64, 2, 2, -128),
        (65, 1, 3, -126),
        (-128, 1, 4, -64),
    ] {
        width_case(left, right, operation, 1, expected);
    }
    for (left, right, operation, expected) in [
        (127, 1, 0, 127),
        (-128, 1, 1, -128),
        (64, 2, 2, 127),
        (65, 1, 3, 127),
        (-128, 1, 4, -64),
    ] {
        width_case(left, right, operation, 2, expected);
    }

    for (opcode, left, right, expected) in [
        (13, -7, 3, -3),
        (14, 3, 4, 81),
        (15, 6, 3, 2),
        (16, 6, 3, 7),
        (17, 6, 3, 5),
        (18, 6, 1, 12),
        (19, 6, 1, 3),
    ] {
        assert_eq!(
            expect_int(super::aura_direct_binary_value(
                opcode,
                int_value(left),
                int_value(right),
            )),
            expected,
            "general numeric opcode {opcode}"
        );
        assert_eq!(
            expect_int(super::aura_direct_binary_value_at(
                opcode,
                int_value(left),
                int_value(right),
                0,
                11,
                13,
            )),
            expected,
            "spanned general numeric opcode {opcode}"
        );
    }
    assert_eq!(
        expect_int(super::aura_direct_unary_value(2, int_value(5))),
        !5_i64 as i128
    );
    assert_eq!(
        expect_int(super::aura_direct_unary_value_at(2, int_value(5), 17, 19)),
        !5_i64 as i128
    );
}

#[test]
fn direct_format_abi_returns_exact_text_and_preserves_type_diagnostic_codes() {
    let spec = "+6d";
    let value_type = "int64";
    let value = int_value(42);
    assert_eq!(
        expect_string(super::aura_direct_format_value(
            value,
            spec.as_ptr(),
            spec.len(),
            value_type.as_ptr(),
            value_type.len(),
        )),
        "   +42"
    );
    unsafe { release_value(value) };

    let invalid_spec = "s";
    let invalid_value = int_value(42);
    let invalid_value_address = invalid_value as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_format_value(
            invalid_value_address as *mut OpaqueValue,
            invalid_spec.as_ptr(),
            invalid_spec.len(),
            value_type.as_ptr(),
            value_type.len(),
        );
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "format code `s` requires `str`, found integer"
    );
    unsafe { release_value(invalid_value) };
}

static DIRECT_CONSTANT_INITIALIZER_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C-unwind" fn counted_direct_constant_initializer(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null());
    assert_eq!(len, 0);
    DIRECT_CONSTANT_INITIALIZER_CALLS.fetch_add(1, Ordering::SeqCst);
    int_value(42)
}

#[test]
fn direct_module_constant_abi_initializes_once_caches_and_reinitializes_after_reset() {
    const HELPER: &str = "direct-module-constant-cache";
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() != Ok(HELPER) {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg("native_runtime::tests::direct_module_constant_abi_initializes_once_caches_and_reinitializes_after_reset")
            .arg("--nocapture")
            .env("AURA_DIRECT_RUNTIME_HELPER", HELPER)
            .output()
            .expect("isolated module constant test should run");
        assert!(
            output.status.success(),
            "isolated module constant cache test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    super::clear_direct_module_constants();
    DIRECT_CONSTANT_INITIALIZER_CALLS.store(0, Ordering::SeqCst);
    let key = "tests::answer";
    let thunk = counted_direct_constant_initializer as *const () as usize as i64;

    let first = super::aura_direct_module_constant(key.as_ptr(), key.len(), thunk);
    let second = super::aura_direct_module_constant(key.as_ptr(), key.len(), thunk);
    assert_eq!(expect_int(first), 42);
    assert_eq!(expect_int(second), 42);
    assert_eq!(
        DIRECT_CONSTANT_INITIALIZER_CALLS.load(Ordering::SeqCst),
        1,
        "the cached value must be reused without calling its initializer twice"
    );

    super::clear_direct_module_constants();
    let after_reset = super::aura_direct_module_constant(key.as_ptr(), key.len(), thunk);
    assert_eq!(expect_int(after_reset), 42);
    assert_eq!(
        DIRECT_CONSTANT_INITIALIZER_CALLS.load(Ordering::SeqCst),
        2,
        "runtime reset drops cached constants so the next program initializes anew"
    );
    super::clear_direct_module_constants();
}

#[test]
fn direct_module_constant_abi_rejects_a_null_initializer_exactly() {
    const HELPER: &str = "direct-module-constant-null-initializer";
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() != Ok(HELPER) {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg("native_runtime::tests::direct_module_constant_abi_rejects_a_null_initializer_exactly")
            .arg("--nocapture")
            .env("AURA_DIRECT_RUNTIME_HELPER", HELPER)
            .output()
            .expect("isolated module constant test should run");
        assert!(
            output.status.success(),
            "isolated null initializer test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    super::clear_direct_module_constants();
    let key = "tests::missing_initializer";
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_module_constant(key.as_ptr(), key.len(), 0);
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(
        diagnostic.message,
        "module constant `tests::missing_initializer` has a null initializer thunk"
    );
    super::clear_direct_module_constants();
}

const REENTRANT_DIRECT_CONSTANT_KEY: &str = "tests::reentrant_constant";

unsafe extern "C-unwind" fn reentrant_direct_constant_initializer(
    args: *const i64,
    len: usize,
) -> *mut OpaqueValue {
    assert!(args.is_null());
    assert_eq!(len, 0);
    let thunk = reentrant_direct_constant_initializer as *const () as usize as i64;
    super::aura_direct_module_constant(
        REENTRANT_DIRECT_CONSTANT_KEY.as_ptr(),
        REENTRANT_DIRECT_CONSTANT_KEY.len(),
        thunk,
    )
}

#[test]
fn direct_module_constant_abi_reports_reentrancy_then_remembers_failure() {
    const HELPER: &str = "direct-module-constant-reentrant-failure";
    if std::env::var("AURA_DIRECT_RUNTIME_HELPER").as_deref() != Ok(HELPER) {
        let output = Command::new(std::env::current_exe().expect("test binary should exist"))
            .arg("--exact")
            .arg("native_runtime::tests::direct_module_constant_abi_reports_reentrancy_then_remembers_failure")
            .arg("--nocapture")
            .env("AURA_DIRECT_RUNTIME_HELPER", HELPER)
            .output()
            .expect("isolated reentrant module constant test should run");
        assert!(
            output.status.success(),
            "isolated reentrant module constant test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    super::clear_direct_module_constants();
    let thunk = reentrant_direct_constant_initializer as *const () as usize as i64;
    let first = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_module_constant(
            REENTRANT_DIRECT_CONSTANT_KEY.as_ptr(),
            REENTRANT_DIRECT_CONSTANT_KEY.len(),
            thunk,
        );
    });
    assert_eq!(first.code, "AU4001");
    assert_eq!(
        first.message,
        "module constant `tests::reentrant_constant` was read while its module was still initializing"
    );

    let second = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_module_constant(
            REENTRANT_DIRECT_CONSTANT_KEY.as_ptr(),
            REENTRANT_DIRECT_CONSTANT_KEY.len(),
            thunk,
        );
    });
    assert_eq!(second.code, "AU4001");
    assert_eq!(
        second.message,
        "module constant `tests::reentrant_constant` previously failed to initialize"
    );
    super::clear_direct_module_constants();
}

#[test]
fn direct_numeric_abis_preserve_every_integer_width_across_modes_power_round_and_divmod() {
    fn boxed_integer(value: IntegerValue) -> *mut OpaqueValue {
        boxed_value(Value::Int(value))
    }

    fn width_binary(
        left: IntegerValue,
        right: IntegerValue,
        operation: i64,
        mode: i64,
    ) -> IntegerValue {
        let left_ptr = boxed_integer(left);
        let right_ptr = boxed_integer(right);
        let result =
            super::aura_direct_integer_width_binary(left_ptr, right_ptr, operation, mode, 31, 37);
        let actual = match unsafe { value_ref(result) } {
            Value::Int(value) => value,
            other => panic!("expected integer result, found {other:?}"),
        };
        for value in [left_ptr, right_ptr, result] {
            unsafe { release_value(value) };
        }
        actual
    }

    fn typed(kind: IntegerKind, value: i128) -> IntegerValue {
        if kind.is_signed() {
            IntegerValue::from_typed_signed(value, kind).expect("signed test value fits")
        } else {
            IntegerValue::from_typed_unsigned(value as u128, kind)
                .expect("unsigned test value fits")
        }
    }

    for kind in [
        IntegerKind::Int8,
        IntegerKind::Int16,
        IntegerKind::Int32,
        IntegerKind::Int64,
        IntegerKind::Int128,
        IntegerKind::IntSize,
        IntegerKind::Uint8,
        IntegerKind::Uint16,
        IntegerKind::Uint32,
        IntegerKind::Uint64,
        IntegerKind::Uint128,
        IntegerKind::UintSize,
    ] {
        let (maximum, wrapped_minimum) = match kind.bounds() {
            IntegerBounds::Signed { min, max } => (
                IntegerValue::from_typed_signed(max, kind).unwrap(),
                IntegerValue::from_typed_signed(min, kind).unwrap(),
            ),
            IntegerBounds::Unsigned { max } => (
                IntegerValue::from_typed_unsigned(max, kind).unwrap(),
                IntegerValue::from_typed_unsigned(0, kind).unwrap(),
            ),
        };
        let one = typed(kind, 1);
        assert_eq!(
            width_binary(maximum, one, 0, 1),
            wrapped_minimum,
            "{} wrapping_add",
            kind.runtime_type_name()
        );
        assert_eq!(
            width_binary(maximum, one, 0, 2),
            maximum,
            "{} saturating_add",
            kind.runtime_type_name()
        );

        let two = typed(kind, 2);
        let high_shift = typed(kind, i128::from(kind.bit_width() - 1));
        assert_eq!(
            width_binary(two, high_shift, 3, 1),
            typed(kind, 0),
            "{} wrapping_shl",
            kind.runtime_type_name()
        );
        assert_eq!(
            width_binary(two, high_shift, 3, 2),
            maximum,
            "{} saturating_shl",
            kind.runtime_type_name()
        );

        let base = boxed_integer(typed(kind, 2));
        let exponent = boxed_integer(typed(kind, 3));
        let powered = super::aura_direct_binary_value_at(14, base, exponent, 0, 41, 43);
        assert_eq!(
            unsafe { value_ref(powered) },
            Value::Int(typed(kind, 8)),
            "{} power",
            kind.runtime_type_name()
        );

        let rounded_input = boxed_integer(typed(kind, 7));
        let rounded = super::aura_direct_round(rounded_input);
        assert_eq!(
            unsafe { value_ref(rounded) },
            Value::Int(typed(kind, 7)),
            "{} round",
            kind.runtime_type_name()
        );

        let dividend = boxed_integer(typed(kind, 7));
        let divisor = boxed_integer(typed(kind, 3));
        let pair = super::aura_direct_divmod(dividend, divisor);
        assert_eq!(
            unsafe { value_ref(pair) },
            Value::Tuple(TupleValue {
                element_types: vec![
                    Type::named(kind.runtime_type_name()),
                    Type::named(kind.runtime_type_name()),
                ],
                elements: vec![Value::Int(typed(kind, 2)), Value::Int(typed(kind, 1))],
            }),
            "{} divmod",
            kind.runtime_type_name()
        );

        for value in [
            base,
            exponent,
            powered,
            rounded_input,
            rounded,
            dividend,
            divisor,
            pair,
        ] {
            unsafe { release_value(value) };
        }
    }

    let minimum_int128 = boxed_integer(
        IntegerValue::from_typed_signed(i128::MIN, IntegerKind::Int128)
            .expect("int128 minimum is representable"),
    );
    let minimum_address = minimum_int128 as usize;
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_abs(minimum_address as *mut OpaqueValue);
    });
    assert_eq!(diagnostic.code, "AU4002");
    assert_eq!(
        diagnostic.message,
        "`abs(...)` overflowed the signed integer range"
    );
    unsafe { release_value(minimum_int128) };
}

#[test]
fn direct_spanned_binary_abi_pins_remaining_arithmetic_and_equality_opcodes() {
    for (opcode, left, right, expected) in [
        (1, 9, 4, Value::Int(IntegerValue::from_i64(5))),
        (2, 9, 4, Value::Int(IntegerValue::from_i64(36))),
        (4, 9, 4, Value::Int(IntegerValue::from_i64(1))),
        (5, 9, 9, Value::Bool(true)),
    ] {
        let result = super::aura_direct_binary_value_at(
            opcode,
            int_value(left),
            int_value(right),
            0,
            47,
            53,
        );
        assert_eq!(unsafe { value_ref(result) }, expected, "opcode {opcode}");
        unsafe { release_value(result) };
    }
    let quotient =
        super::aura_direct_binary_value_at(3, float_value(9.0), float_value(4.0), 64, 47, 53);
    assert_eq!(unsafe { value_ref(quotient) }, Value::Float(2.25));
    unsafe { release_value(quotient) };

    let diagnostic = capture_direct_boundary_diagnostic(|| {
        super::aura_direct_binary_value_at(3, float_value(1.0), float_value(0.0), 64, 59, 61);
    });
    assert_eq!(diagnostic.code, "AU4004");
    assert_eq!(diagnostic.message, "division by zero");
    assert_eq!(diagnostic.span, Some(Span::new(59, 61)));
}

#[test]
fn direct_capacity_format_and_detailed_assertion_abis_pin_observable_contracts() {
    let list = super::aura_direct_vec_empty();
    let dict = super::aura_direct_map_empty();
    let set = super::aura_direct_set_empty();
    for collection in [list, dict, set] {
        expect_unit(super::aura_direct_collection_operation(
            collection,
            std::ptr::null_mut(),
            32,
            4,
        ));
    }
    assert!(super::with_vector(list, |value| value.elements.capacity()) >= 32);
    assert!(super::with_map(dict, |value| value.entries.capacity()) >= 32);
    assert!(super::with_set(set, |value| value.elements.capacity()) >= 32);

    for (value, value_type, spec, expected) in [
        (float_value(12.345), "float32", ".2f", "12.35"),
        (float_value(0.125), "float64", ".1%", "12.5%"),
        (string_value("Aura"), "str", "^8s", "  Aura  "),
    ] {
        assert_eq!(
            expect_string(super::aura_direct_format_value(
                value,
                spec.as_ptr(),
                spec.len(),
                value_type.as_ptr(),
                value_type.len(),
            )),
            expected
        );
        unsafe { release_value(value) };
    }

    let left_label = string_value("actual");
    let left_type = string_value("int64");
    let left_value = string_value("1");
    let right_label = string_value("expected");
    let right_type = string_value("int64");
    let right_value = string_value("2");
    let addresses = [
        left_label,
        left_type,
        left_value,
        right_label,
        right_type,
        right_value,
    ]
    .map(|value| value as usize);
    let diagnostic = capture_direct_boundary_diagnostic(move || {
        super::aura_direct_assert_fail_detailed(
            0,
            0,
            0,
            addresses[0] as i64,
            addresses[1] as i64,
            addresses[2] as i64,
            addresses[3] as i64,
            addresses[4] as i64,
            addresses[5] as i64,
        );
    });
    assert_eq!(diagnostic.code, "AU4001");
    assert_eq!(diagnostic.message, "assertion failed");
    assert_eq!(diagnostic.span, None);
    assert_eq!(diagnostic.assertion_operands.len(), 2);
    assert_eq!(diagnostic.assertion_operands[0].label, "actual");
    assert_eq!(diagnostic.assertion_operands[1].label, "expected");

    for value in [
        list,
        dict,
        set,
        left_label,
        left_type,
        left_value,
        right_label,
        right_type,
        right_value,
    ] {
        unsafe { release_value(value) };
    }
}
